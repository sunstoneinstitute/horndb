---
status: executed
date: 2026-05-31
scope: "Compressed columnar WCOJ TripleSource"
---

# Compressed columnar WCOJ TripleSource Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a memory-compact `CompressedTripleSource` to `horndb-wcoj` that implements the existing `TripleSource`/`OrderedTripleIter` cursor protocol over frame-of-reference + bit-packed columns, then re-point the `four_cycle` benchmark at it and measure whether the reduced working set moves the WCOJ-vs-binary-hash ratio toward the ≥10× SPEC-03 acceptance gate (GitHub #15, unblocks #1).

**Architecture:** The WCOJ executor is generic over `TripleSource` (`crates/wcoj/src/source/mod.rs`) and is deliberately decoupled from `horndb-storage`. We add a *new* source implementation inside `horndb-wcoj` — no cross-crate dependency, no change to the executor. Each ordering's sorted `(l0,l1,l2)` rows are stored as three `PackedColumn`s (per-block frame-of-reference min + minimal bit-width payload). A `CompressedIter` mirrors `VecIter`'s exact trie-cursor semantics (`peek`/`seek`/`open_level`/`up`/`rewind`) but reads column values through `PackedColumn::get` and binary-searches via `PackedColumn::{lower_bound,upper_bound}` instead of `slice::partition_point`. Because the hot 4-cycle orderings (Pso, Pos) have a constant predicate column (0 bits) and monotone subject/object columns (small per-block deltas), the ~48 MB dense working set shrinks several-fold — ideally under L3.

**Tech Stack:** Rust 1.88.0, `horndb-wcoj` crate, `criterion` 0.5 benches, `proptest` differential tests. No new dependencies.

---

## File Structure

- **Create** `crates/wcoj/src/source/packed_column.rs` — the `PackedColumn` FOR+bit-pack codec: build from a `&[u64]`, `get(i)`, `len()`, `lower_bound`/`upper_bound` binary searches, `heap_bytes()`. One clear responsibility: encode/decode one sorted-ish u64 column compactly. Pure, no triple/ordering knowledge.
- **Create** `crates/wcoj/src/source/compressed.rs` — `CompressedTripleSource` (`from_triples`, `impl TripleSource`) and `CompressedIter` (`impl OrderedTripleIter`). Mirrors `VecTripleSource`/`VecIter` shape but over three `PackedColumn`s per ordering.
- **Modify** `crates/wcoj/src/source/mod.rs` — add `pub mod packed_column;` and `pub mod compressed;`.
- **Modify** `crates/wcoj/src/source/synthetic.rs` — extract the edge generator into a reusable `pub fn cyclic_edges(n,k,predicate,seed) -> Vec<Triple>` so both `VecTripleSource` and `CompressedTripleSource` can be built from identical edges in the bench.
- **Create** `crates/wcoj/tests/source_parity.rs` — proptest asserting `WcojExecutor` over `CompressedTripleSource` produces identical result sets to `WcojExecutor` over `VecTripleSource`.
- **Modify** `crates/wcoj/benches/four_cycle.rs` — add `wcoj_compressed` and `binary_hash_compressed` bench functions over a `CompressedTripleSource` built from the same edges, plus a one-time footprint print.
- **Modify** `docs/benchmarks.md`, `docs/architecture.md`, `TASKS.md` — record measured numbers and bookkeeping (Task 5).

---

### Task 1: `PackedColumn` frame-of-reference + bit-packing codec

**Files:**
- Create: `crates/wcoj/src/source/packed_column.rs`
- Modify: `crates/wcoj/src/source/mod.rs` (add `pub mod packed_column;`)
- Test: inline `#[cfg(test)] mod tests` in `packed_column.rs`

- [ ] **Step 1: Register the module**

In `crates/wcoj/src/source/mod.rs`, add ONLY this line in Task 1, after the existing `pub mod synthetic;` / `pub mod vec_source;` lines (around line 8-9). (`pub mod compressed;` is added separately in Task 2, once that file exists — declaring a module whose file is absent fails to compile.)

```rust
pub mod packed_column;
```

- [ ] **Step 2: Write the failing tests**

Create `crates/wcoj/src/source/packed_column.rs`:

```rust
//! `PackedColumn` — a compact, random-access encoding of one `u64` column.
//!
//! The column is split into fixed-size blocks. Each block stores a
//! frame-of-reference base (the block minimum) and the minimal bit width `w`
//! needed to represent `value - base` for every value in the block; the
//! residuals are bit-packed LSB-first into a shared `u64` word stream, with
//! each block starting on a word boundary so `get` never needs the block's
//! global bit offset. A constant block uses `w = 0` and stores nothing.
//!
//! This is the building block for `CompressedTripleSource`: the WCOJ trie
//! cursor reads column values via [`PackedColumn::get`] and narrows ranges via
//! [`PackedColumn::lower_bound`] / [`PackedColumn::upper_bound`], so the column
//! never needs to be fully materialised as dense `u64`s.

/// Values per block. 256 keeps per-block metadata overhead negligible while
/// still letting frame-of-reference exploit local value locality in sorted
/// columns.
const BLOCK: usize = 256;

#[derive(Clone, Copy)]
struct BlockMeta {
    /// Frame-of-reference base: the minimum value in the block.
    base: u64,
    /// Bit width of `value - base`. `0` means a constant block (no payload).
    bits: u8,
    /// Index into `words` where this block's packed residuals start.
    word_offset: u32,
}

/// A compact, random-access encoding of one `u64` column.
pub struct PackedColumn {
    len: usize,
    blocks: Vec<BlockMeta>,
    words: Vec<u64>,
}

#[inline]
fn bits_for(max_delta: u64) -> u8 {
    if max_delta == 0 {
        0
    } else {
        (64 - max_delta.leading_zeros()) as u8
    }
}

impl PackedColumn {
    /// Encode `values` (any order; sorted not required for correctness, but
    /// frame-of-reference compresses sorted/locally-clustered data best).
    pub fn from_slice(values: &[u64]) -> Self {
        let mut blocks = Vec::with_capacity(values.len().div_ceil(BLOCK));
        let mut words: Vec<u64> = Vec::new();
        for chunk in values.chunks(BLOCK) {
            let base = *chunk.iter().min().expect("non-empty chunk");
            let max_delta = chunk.iter().map(|v| v - base).max().unwrap();
            let bits = bits_for(max_delta);
            let word_offset = words.len() as u32;
            blocks.push(BlockMeta {
                base,
                bits,
                word_offset,
            });
            if bits == 0 {
                continue;
            }
            // Reserve enough words for `chunk.len() * bits` bits, then write.
            let total_bits = chunk.len() * bits as usize;
            let n_words = total_bits.div_ceil(64);
            words.resize(word_offset as usize + n_words, 0);
            for (i, v) in chunk.iter().enumerate() {
                let delta = v - base;
                let bit_index = i * bits as usize;
                let w = word_offset as usize + bit_index / 64;
                let off = bit_index % 64;
                words[w] |= delta << off;
                if off + bits as usize > 64 {
                    words[w + 1] |= delta >> (64 - off);
                }
            }
        }
        Self {
            len: values.len(),
            blocks,
            words,
        }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Decode the value at index `i`. Panics if `i >= len`.
    #[inline]
    pub fn get(&self, i: usize) -> u64 {
        let meta = &self.blocks[i / BLOCK];
        if meta.bits == 0 {
            return meta.base;
        }
        let bits = meta.bits as usize;
        let bit_index = (i % BLOCK) * bits;
        let w = meta.word_offset as usize + bit_index / 64;
        let off = bit_index % 64;
        let mask = if bits == 64 {
            u64::MAX
        } else {
            (1u64 << bits) - 1
        };
        let mut v = self.words[w] >> off;
        if off + bits > 64 {
            v |= self.words[w + 1] << (64 - off);
        }
        meta.base + (v & mask)
    }

    /// First index in `[lo, hi)` whose value is `>= value`, assuming the column
    /// is non-decreasing across that range. Mirrors `slice::partition_point(|x| x < value)`.
    #[inline]
    pub fn lower_bound(&self, lo: usize, hi: usize, value: u64) -> usize {
        let (mut lo, mut hi) = (lo, hi);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if self.get(mid) < value {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        lo
    }

    /// First index in `[lo, hi)` whose value is `> value`, assuming the column
    /// is non-decreasing across that range. Mirrors `slice::partition_point(|x| x <= value)`.
    #[inline]
    pub fn upper_bound(&self, lo: usize, hi: usize, value: u64) -> usize {
        let (mut lo, mut hi) = (lo, hi);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if self.get(mid) <= value {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        lo
    }

    /// Heap bytes used by this column (payload + per-block metadata).
    pub fn heap_bytes(&self) -> usize {
        self.words.len() * std::mem::size_of::<u64>()
            + self.blocks.len() * std::mem::size_of::<BlockMeta>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(values: &[u64]) {
        let col = PackedColumn::from_slice(values);
        assert_eq!(col.len(), values.len());
        for (i, &v) in values.iter().enumerate() {
            assert_eq!(col.get(i), v, "mismatch at index {i}");
        }
    }

    #[test]
    fn roundtrip_empty() {
        roundtrip(&[]);
    }

    #[test]
    fn roundtrip_single() {
        roundtrip(&[42]);
    }

    #[test]
    fn roundtrip_constant_block() {
        roundtrip(&vec![7u64; 300]);
    }

    #[test]
    fn roundtrip_monotonic_multiblock() {
        let v: Vec<u64> = (0..1000u64).map(|i| i * 3 + 5).collect();
        roundtrip(&v);
    }

    #[test]
    fn roundtrip_random_with_large_values() {
        // Values needing wide bit widths, including ones that force a 64-bit
        // residual (base 0, value u64::MAX) and cross-word reads.
        let v = vec![0u64, u64::MAX, 1, u64::MAX - 1, 1 << 40, (1 << 40) + 7];
        roundtrip(&v);
    }

    #[test]
    fn roundtrip_full_block_boundary() {
        // Exactly BLOCK and BLOCK+1 elements exercise the block-boundary path.
        roundtrip(&(0..BLOCK as u64).collect::<Vec<_>>());
        roundtrip(&(0..(BLOCK as u64 + 1)).collect::<Vec<_>>());
    }

    #[test]
    fn lower_upper_bound_match_partition_point() {
        let v: Vec<u64> = (0..500u64).map(|i| (i / 3) * 2).collect(); // sorted, with dups
        let col = PackedColumn::from_slice(&v);
        for target in [0u64, 1, 2, 4, 332, 999] {
            let lb = col.lower_bound(0, v.len(), target);
            let expect_lb = v.partition_point(|&x| x < target);
            assert_eq!(lb, expect_lb, "lower_bound target={target}");
            let ub = col.upper_bound(0, v.len(), target);
            let expect_ub = v.partition_point(|&x| x <= target);
            assert_eq!(ub, expect_ub, "upper_bound target={target}");
        }
    }

    #[test]
    fn bounds_respect_subrange() {
        let v: Vec<u64> = (0..100u64).collect();
        let col = PackedColumn::from_slice(&v);
        assert_eq!(col.lower_bound(10, 20, 5), 10); // clamped to lo
        assert_eq!(col.lower_bound(10, 20, 15), 15);
        assert_eq!(col.lower_bound(10, 20, 99), 20); // clamped to hi
    }
}
```

