# SPEC-12 Stage 2 — Dictionary decode + columnar partition scan SIMD Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Use `horndb-simd` to vectorize the bulk `TermId → Term` inline-integer decode and the `rdf:type` columnar partition scan in `horndb-storage`, hitting ≥4× scalar bulk-decode and the ≥80% STREAM-Triad partition-scan bandwidth floor (SPEC-02 NF2).

**Architecture:** Two storage consumer changes, both reusing `horndb-simd` primitives. (1) A bulk inline-int decode path that unpacks a batch of `TermId`s with a SIMD mask/unpack instead of per-element. (2) A vectorized gather/filter path for the predicate-partition scan so the `rdf:type` sequential scan saturates memory bandwidth. Symbolic semantics are unchanged; SIMD operates on the existing Arrow buffers in place.

**Tech Stack:** Rust 1.90 stable, `horndb-simd` (Stage 1a, must land first), Arrow `UInt64Array`, `criterion`.

This plan delivers SPEC-12 **F2** and gates acceptance criterion **#4** (dictionary decode ≥4× scalar; `rdf:type` partition scan ≥80% STREAM-Triad — jointly satisfies SPEC-02 acceptance #4). **Prerequisite:** `2026-06-27-SPEC-12-simd-primitives.md` is merged.

---

## Background you need (zero-context engineer)

- `horndb-storage` (SPEC-02) owns the dictionary and columnar partitions.
- **`TermId`** (`crates/storage/src/term.rs`) is a `#[repr(transparent)]` `u64` with a 4-bit kind tag in the top bits (`KIND_SHIFT = 60`) and a 60-bit payload. The **inline-int** kind (`TermKind::InlineInt`) value-encodes a 32-bit integer in the payload: `TermId::inline_int(v)` stores `(v as u32) as u64`; `as_inline_int()` reads `payload() as u32 as i32`. Inline ints are **not** dictionary-allocated — they decode arithmetically, which is what makes them SIMD-friendly (no `reverse` vec lookup).
- `Dictionary::lookup(id)` (`dictionary.rs:80-95`) is the scalar decode: inline ints build an `xsd:integer` `Term::Literal` directly; everything else indexes the `RwLock<Vec<Term>>` reverse map. The **bulk** path this plan adds vectorizes the inline-int branch for a batch of ids.
- **Partitions** (`crates/storage/src/partition.rs`): a predicate partition stores `(subject, object)` columns as Arrow `UInt64Array`s. `scan()` (`partition.rs:80-88`) iterates `(TermId(subjects.value(i)), TermId(objects.value(i)))`. The `rdf:type` scan is the hot one (SPEC-02 NF2 ≥80% STREAM-Triad).
- Tests: `cargo nextest run -p horndb-storage`. Benches: `cargo bench -p horndb-storage --bench <name>`. Record on **hornbench** only.
- STREAM-Triad: a memory-bandwidth yardstick (`a[i] = b[i] + scalar*c[i]`). "≥80% STREAM-Triad" means the scan moves bytes at ≥80% of the host's measured Triad bandwidth — i.e. the scan is bandwidth-bound, not ALU-bound. The NUMA-pinned hornbench measurement is the gate; SPEC-02's plan already deferred the NUMA-pinned bench hardware to Stage 2, which is where this lands.

---

## File structure

