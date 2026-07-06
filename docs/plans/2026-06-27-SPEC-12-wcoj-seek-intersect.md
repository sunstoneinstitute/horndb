# SPEC-12 Stage 1b — WCOJ SIMD seek + intersect Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Wire `horndb-simd` into the WCOJ trie cursors and leapfrog join so the per-tuple inner loop is vectorized — closing the SPEC-03 NF1 gap toward `per_tuple` ≤2.5 ns/tuple while keeping output bit-identical to the scalar leapfrog.

**Architecture:** Two consumer changes. (1) Replace the scalar lower-bound in the three trie cursors' `seek` (`VecIter`, `CompressedIter`, `PackedColumn`) with `horndb_simd::lower_bound`. (2) Add a pairwise SIMD-intersect fast path inside `LeapfrogJoin::find_match` for the common case of two contiguous sorted runs at the same level, gated behind a column-major (SoA) view of the active level. The k-way leapfrog invariant is preserved; SIMD is a pairwise accelerator inside it.

**Tech Stack:** Rust 1.90 stable, `horndb-simd` (Stage 1a, must land first), `criterion`, `proptest`.

This plan delivers SPEC-12 **F1** and gates acceptance criteria **#2** (`per_tuple` ≤2.5 ns/tuple), **#3**'s WCOJ-side intersect bench, and **#5** (WCOJ differential fuzzer stays green). **Prerequisite:** `2026-06-27-SPEC-12-simd-primitives.md` is merged.

---

## Background you need (zero-context engineer)

- The WCOJ executor (`crates/wcoj`) implements Veldhuizen's leapfrog triejoin. A `TripleSource` materialises six sorted orderings; a trie cursor (`OrderedTripleIter`) walks one ordering with `peek`/`seek`/`open_level`/`up`. The `LeapfrogJoin` (`crates/wcoj/src/trie/leapfrog.rs`) intersects `k` cursors at one variable depth.
- `TermId` in wcoj is `pub type TermId = u64` (`crates/wcoj/src/ids.rs:3`) — so `horndb_simd`'s `&[u64]` primitives apply directly, no conversion.
- The current seek is a binary search. `VecIter::seek` (`vec_source.rs:98-120`) uses `slice::partition_point`, which the SPEC notes "auto-vectorises into a branchless SIMD comparison" — a happy accident, not a contract (SPEC-12 non-goal §2). `CompressedIter::seek` (`compressed.rs:116-121`) delegates to `PackedColumn::lower_bound` (`packed_column.rs:132-143`), a scalar bisection over bit-packed blocks.
- Tests: `cargo nextest run -p horndb-wcoj`. The differential fuzzer is `crates/wcoj/tests/differential_fuzz.rs` (compares WCOJ vs binary-hash on random BGPs). Benches: `cargo bench -p horndb-wcoj --bench per_tuple` (currently a `fn main(){}` stub) and `--bench four_cycle` (the ≥10× WCOJ-win gate, currently ~34×).
- Record bench numbers on **hornbench only** (`CLAUDE.md`).

### The seek/lower_bound mapping

`VecIter::seek(depth, value)` finds the first row in `data[start..hi]` whose `depth` column is `>= value`. The column is monotone non-decreasing within `[lo, hi)`. That is exactly `lower_bound`. But `VecIter`'s `data` is AoS `&[(u64,u64,u64)]`, so a single column is **strided**, not contiguous — `horndb_simd::lower_bound` wants a contiguous `&[u64]`. Two options:

- **`PackedColumn` / `CompressedIter`:** already column-major. `PackedColumn::lower_bound` can call `horndb_simd::lower_bound` only after decoding a block to a contiguous `u64` scratch buffer — the packed bits aren't a plain slice. This is the clean SIMD target.
- **`VecIter` (AoS):** needs the SoA view the SPEC flags as a prerequisite (SPEC-12 F1 final bullet, "Open: AoS → SoA"). Task 2 builds a transient per-level SoA column for the active range.

---

## File structure