- [ ] **Step 3: Run tests to verify they fail, then pass**

Run: `cargo test -p horndb-wcoj packed_column`
Expected first: compile error only if the module isn't registered — confirm Step 1 added `pub mod packed_column;`. Once it compiles, all 8 tests PASS. If any `get` test fails, the bit-packing boundary math is wrong — debug `get`/`from_slice` before proceeding.

- [ ] **Step 4: Commit**

```bash
git add crates/wcoj/src/source/packed_column.rs crates/wcoj/src/source/mod.rs
git commit -m "feat(wcoj): add PackedColumn frame-of-reference bit-packing codec"
```

---

### Task 2: `CompressedTripleSource` + `CompressedIter`

**Files:**
- Create: `crates/wcoj/src/source/compressed.rs`
- Modify: `crates/wcoj/src/source/mod.rs` (add `pub mod compressed;`)
- Test: inline `#[cfg(test)] mod tests` in `compressed.rs`

- [ ] **Step 1: Register the module**

In `crates/wcoj/src/source/mod.rs` add (keeping alphabetical with the Task 1 line):

```rust
pub mod compressed;
```

- [ ] **Step 2: Write the failing test + implementation**

Create `crates/wcoj/src/source/compressed.rs`. The iterator mirrors `VecIter` (`crates/wcoj/src/source/vec_source.rs:48-161`) exactly, but reads columns via `PackedColumn::get` and searches via `lower_bound`/`upper_bound`:

```rust
//! `CompressedTripleSource` — a memory-compact `TripleSource`.
//!
//! Same external behaviour as [`crate::source::vec_source::VecTripleSource`]
//! (all six orderings materialised, sorted, deduped), but each ordering's
//! three columns are stored as [`PackedColumn`]s (frame-of-reference +
//! bit-packing) instead of a dense `Vec<(u64,u64,u64)>`. The trie cursor
//! semantics are identical to `VecIter`; only the physical reads differ.

use std::collections::HashMap;

use crate::error::{Result, WcojError};
use crate::ids::{Ordering, TermId, Triple};
use crate::source::packed_column::PackedColumn;
use crate::source::{OrderedTripleIter, TripleSource};

/// Three packed columns (level 0, 1, 2) for one ordering, plus row count.
struct OrderColumns {
    cols: [PackedColumn; 3],
    rows: usize,
}

pub struct CompressedTripleSource {
    sorted: HashMap<Ordering, OrderColumns>,
    total: usize,
}

impl CompressedTripleSource {
    pub fn from_triples(triples: Vec<Triple>) -> Self {
        let total = triples.len();
        let mut sorted = HashMap::with_capacity(6);
        for &ord in &Ordering::ALL {
            let mut rows: Vec<(TermId, TermId, TermId)> =
                triples.iter().map(|t| t.by_ordering(ord)).collect();
            rows.sort_unstable();
            rows.dedup();
            let l0: Vec<u64> = rows.iter().map(|r| r.0).collect();
            let l1: Vec<u64> = rows.iter().map(|r| r.1).collect();
            let l2: Vec<u64> = rows.iter().map(|r| r.2).collect();
            sorted.insert(
                ord,
                OrderColumns {
                    cols: [
                        PackedColumn::from_slice(&l0),
                        PackedColumn::from_slice(&l1),
                        PackedColumn::from_slice(&l2),
                    ],
                    rows: rows.len(),
                },
            );
        }
        Self { sorted, total }
    }

    /// Total heap bytes across every materialised ordering. Used by the bench
    /// to report bytes/triple against the dense `VecTripleSource`.
    pub fn heap_bytes(&self) -> usize {
        self.sorted
            .values()
            .map(|o| o.cols.iter().map(|c| c.heap_bytes()).sum::<usize>())
            .sum()
    }
}

impl TripleSource for CompressedTripleSource {
    type Iter<'a> = CompressedIter<'a>;

    fn iter(&self, ord: Ordering) -> Result<CompressedIter<'_>> {
        let oc = self
            .sorted
            .get(&ord)
            .ok_or(WcojError::OrderingUnavailable(ord))?;
        Ok(CompressedIter::new(&oc.cols, oc.rows))
    }

    fn total_triples(&self) -> usize {
        self.total
    }
}

/// Cursor over three [`PackedColumn`]s. Field-for-field analogue of
/// [`crate::source::vec_source::VecIter`].
pub struct CompressedIter<'a> {
    cols: &'a [PackedColumn; 3],
    rows: usize,
    /// (lo, hi) per depth — `hi` exclusive.
    range: [(usize, usize); 3],
    /// Cursor index per depth.
    cursor: [usize; 3],
}

impl<'a> CompressedIter<'a> {
    pub(crate) fn new(cols: &'a [PackedColumn; 3], rows: usize) -> Self {
        Self {
            cols,
            rows,
            range: [(0, rows), (0, 0), (0, 0)],
            cursor: [0, 0, 0],
        }
    }
}

impl<'a> OrderedTripleIter for CompressedIter<'a> {
    #[inline]
    fn peek(&self, depth: u8) -> Option<TermId> {
        let (lo, hi) = self.range[depth as usize];
        let c = self.cursor[depth as usize].max(lo);
        if c >= hi {
            return None;
        }
        Some(self.cols[depth as usize].get(c))
    }

    #[inline]
    fn seek(&mut self, depth: u8, value: TermId) {
        let d = depth as usize;
        let (lo, hi) = self.range[d];
        let start = self.cursor[d].max(lo);
        self.cursor[d] = self.cols[d].lower_bound(start, hi, value);
    }

    #[inline]
    fn open_level(&mut self, depth: u8) {
        assert!((1..=2).contains(&depth), "open_level depth must be 1 or 2");
        let parent = (depth - 1) as usize;
        let (_, hi_parent) = self.range[parent];
        let row = self.cursor[parent];
        let v = self.cols[parent].get(row);
        // Contiguous run in [row, hi_parent) whose parent column == v.
        let new_hi = self.cols[parent].upper_bound(row, hi_parent, v);
        self.range[depth as usize] = (row, new_hi);
        self.cursor[depth as usize] = row;
    }

    #[inline]
    fn up(&mut self, depth: u8) {
        let d = depth as usize;
        if d == 0 {
            self.range[0] = (0, self.rows);
            self.cursor[0] = 0;
        } else {
            self.range[d] = (0, 0);
            self.cursor[d] = 0;
        }
    }

    #[inline]
    fn rewind(&mut self, depth: u8) {
        let d = depth as usize;
        self.cursor[d] = self.range[d].0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::vec_source::VecTripleSource;

    fn sample_triples() -> Vec<Triple> {
        vec![
            Triple::new(1, 10, 2),
            Triple::new(1, 10, 5),
            Triple::new(2, 10, 3),
            Triple::new(2, 11, 3),
            Triple::new(4, 10, 1),
        ]
    }

    /// Walking one ordering with a manual peek/open_level/up sequence must
    /// yield the same values as the dense `VecIter`.
    #[test]
    fn matches_vec_iter_walk_spo() {
        let triples = sample_triples();
        let comp = CompressedTripleSource::from_triples(triples.clone());
        let dense = VecTripleSource::from_triples(triples);

        let mut ci = comp.iter(Ordering::Spo).unwrap();
        let mut vi = dense.iter(Ordering::Spo).unwrap();

        // Depth 0: iterate every distinct subject, descending into each.
        loop {
            let (cs, vs) = (ci.peek(0), vi.peek(0));
            assert_eq!(cs, vs);
            if cs.is_none() {
                break;
            }
            ci.open_level(1);
            vi.open_level(1);
            loop {
                assert_eq!(ci.peek(1), vi.peek(1));
                if ci.peek(1).is_none() {
                    break;
                }
                ci.open_level(2);
                vi.open_level(2);
                loop {
                    assert_eq!(ci.peek(2), vi.peek(2));
                    if ci.peek(2).is_none() {
                        break;
                    }
                    ci.seek(2, ci.peek(2).unwrap() + 1);
                    vi.seek(2, vi.peek(2).unwrap() + 1);
                }
                ci.up(2);
                vi.up(2);
                ci.seek(1, ci.peek(1).unwrap() + 1);
                vi.seek(1, vi.peek(1).unwrap() + 1);
            }
            ci.up(1);
            vi.up(1);
            ci.seek(0, ci.peek(0).unwrap() + 1);
            vi.seek(0, vi.peek(0).unwrap() + 1);
        }
    }

    #[test]
    fn total_triples_matches() {
        let triples = sample_triples();
        let comp = CompressedTripleSource::from_triples(triples.clone());
        let dense = VecTripleSource::from_triples(triples);
        assert_eq!(comp.total_triples(), dense.total_triples());
    }

    #[test]
    fn heap_bytes_is_smaller_for_constant_predicate() {
        // Single-predicate graph: l0 of Pso/Pos is constant → near-zero bits.
        let triples: Vec<Triple> = (0..1000u64).map(|s| Triple::new(s, 10, (s * 7) % 1000)).collect();
        let comp = CompressedTripleSource::from_triples(triples);
        // 1000 triples × 6 orderings × 24 bytes dense = 144_000 bytes of payload.
        // Compressed must be well under half that.
        assert!(
            comp.heap_bytes() < 72_000,
            "compressed heap_bytes={} not < 72000",
            comp.heap_bytes()
        );
    }
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p horndb-wcoj compressed`
Expected: all 3 tests PASS. `matches_vec_iter_walk_spo` is the correctness anchor — if it fails, compare `CompressedIter` against `VecIter` method-by-method (`vec_source.rs:80-161`).