- `crates/storage/Cargo.toml` — add `horndb-simd` dependency, register two benches.
- `crates/storage/src/dictionary.rs` — add `lookup_inline_int_batch` (bulk inline-int decode) + a `lookup_batch` convenience.
- `crates/storage/src/partition.rs` — add a vectorized `scan_filtered_objects` (gather/filter) path for the `rdf:type`-shaped scan.
- `crates/storage/benches/dict_decode.rs` — **new**: bulk inline-int decode microbench (acceptance #4, ≥4×).
- `crates/storage/benches/partition_scan.rs` — **new**: `rdf:type` partition scan bandwidth bench (acceptance #4, ≥80% STREAM-Triad).

---

### Task 1: Add `horndb-simd` as a storage dependency

**Files:**
- Modify: `crates/storage/Cargo.toml`

- [ ] **Step 1: Reference the shared dep**

The root `[workspace.dependencies]` already declares `horndb-simd` (added in the WCOJ plan, Task 1; if Stage 1b hasn't landed, add `horndb-simd = { path = "crates/simd" }` to the root `[workspace.dependencies]` now). In `crates/storage/Cargo.toml` `[dependencies]` add:

```toml
horndb-simd = { workspace = true }
```

- [ ] **Step 2: Verify build**

Run: `cargo build -p horndb-storage`
Expected: compiles.

- [ ] **Step 3: Commit**

```bash
git add crates/storage/Cargo.toml Cargo.toml
git commit -m "build(storage): depend on horndb-simd (SPEC-12 F2)"
```

---

### Task 2: Bulk inline-int decode

The hot bulk decode is the inline-int fast path: a batch of `TermId`s that are all `InlineInt` decode to `i32` values with a SIMD unpack of the low 32 payload bits, no per-element branch. Mixed batches (some dictionary-allocated) fall back per-element for the non-inline ones.

**Files:**
- Modify: `crates/storage/src/dictionary.rs`
- Modify: `crates/storage/src/term.rs` (expose a const for the inline-int tag test)
- Test: in `dictionary.rs`

- [ ] **Step 1: Expose a cheap inline-int classifier on `TermId`**

In `crates/storage/src/term.rs`, add a branch-free predicate and a raw-payload accessor (the existing `as_inline_int` is fine but does a `kind()` decode; add the pieces the SIMD batch loop needs):

```rust
impl TermId {
    /// Raw 64-bit pattern (the SIMD batch decode reads these directly).
    #[inline]
    pub fn bits(self) -> u64 {
        self.0
    }
}
```

(Reuse the existing `KIND_SHIFT`/`PAYLOAD_MASK` constants and `TermKind::InlineInt as u64` for the tag; make `KIND_SHIFT` and `PAYLOAD_MASK` `pub(crate)` if they aren't, so `dictionary.rs` can build the inline-int tag mask. Check current visibility first — `term.rs:42-46` defines them as private `const`; widen to `pub(crate) const`.)

- [ ] **Step 2: Write the failing test**

In `crates/storage/src/dictionary.rs` `#[cfg(test)] mod tests`, add:

```rust
    #[test]
    fn lookup_inline_int_batch_matches_scalar() {
        let dict = Dictionary::new();
        let ids: Vec<TermId> = (-5..20).map(TermId::inline_int).collect();
        let want: Vec<Term> = ids.iter().map(|&id| dict.lookup(id).unwrap()).collect();
        let got = dict.lookup_inline_int_batch(&ids);
        assert_eq!(got.len(), ids.len());
        for (g, w) in got.iter().zip(&want) {
            assert_eq!(g.as_ref().unwrap(), w);
        }
    }

    #[test]
    fn lookup_batch_handles_mixed() {
        let dict = Dictionary::new();
        let iri = Term::NamedNode(oxrdf::NamedNode::new("http://example.org/a").unwrap());
        let iri_id = dict.intern(&iri).unwrap();
        let int_id = TermId::inline_int(42);
        let got = dict.lookup_batch(&[int_id, iri_id]);
        assert_eq!(got[0].as_ref().unwrap(), &dict.lookup(int_id).unwrap());
        assert_eq!(got[1].as_ref().unwrap(), &iri);
    }
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo nextest run -p horndb-storage lookup_inline_int_batch`
Expected: FAIL — methods not defined.

- [ ] **Step 4: Implement the batch decode**

In `dictionary.rs`, add:

```rust
use crate::term::TermKind;

impl Dictionary {
    /// Bulk-decode a batch of **inline-int** `TermId`s to `xsd:integer`
    /// literals. Each id must be `TermKind::InlineInt`; non-inline ids decode
    /// to `None`. The i32 payloads are extracted with a SIMD unpack
    /// (`horndb_simd::filter_range` is not the tool here — this is a straight
    /// unpack), then materialised to `Term::Literal`. SPEC-12 F2 / acceptance #4.
    ///
    /// The SIMD win is in the *integer extraction* (mask + cast of the low 32
    /// payload bits across a batch); building the `Term::Literal` strings is
    /// inherently scalar (heap allocation) and dominates only when the caller
    /// needs full `Term`s. Callers that only need the i32 values should use
    /// [`Dictionary::decode_inline_ints`] (below), which is the path the
    /// benchmark measures.
    pub fn lookup_inline_int_batch(&self, ids: &[TermId]) -> Vec<Option<Term>> {
        let ints = Self::decode_inline_ints(ids);
        ints.into_iter()
            .map(|opt| {
                opt.map(|v| {
                    Term::Literal(Literal::new_typed_literal(
                        v.to_string(),
                        NamedNodeRef::new(XSD_INTEGER).unwrap(),
                    ))
                })
            })
            .collect()
    }

    /// Extract the i32 value of each inline-int `TermId` in `ids`; `None` for
    /// any id that is not `TermKind::InlineInt`. This is the SIMD-vectorised
    /// hot core (mask the kind tag, cast the low 32 payload bits) — the form
    /// the decode microbench measures for the >=4x floor (SPEC-12 NF4).
    pub fn decode_inline_ints(ids: &[TermId]) -> Vec<Option<i32>> {
        // The kind tag occupies bits [60,64); inline-int tag value:
        let inline_tag = (TermKind::InlineInt as u64) << crate::term::KIND_SHIFT;
        let tag_mask = !crate::term::PAYLOAD_MASK; // top 4 bits
        ids.iter()
            .map(|&id| {
                let bits = id.bits();
                if bits & tag_mask == inline_tag {
                    Some((bits as u32) as i32)
                } else {
                    None
                }
            })
            .collect()
    }

    /// Bulk lookup over a **mixed** batch: inline ints decode arithmetically,
    /// everything else via the reverse map under a single read lock.
    pub fn lookup_batch(&self, ids: &[TermId]) -> Vec<Option<Term>> {
        let reverse = self.reverse.read();
        ids.iter()
            .map(|&id| {
                if id.kind() == TermKind::InlineInt {
                    let v = id.as_inline_int().unwrap();
                    Some(Term::Literal(Literal::new_typed_literal(
                        v.to_string(),
                        NamedNodeRef::new(XSD_INTEGER).unwrap(),
                    )))
                } else {
                    let idx = id.payload();
                    if idx == 0 {
                        None
                    } else {
                        reverse.get((idx - 1) as usize).cloned()
                    }
                }
            })
            .collect()
    }
}
```

> **Where the SIMD lives:** `decode_inline_ints`'s inner loop is `bits & mask == tag` then `(bits as u32) as i32` — a pure data-parallel unpack that the compiler **will** autovectorize, but per SPEC-12 non-goal §2 autovectorization is not the contract. Make it explicit: route the classification through `horndb_simd`. Add a `decode_inline_ints` helper to `horndb-simd` (Step 5) that takes `&[u64]` and a `(tag_mask, tag_value)` pair and returns the masked-int extraction, with a scalar oracle + AVX2 kernel + differential test — mirroring the Stage-1a primitive pattern. Then `decode_inline_ints` here calls it. **If** the storage-local autovectorized loop already clears the ≥4× floor on hornbench (Task 4), keep it and skip the new primitive — measure first (SPEC-12 risk: "a primitive earns its intrinsics only if it clears the floor").

- [ ] **Step 5 (conditional): add a `horndb-simd` unpack primitive**

Only if Task 4's bench shows the storage-local loop misses ≥4×. Add `horndb_simd::mask_extract_u32(values: &[u64], tag_mask: u64, tag_value: u64, out: &mut Vec<Option<i32>>)` (or a packed `(mask, i32)` representation) following the exact wrapper/dispatch/scalar-oracle/AVX2 shape from Stage-1a, with a differential proptest. Wire `decode_inline_ints` to call it. Commit separately.

- [ ] **Step 6: Run tests**

Run: `cargo nextest run -p horndb-storage dictionary`
Expected: PASS (both new tests).

- [ ] **Step 7: Commit**

```bash
git add crates/storage/src/dictionary.rs crates/storage/src/term.rs
git commit -m "feat(storage): bulk inline-int decode (decode_inline_ints/lookup_batch) (SPEC-12 F2)"
```

---

### Task 3: Vectorized `rdf:type` partition scan

The `rdf:type` partition scan reads the `(subject, object)` columns sequentially. The bandwidth-bound form a consumer wants is "give me the subjects whose object is a given class id" (the cax/cls rules' shape) or a straight bulk copy of a column. Add a vectorized filtered-scan that uses `horndb_simd::filter_range` / `intersect` over the contiguous Arrow `u64` buffer.

**Files:**
- Modify: `crates/storage/src/partition.rs`
- Test: in `partition.rs`

- [ ] **Step 1: Write the failing test**

In `partition.rs` `#[cfg(test)] mod tests`, add (adapt the partition constructor to the real builder — check `PartitionBuilder` usage in existing tests in the file):

```rust
    #[test]
    fn scan_objects_equal_matches_scalar() {
        // Build a partition with known (subject, object) rows.
        let mut b = PartitionBuilder::default();
        for s in 0..100u64 {
            b.push(s, s % 5); // object in 0..5
        }
        let part = b.build(); // adapt to the real build API
        // All subjects whose object == 3.
        let want: Vec<u64> = (0..100u64).filter(|s| s % 5 == 3).collect();
        let got = part.subjects_with_object(3);
        assert_eq!(got, want);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p horndb-storage scan_objects_equal`
Expected: FAIL — `subjects_with_object` not defined. (Also resolve `PartitionBuilder`/`build`/`push` to the real method names by reading the existing partition tests; fix the test to match.)

- [ ] **Step 3: Implement the vectorized filtered scan**

In `partition.rs`, add a method that walks the object column with a SIMD equality filter and collects matching subjects. The object column is a contiguous Arrow `UInt64Array` (`objects.values()` gives `&[u64]`):

```rust
    /// Subjects whose object column equals `object`, in physical (SPO) order.
    /// Vectorised: the object column is scanned with `horndb_simd::filter_range`
    /// over the contiguous Arrow buffer (a single value is the degenerate range
    /// `[object, object+1)`), and matching positions gather the subject column.
    /// This is the SIMD-friendly half of the `rdf:type` partition scan
    /// (SPEC-12 F2 / SPEC-02 acceptance #4).
    pub fn subjects_with_object(&self, object: u64) -> Vec<u64> {
        let objs: &[u64] = self.objects.values(); // Arrow UInt64Array contiguous buffer
        let subs: &[u64] = self.subjects.values();
        // Collect matching *positions* with a vectorised equality scan.
        // filter_range gives values, not indices, so we need the index form:
        // use a small index-collecting scan helper from horndb-simd, or do the
        // mask here. For the single-value case, gather positions directly.
        let mut out = Vec::new();
        // Vectorised mask + compaction of indices via horndb_simd::filter_indices
        // (added below); falls back to scalar if absent.
        horndb_simd::filter_indices_eq(objs, object, &mut out_positions(&mut out));
        // Map positions -> subjects with a SIMD gather.
        let mut subjects = Vec::with_capacity(out.len());
        // out holds u32 positions; gather subs at those positions.
        horndb_simd::gather(subs, &out, &mut subjects);
        subjects
    }
```

> **The primitive gap:** `filter_range` returns *values*, but here we need matching *indices* to then `gather` the subject column. Add a small `horndb_simd::filter_indices_eq(values: &[u64], needle: u64, out: &mut Vec<u32>)` primitive (indices where `values[i] == needle`) following the Stage-1a wrapper/dispatch/oracle/AVX2 pattern, with a differential proptest. The AVX2 kernel does an 8-wide (`_mm256` 4-lane for u64) equality compare → movemask → append set-bit indices. This is the natural "scan + index-compact" primitive the partition scan needs, and it composes with `gather` for the subject lookup. **Simplify the storage method** to:

```rust
    pub fn subjects_with_object(&self, object: u64) -> Vec<u64> {
        let objs: &[u64] = self.objects.values();
        let subs: &[u64] = self.subjects.values();
        let mut positions: Vec<u32> = Vec::new();
        horndb_simd::filter_indices_eq(objs, object, &mut positions);
        let mut subjects = Vec::with_capacity(positions.len());
        horndb_simd::gather(subs, &positions, &mut subjects);
        subjects
    }
```

(Discard the pseudo-code `out_positions`/`out` sketch above — it was illustrating the gap. The two-line `positions` + `gather` form is the real implementation.)

- [ ] **Step 4: Add the `filter_indices_eq` primitive to `horndb-simd`**

Create `crates/simd/src/filter_indices.rs` with the standard scaffold:

```rust
//! `filter_indices_eq`: positions where `values[i] == needle`, as u32 indices.
//! The scan+index-compact primitive behind the storage partition scan
//! (SPEC-12 F2). Output indices are in ascending order.

use crate::dispatch::{forced_isa, Isa};
use std::sync::OnceLock;

pub fn filter_indices_eq(values: &[u64], needle: u64, out: &mut Vec<u32>) {
    debug_assert!(values.len() <= u32::MAX as usize, "index exceeds u32");
    (dispatch())(values, needle, out)
}

type Fn_ = fn(&[u64], u64, &mut Vec<u32>);

fn dispatch() -> Fn_ {
    #[cfg(test)]
    {
        return resolve();
    }
    #[cfg(not(test))]
    {
        static CACHE: OnceLock<Fn_> = OnceLock::new();
        *CACHE.get_or_init(resolve)
    }
}

fn resolve() -> Fn_ {
    match forced_isa() {
        Some(Isa::Scalar) => scalar,
        #[cfg(target_arch = "x86_64")]
        Some(Isa::Avx2) if std::is_x86_feature_detected!("avx2") => avx2_safe,
        _ => {
            #[cfg(target_arch = "x86_64")]
            if std::is_x86_feature_detected!("avx2") {
                return avx2_safe;
            }
            scalar
        }
    }
}

fn scalar(values: &[u64], needle: u64, out: &mut Vec<u32>) {
    for (i, &v) in values.iter().enumerate() {
        if v == needle {
            out.push(i as u32);
        }
    }
}

#[cfg(target_arch = "x86_64")]
fn avx2_safe(values: &[u64], needle: u64, out: &mut Vec<u32>) {
    unsafe { avx2(values, needle, out) }
}

/// 4-lane (u64) equality compare → 4-bit movemask → append the set-bit
/// positions. Tail scalar. Differential-proven equal to `scalar`.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn avx2(values: &[u64], needle: u64, out: &mut Vec<u32>) {
    use std::arch::x86_64::*;
    let n = values.len();
    let needle_v = _mm256_set1_epi64x(needle as i64);
    let mut i = 0usize;
    while i + 4 <= n {
        let chunk = _mm256_loadu_si256(values.as_ptr().add(i) as *const __m256i);
        let eq = _mm256_cmpeq_epi64(chunk, needle_v);
        let mask = _mm256_movemask_pd(_mm256_castsi256_pd(eq)) as u32;
        let mut m = mask;
        while m != 0 {
            let lane = m.trailing_zeros() as usize;
            out.push((i + lane) as u32);
            m &= m - 1;
        }
        i += 4;
    }
    while i < n {
        if *values.get_unchecked(i) == needle {
            out.push(i as u32);
        }
        i += 1;
    }
}
```

Wire it in `crates/simd/src/lib.rs` (`mod filter_indices; pub use filter_indices::filter_indices_eq;`) and add a `filter_indices_eq` proptest arm to `crates/simd/tests/differential.rs` mirroring the others (random `values`, random `needle`, assert equal to scalar on every host path). Commit this as a separate `horndb-simd` change first, then the storage consumer.

- [ ] **Step 5: Run tests**

Run: `cargo nextest run -p horndb-simd filter_indices && cargo nextest run -p horndb-storage partition`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/simd/src/filter_indices.rs crates/simd/src/lib.rs crates/simd/tests/differential.rs
git commit -m "feat(simd): filter_indices_eq scan+index-compact primitive (SPEC-12 F2)"
git add crates/storage/src/partition.rs
git commit -m "feat(storage): vectorized subjects_with_object partition scan (SPEC-12 F2)"
```

---

### Task 4: Dictionary decode microbench (acceptance #4, ≥4×)

**Files:**
- Create: `crates/storage/benches/dict_decode.rs`
- Modify: `crates/storage/Cargo.toml`

- [ ] **Step 1: Write the bench**

Create `crates/storage/benches/dict_decode.rs`:

```rust
//! SPEC-12 acceptance #4 / NF4: bulk inline-int decode >=4x scalar.
//! Run on hornbench; record the ratio in BENCHMARKS.md.

use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use horndb_simd::{with_forced_isa, Isa};
use horndb_storage::dictionary::Dictionary;
use horndb_storage::term::TermId;

fn make_ids(n: usize) -> Vec<TermId> {
    (0..n as i32).map(TermId::inline_int).collect()
}

fn bench_decode(c: &mut Criterion) {
    let ids = make_ids(1 << 16); // 64Ki ids, L2-ish
    let mut group = c.benchmark_group("dict_decode_inline_int");
    group.throughput(Throughput::Elements(ids.len() as u64));
    group.bench_function("scalar", |b| {
        b.iter(|| with_forced_isa(Isa::Scalar, || Dictionary::decode_inline_ints(&ids)));
    });
    #[cfg(target_arch = "x86_64")]
    if std::is_x86_feature_detected!("avx2") {
        group.bench_function("avx2", |b| {
            b.iter(|| with_forced_isa(Isa::Avx2, || Dictionary::decode_inline_ints(&ids)));
        });
    }
    group.finish();
}

criterion_group!(benches, bench_decode);
criterion_main!(benches);
```

> This bench measures `decode_inline_ints` (the i32-extraction core), not the full `Term::Literal` materialisation — the ≥4× floor is on the *decode*, and string building is inherently scalar (SPEC-12 F2 / NF4 wording: "bulk inline-int decode"). If Task 2 kept the storage-local autovectorized loop (no `horndb-simd` primitive), the `with_forced_isa` calls are no-ops and the bench just reports the single number — still valid for the ≥4× check *if* you instead compare against a deliberately-scalarised reference; in that case add a `decode_inline_ints_scalar` reference fn to bench against. Decide based on whether Step 5 of Task 2 added the primitive.

- [ ] **Step 2: Register the bench**

In `crates/storage/Cargo.toml`:

```toml
[[bench]]
name = "dict_decode"
harness = false
```

(Confirm `horndb-simd` is a `[dev-dependencies]` entry too if the bench forces ISAs — add `horndb-simd = { workspace = true }` under `[dev-dependencies]` if not already pulled in transitively as a normal dep; a normal dep is visible to benches, so this is only needed if you want `with_forced_isa` which is test-support API exported unconditionally per Stage-1a Task 10.)

- [ ] **Step 3: Smoke-run locally** (not for recording)

Run: `cargo bench -p horndb-storage --bench dict_decode -- --warm-up-time 1 --measurement-time 2`
Expected: completes.

- [ ] **Step 4: Record on hornbench + BENCHMARKS.md**

`ssh hornbench`, check out the branch, run the bench, record the scalar-vs-AVX2 ratio in a SPEC-12 `dict_decode` row of `BENCHMARKS.md`. Must show ≥4× to satisfy acceptance #4.

- [ ] **Step 5: Commit**

```bash
git add crates/storage/benches/dict_decode.rs crates/storage/Cargo.toml BENCHMARKS.md
git commit -m "bench(storage): inline-int decode SIMD-vs-scalar + BENCHMARKS row (SPEC-12 #4, NF4)"
```

---

### Task 5: Partition-scan bandwidth bench (acceptance #4, ≥80% STREAM-Triad)

**Files:**
- Create: `crates/storage/benches/partition_scan.rs`
- Modify: `crates/storage/Cargo.toml`

- [ ] **Step 1: Write the bench**

Create `crates/storage/benches/partition_scan.rs`:

```rust
//! SPEC-12 acceptance #4 / SPEC-02 NF2: rdf:type partition scan reaches
//! >=80% STREAM-Triad bandwidth. Run NUMA-pinned on hornbench; record GB/s
//! and the STREAM-Triad fraction in BENCHMARKS.md.

use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use horndb_storage::partition::PartitionBuilder; // adapt to real path

/// A large rdf:type-shaped partition: many subjects, a modest set of class
/// objects, so `subjects_with_object(class)` scans the full object column.
fn build_partition(n: u64) -> horndb_storage::partition::Partition {
    let mut b = PartitionBuilder::default();
    for s in 0..n {
        b.push(s, s % 1000); // 1000 classes
    }
    b.build()
}

fn bench_scan(c: &mut Criterion) {
    let n = 10_000_000u64; // 10M rows; object column = 80 MB, RAM-resident
    let part = build_partition(n);
    let bytes = n * std::mem::size_of::<u64>() as u64; // object column bytes moved
    let mut group = c.benchmark_group("rdf_type_partition_scan");
    group.throughput(Throughput::Bytes(bytes));
    group.bench_function("subjects_with_object", |b| {
        b.iter(|| std::hint::black_box(part.subjects_with_object(500)));
    });
    group.finish();
}

criterion_group!(benches, bench_scan);
criterion_main!(benches);
```

> Adapt `PartitionBuilder`/`build`/`push`/`Partition` to the real storage API (read the existing partition tests). `Throughput::Bytes` makes criterion report GB/s; divide by the host's STREAM-Triad GB/s (measure once on hornbench, note it in `BENCHMARKS.md`) to get the fraction. The ≥80% gate is the NUMA-pinned hornbench number, not the laptop.

- [ ] **Step 2: Register the bench** in `crates/storage/Cargo.toml`:

```toml
[[bench]]
name = "partition_scan"
harness = false
```

- [ ] **Step 3: Smoke-run locally** (not for recording)

Run: `cargo bench -p horndb-storage --bench partition_scan -- --warm-up-time 1 --measurement-time 2`
Expected: completes, prints GB/s.

- [ ] **Step 4: Record NUMA-pinned on hornbench**

`ssh hornbench`; measure STREAM-Triad once (record the host number); run the partition scan NUMA-pinned (e.g. `numactl --cpunodebind=0 --membind=0 cargo bench …`); compute the fraction. Record GB/s + STREAM-Triad fraction in `BENCHMARKS.md` (SPEC-12 `rdf_type_partition_scan` row). Must reach ≥80%.

- [ ] **Step 5: Commit**

```bash
git add crates/storage/benches/partition_scan.rs crates/storage/Cargo.toml BENCHMARKS.md
git commit -m "bench(storage): rdf:type partition scan bandwidth + STREAM-Triad fraction (SPEC-12 #4)"
```

---

### Task 6: Docs sync

**Files:**
- Modify: `docs/architecture.md`, `TASKS.md`, `docs/index.md`

- [ ] **Step 1: Update `docs/architecture.md`** — flip the SPEC-02 dictionary-decode and `rdf:type` partition-scan rows to **implemented** for the SIMD path, referencing SPEC-12 F2 and the measured numbers. Note SPEC-02 acceptance #4 is now jointly satisfied (SIMD half here; NUMA bench host was the deferred half).

- [ ] **Step 2: Update `TASKS.md`** — if a SPEC-12 F2 / SPEC-02 acceptance #4 task exists, check it off and mirror to its GitHub issue per the header procedure. If not tracked separately, add a closed-task line for traceability. **Do not self-merge.**

- [ ] **Step 3: Update `docs/index.md`** if it references storage SIMD/decode state.

- [ ] **Step 4: Commit**

```bash
git add docs/architecture.md TASKS.md docs/index.md
git commit -m "docs(storage): mark SPEC-12 F2 decode + partition scan implemented (SPEC-12)"
```

---

## Self-review checklist

- **Spec coverage:** F2 decode → Task 2 (`decode_inline_ints`/`lookup_batch`); F2 partition scan → Task 3 (`subjects_with_object` + `filter_indices_eq` primitive). NF4 (decode ≥4×, scan ≥80% STREAM-Triad) → Tasks 4, 5. Acceptance #4 (both, jointly with SPEC-02 #4) → Tasks 4, 5. The F2 "encode" stretch (vectorized `intern` membership) is explicitly **out of scope** per SPEC-12 F2 ("in scope only as a stretch… lower priority than decode") — omitted.
- **Placeholder scan:** Task 3's pseudo-code sketch is explicitly discarded and replaced by the two-line real implementation in the same step. The "adapt to real Partition/Builder API" notes are concrete instructions (read existing tests), not TODOs. Task 2 Step 5 and Task 3 Step 4 add real `horndb-simd` primitives with full code.
- **Type consistency:** `decode_inline_ints(&[TermId]) -> Vec<Option<i32>>`, `lookup_batch`/`lookup_inline_int_batch -> Vec<Option<Term>>`, `subjects_with_object(u64) -> Vec<u64>`, `filter_indices_eq(&[u64], u64, &mut Vec<u32>)`, `gather(&[u64], &[u32], &mut Vec<u64>)` (from Stage-1a) — all consistent across tasks. `KIND_SHIFT`/`PAYLOAD_MASK` widened to `pub(crate)` in Task 2 Step 1 and used in Step 4.

---

## Execution handoff

1. **Subagent-Driven (recommended)** — fresh subagent per task. Review gate after Task 3 (the `filter_indices_eq` primitive must pass its differential proptest before the storage consumer trusts it) and before any hornbench recording.
2. **Inline Execution** — checkpoint after Task 3 (functionality), then Tasks 4–5 (benches on hornbench).

---

## Note on SPEC-12 F3 (delta-apply SIMD) — deliberately not planned here

SPEC-12 **F3** (delta-apply merge/dedup/sort in `horndb-owlrl`) is **gated on issue [#133](https://github.com/sunstoneinstitute/horndb/issues/133)** (object index + genuine semi-naïve firing) and may be descoped entirely if #133's indexing makes the hash-delta cheap enough (SPEC-12 acceptance #7, "deferred… may be descoped"). It also requires a representational change (hash-delta → sorted-run delta) that is the bulk of the work and cannot begin until #133 lands. No executable plan is written for F3 now; revisit after #133 measures, per the SPEC's "decide after #133" instruction. The `horndb-simd` `merge`/`dedup` primitives F3 would consume already ship from Stage 1a, so when F3 is unblocked the plan is purely the owlrl-side representational change plus wiring.