- `crates/wcoj/Cargo.toml` — add `horndb-simd` dependency.
- `crates/wcoj/src/source/soa.rs` — **new**: a transient column-major view (`LevelColumn`) of one trie level's active range, built once per `open_level`.
- `crates/wcoj/src/source/vec_source.rs` — `VecIter` gains a cached SoA column for the active level; `seek` uses `horndb_simd::lower_bound`.
- `crates/wcoj/src/source/packed_column.rs` — `lower_bound` gains a SIMD finish over a decoded block scratch buffer.
- `crates/wcoj/src/trie/leapfrog.rs` — `find_match` gains a pairwise `simd::intersect` fast path.
- `crates/wcoj/benches/per_tuple.rs` — replace the stub with a real per-tuple microbench (acceptance #2).
- `crates/wcoj/benches/intersect_wcoj.rs` — **new** (optional): WCOJ-level intersect bench (acceptance #3).
- `crates/wcoj/Cargo.toml` — register the new bench(es).

---

### Task 1: Add `horndb-simd` as a WCOJ dependency

**Files:**
- Modify: `crates/wcoj/Cargo.toml`
- Modify: root `Cargo.toml` (`[workspace.dependencies]`)

- [ ] **Step 1: Declare the shared dep**

In the root `Cargo.toml` `[workspace.dependencies]`, add:

```toml
horndb-simd = { path = "crates/simd" }
```

- [ ] **Step 2: Reference it from wcoj**

In `crates/wcoj/Cargo.toml` `[dependencies]`, add:

```toml
horndb-simd = { workspace = true }
```

- [ ] **Step 3: Verify it builds**

Run: `cargo build -p horndb-wcoj`
Expected: compiles (no usage yet).

- [ ] **Step 4: Commit**

```bash
git add crates/wcoj/Cargo.toml Cargo.toml
git commit -m "build(wcoj): depend on horndb-simd (SPEC-12 F1)"
```

---

### Task 2: Transient SoA column view for a trie level

`VecIter` stores AoS `&[(u64,u64,u64)]`. To feed `horndb_simd::lower_bound` (contiguous `&[u64]`) and `intersect`, materialise the active level's column into a contiguous scratch `Vec<u64>` once when the level opens, then seek against the scratch.

**Files:**
- Create: `crates/wcoj/src/source/soa.rs`
- Modify: `crates/wcoj/src/source/mod.rs` (add `pub(crate) mod soa;`)
- Test: in `soa.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/wcoj/src/source/soa.rs`:

```rust
//! Transient column-major (SoA) view of one trie level's active range.
//!
//! The dense `VecTripleSource` stores rows as AoS `(u64, u64, u64)`. A single
//! column over a `[lo, hi)` range is therefore strided, which the SIMD
//! `lower_bound`/`intersect` primitives can't consume directly (they want a
//! contiguous `&[u64]`). `LevelColumn` extracts one column of `[lo, hi)` into a
//! contiguous buffer once, so repeated seeks within that level are SIMD-friendly.
//!
//! Rebuilt on each `open_level`; the cost (one strided copy of the active run)
//! is amortised over the seeks the leapfrog performs within the level.

/// A contiguous copy of one column over a trie level's `[lo, hi)` active range.
pub(crate) struct LevelColumn {
    /// Column values for rows `lo..hi`, contiguous.
    values: Vec<u64>,
    /// The `lo` the column was built from, so callers map absolute row indices.
    base: usize,
}

impl LevelColumn {
    /// Extract `data[lo..hi]`'s `depth` column into a contiguous buffer.
    pub(crate) fn from_aos(
        data: &[(u64, u64, u64)],
        lo: usize,
        hi: usize,
        depth: u8,
    ) -> Self {
        let mut values = Vec::with_capacity(hi - lo);
        for row in &data[lo..hi] {
            let v = match depth {
                0 => row.0,
                1 => row.1,
                2 => row.2,
                _ => unreachable!("depth {depth} > 2"),
            };
            values.push(v);
        }
        Self { values, base: lo }
    }

    /// First absolute row index in `[base, base+len)` whose value is `>= value`,
    /// using the SIMD lower_bound. Returns an absolute (data-relative) index.
    pub(crate) fn lower_bound_from(&self, start_abs: usize, value: u64) -> usize {
        let start_rel = start_abs - self.base;
        let off = horndb_simd::lower_bound(&self.values[start_rel..], value);
        start_abs + off
    }

    pub(crate) fn values(&self) -> &[u64] {
        &self.values
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lower_bound_matches_scalar() {
        // Column 1 (middle) of an AoS run.
        let data = vec![(0, 2, 9), (0, 4, 8), (0, 4, 1), (0, 7, 3), (0, 9, 0)];
        let col = LevelColumn::from_aos(&data, 0, data.len(), 1);
        assert_eq!(col.values(), &[2, 4, 4, 7, 9]);
        assert_eq!(col.lower_bound_from(0, 4), 1);
        assert_eq!(col.lower_bound_from(0, 5), 3);
        assert_eq!(col.lower_bound_from(2, 4), 2); // start past first 4
        assert_eq!(col.lower_bound_from(0, 10), 5);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p horndb-wcoj soa`
Expected: FAIL — `soa` module not declared.

- [ ] **Step 3: Declare the module**

In `crates/wcoj/src/source/mod.rs`, add `pub(crate) mod soa;` alongside the other source submodules.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo nextest run -p horndb-wcoj soa`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/wcoj/src/source/soa.rs crates/wcoj/src/source/mod.rs
git commit -m "feat(wcoj): transient SoA LevelColumn for SIMD seek (SPEC-12 F1)"
```

---

### Task 3: `VecIter::seek` uses the SoA column + SIMD lower_bound

Hold a `LevelColumn` per active depth, rebuilt in `open_level`/`up`, and seek against it.

**Files:**
- Modify: `crates/wcoj/src/source/vec_source.rs`
- Test: existing `vec_source.rs` tests + the differential fuzzer (Task 7)

- [ ] **Step 1: Add the cached column to `VecIter`**

In `vec_source.rs`, extend the struct and constructor:

```rust
use crate::source::soa::LevelColumn;

pub struct VecIter<'a> {
    data: &'a [(TermId, TermId, TermId)],
    range: [(usize, usize); 3],
    cursor: [usize; 3],
    /// SoA column for the active range at each depth, rebuilt on open_level/up.
    /// `None` until the level is opened. Depth 0 is built lazily on first seek.
    col_view: [Option<LevelColumn>; 3],
}
```

In `VecIter::new`, initialise `col_view: [None, None, None]` and **eagerly build depth 0** (its range is the full `data`):

```rust
    pub(crate) fn new(data: &'a [(TermId, TermId, TermId)]) -> Self {
        let full = (0usize, data.len());
        let col0 = LevelColumn::from_aos(data, 0, data.len(), 0);
        Self {
            data,
            range: [full, (0, 0), (0, 0)],
            cursor: [0, 0, 0],
            col_view: [Some(col0), None, None],
        }
    }
```

> `LevelColumn` is `Option<…>` and not `Copy`; `[None, None, None]` works because `Option<LevelColumn>` arrays of size 3 can be built with explicit elements (no `Copy` needed for an array literal with named elements).

- [ ] **Step 2: Rewrite `seek` to use the column**

Replace the `partition_point` body (`vec_source.rs:98-120`) with:

```rust
    #[inline]
    fn seek(&mut self, depth: u8, value: TermId) {
        let d = depth as usize;
        let (lo, _hi) = self.range[d];
        let start = self.cursor[d].max(lo);
        // The SoA column for this depth was built by open_level (or new() for
        // depth 0); its lower_bound finishes with a SIMD block compare.
        let col = self.col_view[d]
            .as_ref()
            .expect("seek before open_level at this depth");
        self.cursor[d] = col.lower_bound_from(start, value);
    }
```

- [ ] **Step 3: Rebuild the column in `open_level`**

`open_level` (`vec_source.rs:123-147`) currently sets `range[depth]`. After computing `new_lo`/`new_hi`, build the child column:

```rust
        self.range[depth as usize] = (new_lo, new_hi);
        self.cursor[depth as usize] = new_lo;
        self.col_view[depth as usize] =
            Some(LevelColumn::from_aos(self.data, new_lo, new_hi, depth));
```

Note `open_level` itself still uses `partition_point` to find the run boundary (`end_off`). That call operates on the AoS slice and is correctness-only (run-boundary detection, not the hot seek); leave it scalar — it is not on the per-tuple seek path the NF targets. (Optional follow-up: also route it through the column.)

- [ ] **Step 4: Reset the column in `up`**

In `up` (`vec_source.rs:149+`), when resetting depth 0 rebuild its full column; for `d != 0` clear the stale child column:

```rust
    fn up(&mut self, depth: u8) {
        let d = depth as usize;
        if d == 0 {
            self.range[0] = (0, self.data.len());
            self.cursor[0] = 0;
            self.col_view[0] = Some(LevelColumn::from_aos(self.data, 0, self.data.len(), 0));
        } else {
            self.range[d] = (0, 0);
            self.cursor[d] = 0;
            self.col_view[d] = None;
        }
    }
```

(If `up` has a `rewind` sibling, leave `rewind` untouched — it only moves the cursor, not the range, so the column stays valid.)

- [ ] **Step 5: Run the existing WCOJ tests**

Run: `cargo nextest run -p horndb-wcoj`
Expected: PASS — `VecIter`'s observable behaviour is unchanged; the column is an internal acceleration. Pay attention to any test that constructs a `VecIter` and seeks at depth 1/2 without `open_level` (the `expect` would fire) — the trie protocol always opens a level before seeking it, so this should not occur, but if a unit test violates it, fix the test to follow the protocol.

- [ ] **Step 6: Commit**

```bash
git add crates/wcoj/src/source/vec_source.rs
git commit -m "feat(wcoj): VecIter::seek via SoA column + SIMD lower_bound (SPEC-12 F1)"
```

---

### Task 4: `PackedColumn::lower_bound` SIMD finish

`CompressedIter::seek` already delegates to `PackedColumn::lower_bound`, a scalar bisection over bit-packed blocks. Bisect to the owning block, decode that block to a contiguous scratch buffer once, then SIMD-finish within it.

**Files:**
- Modify: `crates/wcoj/src/source/packed_column.rs`
- Test: existing `packed_column.rs` tests

- [ ] **Step 1: Add a block-decode helper**

In `packed_column.rs`, add a method that decodes one block `[b*BLOCK, min((b+1)*BLOCK, len))` into a caller-provided scratch `Vec<u64>`:

```rust
    /// Decode block `b` into `scratch` (cleared then filled). Returns the
    /// absolute start index of the block.
    #[inline]
    fn decode_block(&self, b: usize, scratch: &mut Vec<u64>) -> usize {
        let start = b * BLOCK;
        let end = ((b + 1) * BLOCK).min(self.len);
        scratch.clear();
        for i in start..end {
            scratch.push(self.get(i));
        }
        start
    }
```

- [ ] **Step 2: Rewrite `lower_bound` to bisect-to-block then SIMD-finish**

Replace the scalar `lower_bound` (`packed_column.rs:132-143`) with:

```rust
    /// First index in `[lo, hi)` whose value is `>= value`, assuming the column
    /// is non-decreasing across that range. Bisects to the owning block by its
    /// frame-of-reference base, decodes that block once, and SIMD-finishes.
    #[inline]
    pub fn lower_bound(&self, lo: usize, hi: usize, value: u64) -> usize {
        if lo >= hi {
            return lo;
        }
        // Block-level bisection: a block can contain the boundary iff its base
        // (block min) < value <= last value. We bisect on block bases, which
        // are non-decreasing for a sorted column.
        let first_block = lo / BLOCK;
        let last_block = (hi - 1) / BLOCK;
        let mut b = first_block;
        // Find the first block whose *max* (next block's base, or +inf) could
        // hold `value`: advance while the next block's base <= value.
        while b < last_block && self.blocks[b + 1].base <= value {
            b += 1;
        }
        // Decode the candidate block and SIMD-finish within its active sub-range.
        let mut scratch: Vec<u64> = Vec::with_capacity(BLOCK);
        let block_start = self.decode_block(b, &mut scratch);
        let sub_lo = lo.max(block_start);
        let sub_hi = hi.min(block_start + scratch.len());
        let rel_lo = sub_lo - block_start;
        let rel_hi = sub_hi - block_start;
        let off = horndb_simd::lower_bound(&scratch[rel_lo..rel_hi], value);
        let idx = sub_lo + off;
        if idx < sub_hi {
            idx
        } else {
            // Boundary is at/after the block end: it's `sub_hi` (== hi if this
            // was the last candidate block).
            sub_hi
        }
    }
```

> **Correctness note:** the block-base advance assumes a sorted column (which `lower_bound`'s contract already requires). The differential check is the existing `packed_column.rs` unit tests plus the WCOJ fuzzer (Task 7) — both compare against the previously-correct scalar bisection / binary-hash. Keep the old scalar `lower_bound` available as `lower_bound_scalar` (rename, don't delete) and add a `#[cfg(test)]` proptest asserting the two agree on random sorted columns.

- [ ] **Step 2b: Add `horndb-simd` to wcoj already done (Task 1).** Confirm `use horndb_simd;` is reachable (it is, as a crate dep — call fully-qualified `horndb_simd::lower_bound`).

- [ ] **Step 3: Add the equivalence proptest**

In `packed_column.rs` `#[cfg(test)] mod tests`, add:

```rust
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn lower_bound_matches_scalar(mut vals: Vec<u64>, value: u64) {
            vals.sort_unstable();
            let col = PackedColumn::from_slice(&vals);
            let n = vals.len();
            let want = vals.partition_point(|&x| x < value);
            prop_assert_eq!(col.lower_bound(0, n, value), want);
        }
    }
```

(Ensure `proptest` is a dev-dependency of `horndb-wcoj` — it already is, per `crates/wcoj/Cargo.toml`.)

- [ ] **Step 4: Run tests**

Run: `cargo nextest run -p horndb-wcoj packed_column`
Expected: PASS, including the proptest.

- [ ] **Step 5: Commit**

```bash
git add crates/wcoj/src/source/packed_column.rs
git commit -m "feat(wcoj): PackedColumn::lower_bound SIMD block-finish + equivalence proptest (SPEC-12 F1)"
```

---

### Task 5: Pairwise SIMD-intersect fast path in `LeapfrogJoin::find_match`

When two cursors at the same level both expose a contiguous sorted run for their active range, intersect them with `horndb_simd::intersect` instead of one-at-a-time `seek`/`peek`. The k-way leapfrog invariant is preserved: the fast path is a pairwise accelerator that produces the same emitted values in the same order.

**Files:**
- Modify: `crates/wcoj/src/trie/leapfrog.rs`
- Modify: `crates/wcoj/src/trie/mod.rs` (extend `TrieIterator` with an optional run accessor)
- Test: the differential fuzzer (Task 7) + a focused unit test

- [ ] **Step 1: Add an optional contiguous-run accessor to `TrieIterator`**

In `crates/wcoj/src/trie/mod.rs`, add a default-`None` method to the `TrieIterator` trait so only sources that can cheaply expose a contiguous `&[u64]` for the active level opt in:

```rust
    /// If this iterator can expose its active level's remaining values as a
    /// contiguous sorted `&[u64]`, return it (for the leapfrog SIMD intersect
    /// fast path). Default `None` — the leapfrog falls back to seek/peek.
    /// The slice must be the values from the current cursor to the level end,
    /// in trie order, with no duplicates (matching the source's dedup).
    fn active_run(&self, _depth: u8) -> Option<&[TermId]> {
        None
    }
```

- [ ] **Step 2: Implement `active_run` for `VecIter`**

In `vec_source.rs`, `VecIter` now holds a `LevelColumn` per depth (Task 3). Expose its tail from the cursor:

```rust
    fn active_run(&self, depth: u8) -> Option<&[TermId]> {
        let d = depth as usize;
        let (lo, hi) = self.range[d];
        let col = self.col_view[d].as_ref()?;
        let start = self.cursor[d].max(lo);
        // The column covers [lo, hi); slice from the cursor to the level end.
        let rel_start = start - lo;
        let rel_end = hi - lo;
        Some(&col.values()[rel_start..rel_end])
    }
```

(`CompressedIter` returns the default `None` — its values are bit-packed, no contiguous slice. The fast path simply doesn't engage for compressed sources; correctness is unaffected.)

- [ ] **Step 3: Add the pairwise fast path to `find_match`**

The current `find_match` (`leapfrog.rs:101-117`) is the round-robin loop. Add a special case **only for the 2-iterator join** (`k == 2`), the common binary-intersection case, when both expose `active_run`. Emit the precomputed intersection one value at a time so the join's `next()` contract is unchanged.

Add an intersection buffer to `LeapfrogJoin`:

```rust
pub struct LeapfrogJoin<'a> {
    iters: Vec<Box<dyn TrieIterator + 'a>>,
    depth: u8,
    p: usize,
    order: Vec<usize>,
    done: bool,
    primed: bool,
    /// Precomputed pairwise intersection (k==2 SIMD fast path) and read cursor.
    simd_buf: Vec<TermId>,
    simd_pos: usize,
    simd_active: bool,
}
```

Initialise the three new fields to `Vec::new()`, `0`, `false` in `new` and `reentry_marker`.

In `next`, after priming (so `order`/heads are set), try to arm the fast path once:

```rust
    fn try_arm_simd(&mut self) {
        if self.simd_active || self.iters.len() != 2 {
            return;
        }
        let a = self.iters[0].active_run(self.depth);
        let b = self.iters[1].active_run(self.depth);
        if let (Some(a), Some(b)) = (a, b) {
            self.simd_buf.clear();
            horndb_simd::intersect(a, b, &mut self.simd_buf);
            self.simd_pos = 0;
            self.simd_active = true;
        }
    }
```

Then make `find_match` consume `simd_buf` when armed:

```rust
    fn find_match(&mut self) -> Option<TermId> {
        if !self.simd_active {
            self.try_arm_simd();
        }
        if self.simd_active {
            if self.simd_pos < self.simd_buf.len() {
                let v = self.simd_buf[self.simd_pos];
                self.simd_pos += 1;
                // Keep the underlying cursors consistent for descent: position
                // both iters at `v` so open_level on the children is correct.
                self.iters[0].seek(self.depth, v);
                self.iters[1].seek(self.depth, v);
                return Some(v);
            }
            self.done = true;
            return None;
        }
        // ... existing scalar round-robin loop unchanged ...
    }
```

> **Why re-seek the cursors:** the WCOJ executor descends via `open_level` on each contributing iter after a match (`iters_mut`), which reads `cursor[depth]`. The SIMD path computes the intersection from the runs but must leave each cursor pointing at the emitted value so descent binds the right child range. The `seek` calls are O(log) each but happen once per *emitted* value (the matches), not per *candidate* — the whole point of the fast path is that the candidate-skipping is done in bulk by `intersect`.

Also adjust `next`'s "subsequent call" branch (`leapfrog.rs:82-93`): when `simd_active`, the post-match advance must **not** run the scalar `seek(cur+1)` rotate — instead it should just call `find_match` again (which reads the next `simd_buf` entry). Guard it:

```rust
        // Subsequent call.
        if self.simd_active {
            return self.find_match();
        }
        let k = self.iters.len();
        // ... existing scalar advance unchanged ...
```

- [ ] **Step 4: Reset the fast path on `up`/descent**

`LeapfrogJoin` is rebuilt per descent by the executor (the join holds iters for one level), so `simd_active` is naturally per-level. Confirm by reading `crates/wcoj/src/executor/wcoj.rs` how the join is constructed each level — if a join is *reused* across `open_level`/`up` rather than reconstructed, add a reset of `simd_buf`/`simd_pos`/`simd_active` wherever the level changes. (Stage-1 executor reconstructs per level; verify and note in the commit.)

- [ ] **Step 5: Focused unit test**

In `leapfrog.rs` `#[cfg(test)]`, add a test that builds two `VecTripleSource` cursors over known overlapping single-predicate runs, runs the leapfrog to exhaustion, and asserts the emitted sequence equals the plain sorted-set intersection of the two runs — exercising the `k==2` SIMD path. Compare against a `BTreeSet` intersection oracle.

- [ ] **Step 6: Run tests**

Run: `cargo nextest run -p horndb-wcoj`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/wcoj/src/trie/leapfrog.rs crates/wcoj/src/trie/mod.rs crates/wcoj/src/source/vec_source.rs
git commit -m "feat(wcoj): pairwise SIMD intersect fast path in leapfrog (SPEC-12 F1)"
```

---

### Task 6: Real `per_tuple` microbench (acceptance #2)

Replace the `fn main(){}` stub with a criterion microbench that measures nanoseconds per emitted tuple on a representative WCOJ scan, so the ≤2.5 ns/tuple gate is checkable on hornbench.

**Files:**
- Modify: `crates/wcoj/benches/per_tuple.rs`

- [ ] **Step 1: Write the bench**

Replace `crates/wcoj/benches/per_tuple.rs`:

```rust
//! SPEC-12 acceptance #2 / SPEC-03 NF1: per-tuple WCOJ overhead.
//! Target: <=2.5 ns/tuple on hornbench (from the <=5 ns Stage-1 envelope toward
//! DuckDB's ~2 ns). Records the `per_tuple` row in docs/benchmarks.md.
//!
//! The bench runs a 2-variable join whose output is large and seek-dominated,
//! so the measured time/tuple isolates the cursor seek + leapfrog inner loop.

use std::time::Duration;

use criterion::{criterion_group, criterion_main, Criterion, Throughput};

use horndb_wcoj::cancel::CancelToken;
use horndb_wcoj::executor::wcoj::WcojExecutor;
use horndb_wcoj::ids::{TermId, Triple};
use horndb_wcoj::pattern::{Bgp, Term, TriplePattern, Var};
use horndb_wcoj::plan::{ExecutionPlan, PlanKind};
use horndb_wcoj::source::vec_source::VecTripleSource;

/// A two-star join: ?x p1 ?y . ?x p2 ?y — output is the intersection of the
/// two predicates' (s,o) pairs, which stresses same-level seek/intersect.
fn build_source(n: u64) -> (VecTripleSource, usize) {
    let mut triples = Vec::new();
    // p1 = 100, p2 = 101; overlap on ~half the (s,o) pairs.
    for s in 0..n {
        for o in 0..8u64 {
            triples.push(Triple::new(s, 100, o));
            if o % 2 == 0 {
                triples.push(Triple::new(s, 101, o));
            }
        }
    }
    let expected_out = (n as usize) * 4; // o in {0,2,4,6}
    (VecTripleSource::from_triples(triples), expected_out)
}

fn bench_per_tuple(c: &mut Criterion) {
    let n = 50_000u64;
    let (source, expected_out) = build_source(n);

    // ?x 100 ?y . ?x 101 ?y
    let bgp = Bgp::new(vec![
        TriplePattern::new(Term::Var(Var::new("x")), Term::Id(100), Term::Var(Var::new("y"))),
        TriplePattern::new(Term::Var(Var::new("x")), Term::Id(101), Term::Var(Var::new("y"))),
    ]);
    let plan = ExecutionPlan::new(PlanKind::Wcoj, bgp);

    let mut group = c.benchmark_group("per_tuple");
    group.throughput(Throughput::Elements(expected_out as u64));
    group.measurement_time(Duration::from_secs(5));
    group.bench_function("two_star_50k", |b| {
        b.iter(|| {
            let exec = WcojExecutor::new(&source, &plan, CancelToken::none());
            let mut count = 0usize;
            for batch in exec.run() {
                count += batch.unwrap().num_rows();
            }
            assert_eq!(count, expected_out);
            count
        });
    });
    group.finish();
}

criterion_group!(benches, bench_per_tuple);
criterion_main!(benches);
```

> **Adapt the API calls to the real surface.** The exact constructors (`Term::Id`, `ExecutionPlan::new`, `WcojExecutor::new`, `exec.run()`) must match the current `horndb-wcoj` public API — cross-check against `crates/wcoj/benches/four_cycle.rs`, which already wires `WcojExecutor`/`Bgp`/`ExecutionPlan` correctly, and copy its exact call shapes. The throughput is set to **output tuples** so criterion's per-element time *is* ns/tuple.

- [ ] **Step 2: Smoke-run locally (not for recording)**

Run: `cargo bench -p horndb-wcoj --bench per_tuple -- --warm-up-time 1 --measurement-time 2`
Expected: completes, prints a time/element. **Do not record laptop numbers.**

- [ ] **Step 3: Confirm four_cycle still passes ≥10×**

Run: `cargo bench -p horndb-wcoj --bench four_cycle -- --warm-up-time 1 --measurement-time 2`
Expected: WCOJ still ≥10× binary-hash (the SIMD seek must not regress the win-case ratio). If it regresses below 10×, the SoA rebuild cost in `open_level` is too high for the win-case shape — investigate before recording (e.g. only build the column when the active run length exceeds a threshold).

- [ ] **Step 4: Record on hornbench**

`ssh hornbench`, check out the branch, `cargo bench -p horndb-wcoj --bench per_tuple` and `--bench four_cycle`. Record `per_tuple` ns/tuple and the four_cycle ratio.

- [ ] **Step 5: Update `docs/benchmarks.md`**

Update the `per_tuple` row with the measured ns/tuple (note it must reach ≤2.5 ns to satisfy acceptance #2) and confirm the four_cycle row stays ≥10×. Note the host (EPYC Zen4) and the dispatched ISA.

- [ ] **Step 6: Commit**

```bash
git add crates/wcoj/benches/per_tuple.rs docs/benchmarks.md
git commit -m "bench(wcoj): real per_tuple microbench + BENCHMARKS row (SPEC-12 #2, NF1)"
```

---

### Task 7: WCOJ differential fuzzer stays green (acceptance #5)

The SIMD seek + intersect path must produce bit-identical bindings to binary-hash. The existing fuzzer is the gate.

**Files:**
- Modify (if needed): `crates/wcoj/tests/differential_fuzz.rs`

- [ ] **Step 1: Run the fuzzer unchanged**

Run: `cargo nextest run -p horndb-wcoj --test differential_fuzz`
Expected: PASS, zero mismatches — the SIMD path is on by default now (`VecTripleSource` is the fuzzer's source), so this *is* the F1-enabled differential check the SPEC requires.

- [ ] **Step 2: Widen coverage of the k==2 path**

The fuzzer builds 2–6 pattern BGPs. Confirm at least some generated BGPs reduce to a 2-iterator leapfrog at some level (most will). Optionally lower `N_VERTICES`/predicate cardinality variance or bump the Stage-1 case count locally to stress the SIMD intersect path — but do **not** commit a heavier default case count (that's a Stage-2 nightly concern per the file's header comment).

- [ ] **Step 3: Run the full WCOJ suite + clippy**

Run: `cargo nextest run -p horndb-wcoj && cargo clippy -p horndb-wcoj --all-targets -- -D warnings`
Expected: green.

- [ ] **Step 4: Commit (only if Step 2 changed anything)**

```bash
git add crates/wcoj/tests/differential_fuzz.rs
git commit -m "test(wcoj): confirm differential fuzzer green with SIMD seek/intersect (SPEC-12 #5)"
```

---

### Task 8: Docs sync

**Files:**
- Modify: `docs/architecture.md`, `TASKS.md`, `docs/benchmarks.md` (already touched), `docs/index.md`

- [ ] **Step 1: Update `docs/architecture.md`** — flip the WCOJ row's SIMD-seek note to **implemented**, referencing SPEC-12 F1 and the `per_tuple` number.

- [ ] **Step 2: Update `TASKS.md`** — check off the SPEC-12 `[#132]` WCOJ-consumer portion (primitives landed in Stage 1a, WCOJ seek/intersect now landed). Per the header procedure, update the GitHub issue. **Do not self-merge** — claim/complete pushes via `tasks.sh` are allowed, merge stays blocked until the user says "merge it" (memory `next-task-automode-guardrails`).

- [ ] **Step 3: Update `docs/index.md`** if it references the WCOJ SIMD state.

- [ ] **Step 4: Commit**

```bash
git add docs/architecture.md TASKS.md docs/index.md
git commit -m "docs(wcoj): mark SPEC-12 F1 seek/intersect implemented, sync architecture/TASKS"
```

---

## Self-review checklist

- **Spec coverage:** F1 seek (3 cursors) → Tasks 3, 4 (`VecIter`, `CompressedIter` via `PackedColumn`); the SoA prerequisite → Task 2. F1 intersect (leapfrog fast path) → Task 5. Output bit-identical → Task 7. NF1 (`per_tuple` ≤2.5 ns) → Task 6; four_cycle no-regression → Task 6 Step 3. Acceptance #2 → Task 6; #5 → Task 7; #3's WCOJ side → covered by the `horndb-simd` intersect bench (Stage 1a Task 12) — a separate WCOJ-level intersect bench is optional and omitted to avoid duplication.
- **Placeholder scan:** the `per_tuple` bench flags "adapt API calls to the real surface" with the concrete reference (`four_cycle.rs`) — that's a real instruction, not a TODO. `CompressedIter` deliberately returns `active_run() == None` (documented).
- **Type consistency:** `LevelColumn::from_aos`/`lower_bound_from`/`values` names match across Tasks 2/3/5; `active_run(depth) -> Option<&[TermId]>` matches in the trait (Task 5 Step 1), `VecIter` impl (Step 2), and leapfrog use (Step 3); `simd_buf`/`simd_pos`/`simd_active` names consistent.

---

## Execution handoff

1. **Subagent-Driven (recommended)** — fresh subagent per task. Critical review gate after Task 5 (leapfrog correctness) and Task 7 (differential green) before any bench recording.
2. **Inline Execution** — checkpoint after Task 4 (seek paths), Task 7 (differential), Task 6 (bench on hornbench).