- [ ] **Step 4: Run the full wcoj suite to confirm nothing regressed**

Run: `cargo test -p horndb-wcoj`
Expected: PASS (existing executor/fuzzer tests untouched).

- [ ] **Step 5: Commit**

```bash
git add crates/wcoj/src/source/compressed.rs crates/wcoj/src/source/mod.rs
git commit -m "feat(wcoj): add CompressedTripleSource over packed columns"
```

---

### Task 3: Source-parity differential test

**Files:**
- Create: `crates/wcoj/tests/source_parity.rs`

This reuses the structure of `crates/wcoj/tests/differential_fuzz.rs` but compares `WcojExecutor` over the *compressed* source against `WcojExecutor` over the *dense* source — proving the new source is a behaviour-preserving drop-in for arbitrary BGPs.

- [ ] **Step 1: Write the failing test**

Create `crates/wcoj/tests/source_parity.rs`:

```rust
//! Differential parity: `WcojExecutor` over `CompressedTripleSource` must
//! produce identical result sets to `WcojExecutor` over `VecTripleSource`
//! for arbitrary BGPs. This proves the compressed source is a
//! behaviour-preserving drop-in (GitHub #15).

use std::collections::BTreeSet;

use arrow::array::UInt64Array;
use proptest::prelude::*;

use horndb_wcoj::cancel::CancelToken;
use horndb_wcoj::executor::wcoj::WcojExecutor;
use horndb_wcoj::ids::{TermId, Triple};
use horndb_wcoj::pattern::{Bgp, Term, TriplePattern, Var};
use horndb_wcoj::plan::{ExecutionPlan, PlanKind};
use horndb_wcoj::source::compressed::CompressedTripleSource;
use horndb_wcoj::source::vec_source::VecTripleSource;

const N_VERTICES: u64 = 30;
const PREDICATES: &[u64] = &[100, 101, 102];

fn build_triples(seed: u64) -> Vec<Triple> {
    let mut state = seed | 1;
    let mut rand = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let mut triples = Vec::new();
    for s in 0..N_VERTICES {
        for &p in PREDICATES {
            for _ in 0..(rand() % 4) {
                let o = rand() % N_VERTICES;
                triples.push(Triple::new(s, p, o));
            }
        }
    }
    triples
}

fn collect_rows(
    batches: impl Iterator<Item = horndb_wcoj::error::Result<arrow::record_batch::RecordBatch>>,
) -> BTreeSet<Vec<TermId>> {
    let mut out = BTreeSet::new();
    for b in batches {
        let b = b.unwrap();
        let cols: Vec<&UInt64Array> = (0..b.num_columns())
            .map(|i| b.column(i).as_any().downcast_ref::<UInt64Array>().unwrap())
            .collect();
        for r in 0..b.num_rows() {
            out.insert(cols.iter().map(|c| c.value(r)).collect::<Vec<TermId>>());
        }
    }
    out
}

fn arb_term() -> impl Strategy<Value = Term> {
    prop_oneof![
        (0u8..3u8).prop_map(|v| Term::Var(Var(v))),
        (0u64..N_VERTICES).prop_map(Term::Bound),
    ]
}

fn arb_predicate_term() -> impl Strategy<Value = Term> {
    prop::sample::select(PREDICATES.to_vec()).prop_map(Term::Bound)
}

fn arb_pattern() -> impl Strategy<Value = TriplePattern> {
    (arb_term(), arb_predicate_term(), arb_term())
        .prop_map(|(s, p, o)| TriplePattern::new(s, p, o))
        .prop_filter("no self-loop variables", |pat| {
            let mut seen = std::collections::HashSet::new();
            for t in [pat.s, pat.p, pat.o] {
                if let Term::Var(v) = t {
                    if !seen.insert(v) {
                        return false;
                    }
                }
            }
            true
        })
}

fn arb_bgp() -> impl Strategy<Value = Bgp> {
    prop::collection::vec(arb_pattern(), 2..=6).prop_map(Bgp::new)
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    #[test]
    fn compressed_matches_dense(seed in any::<u64>(), bgp in arb_bgp()) {
        let triples = build_triples(seed);
        let dense = VecTripleSource::from_triples(triples.clone());
        let comp = CompressedTripleSource::from_triples(triples);

        let out_vars = bgp.variables();
        prop_assume!(!out_vars.is_empty());

        let plan = ExecutionPlan {
            kind: PlanKind::Wcoj,
            var_order: out_vars.clone(),
        };
        let dense_rows = collect_rows(
            WcojExecutor::new(&dense, &bgp, &plan, CancelToken::new()).into_iter(),
        );
        let comp_rows = collect_rows(
            WcojExecutor::new(&comp, &bgp, &plan, CancelToken::new()).into_iter(),
        );
        prop_assert_eq!(dense_rows, comp_rows);
    }
}
```

- [ ] **Step 2: Run the test**

Run: `cargo test -p horndb-wcoj --test source_parity`
Expected: PASS (256 proptest cases, zero mismatches). A mismatch means `CompressedIter` diverges from `VecIter` on some cursor path — debug against the failing seed/BGP proptest prints.

- [ ] **Step 3: Commit**

```bash
git add crates/wcoj/tests/source_parity.rs
git commit -m "test(wcoj): differential parity for CompressedTripleSource vs dense"
```

---

### Task 4: Re-point the `four_cycle` bench + measure footprint

**Files:**
- Modify: `crates/wcoj/src/source/synthetic.rs` (extract `cyclic_edges`)
- Modify: `crates/wcoj/benches/four_cycle.rs`

- [ ] **Step 1: Expose the edge generator on `SyntheticGraph`**

In `crates/wcoj/src/source/synthetic.rs`, refactor `cyclic` to delegate to a new public `cyclic_edges`. Replace the body of `impl SyntheticGraph` (lines 19-48) with:

```rust
impl SyntheticGraph {
    /// Deterministically generate the cyclic graph's edges (no source built).
    /// Exposed so benches can build both a dense and a compressed source from
    /// identical edges.
    pub fn cyclic_edges(n: u64, k: u64, predicate: u64, seed: u64) -> Vec<Triple> {
        let mut state = seed | 1;
        let mut rand = || -> u64 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };

        let mut edges: BTreeSet<Triple> = BTreeSet::new();
        for s in 0..n {
            let mut added = 0;
            while added < k {
                let o = rand() % n;
                if o == s {
                    continue;
                }
                if edges.insert(Triple::new(s, predicate, o)) {
                    added += 1;
                }
            }
        }
        edges.into_iter().collect()
    }

    pub fn cyclic(n: u64, k: u64, predicate: u64, seed: u64) -> Self {
        let triples = Self::cyclic_edges(n, k, predicate, seed);
        Self {
            inner: VecTripleSource::from_triples(triples),
        }
    }
}
```

- [ ] **Step 2: Verify the refactor compiles and tests pass**

Run: `cargo test -p horndb-wcoj synthetic` then `cargo build -p horndb-wcoj --benches`
Expected: PASS / clean build. (No behaviour change — `cyclic` produces the same edges.)

- [ ] **Step 3: Add compressed bench functions + footprint print**

In `crates/wcoj/benches/four_cycle.rs`, add to the imports near the top (after the existing `use horndb_wcoj::source::synthetic::SyntheticGraph;`):

```rust
use horndb_wcoj::source::compressed::CompressedTripleSource;
use horndb_wcoj::source::vec_source::VecTripleSource;
```

Then rewrite `bench_four_cycle` so it builds both sources from one edge set, prints the footprint once, and benches WCOJ + binary-hash over each. **Replace the entire span from the `let graph = SyntheticGraph::cyclic(...)` line (currently line 35) through the function's closing `group.finish();` (currently line 67) — inclusive — with the block below.** The block ends with its own `group.finish();`, so afterwards the function must contain exactly one `group.finish();` and the `criterion_group!`/`criterion_main!` lines below it stay unchanged:

```rust
    // 10^6 edges: 250_000 vertices * 4 out-edges = 1_000_000.
    let edges = SyntheticGraph::cyclic_edges(250_000, 4, 10, 0xDEAD_BEEF);
    let dense = VecTripleSource::from_triples(edges.clone());
    let compressed = CompressedTripleSource::from_triples(edges);
    let bgp = make_4_cycle_bgp();

    // One-time footprint report (stdout; criterion does not capture this).
    let comp_bytes = compressed.heap_bytes();
    let n = dense.total_triples().max(1);
    eprintln!(
        "four_cycle source footprint: compressed = {} bytes ({:.2} B/triple over 6 orderings); \
         dense ≈ {} bytes ({} B/triple)",
        comp_bytes,
        comp_bytes as f64 / n as f64,
        n * 6 * 24,
        6 * 24,
    );

    let mut group = c.benchmark_group("four_cycle");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(30));

    group.bench_function("wcoj_dense", |b| {
        b.iter(|| {
            let plan = ExecutionPlan {
                kind: PlanKind::Wcoj,
                var_order: vec![Var(0), Var(1), Var(2), Var(3)],
            };
            let exec = WcojExecutor::new(&dense, &bgp, &plan, CancelToken::new());
            let mut rows = 0u64;
            for batch in exec.into_iter() {
                rows += batch.unwrap().num_rows() as u64;
            }
            criterion::black_box(rows);
        });
    });

    group.bench_function("wcoj_compressed", |b| {
        b.iter(|| {
            let plan = ExecutionPlan {
                kind: PlanKind::Wcoj,
                var_order: vec![Var(0), Var(1), Var(2), Var(3)],
            };
            let exec = WcojExecutor::new(&compressed, &bgp, &plan, CancelToken::new());
            let mut rows = 0u64;
            for batch in exec.into_iter() {
                rows += batch.unwrap().num_rows() as u64;
            }
            criterion::black_box(rows);
        });
    });

    group.bench_function("binary_hash_dense", |b| {
        b.iter(|| {
            let exec = BinaryHashExecutor::new(
                &dense,
                &bgp,
                vec![Var(0), Var(1), Var(2), Var(3)],
                CancelToken::new(),
            );
            let mut rows = 0u64;
            for batch in exec.into_iter() {
                rows += batch.unwrap().num_rows() as u64;
            }
            criterion::black_box(rows);
        });
    });

    group.bench_function("binary_hash_compressed", |b| {
        b.iter(|| {
            let exec = BinaryHashExecutor::new(
                &compressed,
                &bgp,
                vec![Var(0), Var(1), Var(2), Var(3)],
                CancelToken::new(),
            );
            let mut rows = 0u64;
            for batch in exec.into_iter() {
                rows += batch.unwrap().num_rows() as u64;
            }
            criterion::black_box(rows);
        });
    });

    group.finish();
```

Note: delete the now-replaced original `wcoj` / `binary_hash` blocks and the old `let graph = ...` line so they are not duplicated. The closing `group.finish();` (if already present at the end of the function) must appear exactly once.

- [ ] **Step 4: Build the bench, then run it and capture numbers**

Run: `cargo build -p horndb-wcoj --benches`
Expected: clean compile.

Run: `cargo bench -p horndb-wcoj --bench four_cycle 2>&1 | tee /tmp/four_cycle_bench.txt`
Expected: criterion reports four functions (`wcoj_dense`, `wcoj_compressed`, `binary_hash_dense`, `binary_hash_compressed`) and the `eprintln!` footprint line. Record:
- the footprint line (compressed B/triple vs dense 144 B/triple),
- mean time for each of the four functions,
- the ratio `binary_hash_compressed / wcoj_compressed` (the SPEC-03 #1 gate ratio on the compressed source) and `wcoj_dense / wcoj_compressed` (the bandwidth win).

These numbers feed Task 5. Do not fabricate — paste the actual criterion output into the commit body.

- [ ] **Step 5: Commit**

```bash
git add crates/wcoj/src/source/synthetic.rs crates/wcoj/benches/four_cycle.rs
git commit -m "bench(wcoj): run four_cycle over compressed source; report footprint"
```

---

### Task 5: Record measured results + docs/issue bookkeeping

**Files:**
- Modify: `docs/benchmarks.md`
- Modify: `docs/architecture.md`
- Modify: `TASKS.md`

> The measured numbers come from Task 4's `/tmp/four_cycle_bench.txt`. Use the
> REAL numbers. The placeholders `<…>` below MUST be replaced with captured
> values before committing.

- [ ] **Step 1: Update `docs/benchmarks.md` 4-cycle row**

Find the `four_cycle` row (around `docs/benchmarks.md:144`) and update the "current measured" cell to include the compressed numbers, e.g.:

```
| 4-cycle, 10⁶-edge synthetic (`benches/four_cycle.rs`) | `horndb-wcoj` | WCOJ ≥10× binary-hash | dense: WCOJ <X> s vs binary-hash <Y> s; **compressed: WCOJ <A> s vs binary-hash <B> s → <R>× faster**; source footprint <F> B/triple (was 144) (2026-05-31) | <RED if R<10 else GREEN — note> |
```

Also update the warm-tier/footprint discussion lines (around `docs/benchmarks.md:101-150`) to note the compressed-source result and whether it closed the L3 gap.

- [ ] **Step 2: Update `docs/architecture.md`**

In §5 (SPEC-03), update the 4-cycle gate row (around `docs/architecture.md:167`) to reflect the compressed-source measurement. If `R >= 10`, change its **Status** to **implemented** and note #1 closed; otherwise keep **planned** and record the new ratio + remaining gap. In §4 (SPEC-02, around line 145-147), update the note to point at the new compressed wcoj source and the measured result.

- [ ] **Step 3: Update `TASKS.md` — close increment #15, parent #3 stays `[v]`**

In the body breakdown bullet for #3 (the "Epic breakdown" sub-bullet added on the claim), mark #15 done: change its `#15` reference to note it is delivered (e.g. prefix `✅`). Do NOT flip the parent `[v]`. If the ≥10× gate was met, also flip the #1 index + body lines `[ ]`→`[x]` (this is the documented unblock); otherwise leave #1 open and update its measured number inline.

Example breakdown edit:

```
    ✅ [#15](https://github.com/sunstoneinstitute/horndb/issues/15) compressed
    columnar warm tier (delivered 2026-05-31; <closed #1 | new ratio <R>× recorded on #1>);
```

- [ ] **Step 4: Verify the docs reference real numbers**

Run: `grep -n "<" docs/benchmarks.md docs/architecture.md TASKS.md | grep -E "<[A-Za-z]" || echo "no placeholders remain"`
Expected: `no placeholders remain` (every `<…>` placeholder replaced).

- [ ] **Step 5: Commit**

```bash
git add docs/benchmarks.md docs/architecture.md TASKS.md
git commit -m "docs(wcoj): record compressed four_cycle results; close #15 increment"
```

---

## Notes for the executor

- **Harness-first (SPEC-00):** this work adds a source implementation and a parity test; it does not change any conformance subset. The relevant gate is the `four_cycle` bench (SPEC-03 acceptance #2) — run it for real (Task 4).
- **The ratio may not reach ≥10×.** That is an expected, honest outcome — compression speeds both executors. The increment's value is the compressed source + the *measured* result. Record the real number either way; only close #1 if the gate is genuinely met.
- **Clippy is the CI gate.** After Task 5, run `cargo clippy -p horndb-wcoj --all-targets -- -D warnings` (full-workspace clippy runs in Phase 6 verification) and fix any lint before the PR.
- **Do not add dependencies.** The codec is hand-rolled by design (workspace has no bit-packing crate; adding one needs a workspace-deps change + review).
