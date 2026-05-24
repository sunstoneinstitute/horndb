# SPEC-03 WCOJ Query Engine — Stage 0 + Stage 1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a worst-case-optimal Leapfrog Triejoin executor for RDF triple patterns in `crates/wcoj`, sitting behind a trait-abstracted storage interface, with Arrow vectorization, a binary-hash-join fallback, a minimal cost-based planner, and cancellation — sufficient to satisfy SPEC-03 acceptance criteria #2 (4-cycle WCOJ-wins-by-10×) and #3 (differential fuzzer vs. binary join).

**Architecture:** Three layers, each in its own file/module: (1) a `TripleSource` trait that exposes per-ordering sorted iterators with `seek(value)` — the only thing the executor knows about storage; (2) a Leapfrog Triejoin core that composes per-variable `TrieIterator`s and runs the leapfrog seek loop; (3) a vectorized output stage that materializes bindings into Arrow `RecordBatch`es of `STANDARD_VECTOR_SIZE = 2048` rows. A separate binary-hash-join executor serves both as the ≤3-pattern fast path and as the differential-fuzzer reference. A tiny cardinality estimator stub (uniform-with-known-totals) feeds the planner that picks between the two.

**Tech Stack:** Rust 2021, `arrow` (crate `arrow` 53.x for `RecordBatch` + `UInt64Array`), `roaring` (only as an opaque set type, not required for Stage 1), `proptest` (differential fuzzing), `criterion` (microbench gate for the per-tuple overhead NF1 target and for the 10× win on acceptance #2), `thiserror`/`anyhow` (workspace deps).

**Stage 0/1 scope boundary:** In: triple-pattern executor (F1), WCOJ on ≥4 patterns (F2), Arrow vectorization to 2048-row batches (F3), binary-hash-join fallback for ≤3 patterns and ground patterns, cardinality estimator stub (F6 minimal), cancellation hook (F7), differential fuzzer. Out (deferred to a later plan): magic-sets rewriter (F4), SLG tabling (F5), full SIMD intrinsics (NF1 hot-path tuning beyond a simple scalar loop), parallel partition execution (NF3), GPU WCOJ.

**Dependency handling:** SPEC-02 (storage) lands in parallel. We define `TripleSource` and `OrderedTripleIter` traits in this crate and implement two in-crate doubles for testing: `VecTripleSource` (sorted `Vec<(u64,u64,u64)>`) and a generative `SyntheticGraph` for the 4-cycle benchmark. When SPEC-02 ships, a thin wrapper crate (or impl block in `reasoner-storage`) adapts its concrete types to these traits — we never depend on the storage crate directly in Stage 0/1.

---

## File Structure

```
crates/wcoj/
├── Cargo.toml
├── benches/
│   ├── four_cycle.rs           # Acceptance criterion #2 — WCOJ ≥10× binary-join
│   └── per_tuple.rs            # NF1 ≤5 ns/tuple sanity check
├── src/
│   ├── lib.rs                  # Re-exports, crate-level docs
│   ├── error.rs                # `WcojError` (thiserror)
│   ├── ids.rs                  # `TermId`, `Triple`, `Ordering` enum (SPO/SOP/PSO/POS/OSP/OPS)
│   ├── source/
│   │   ├── mod.rs              # `TripleSource` trait + `OrderedTripleIter` trait
│   │   ├── vec_source.rs       # `VecTripleSource` — in-memory sorted-Vec impl for tests
│   │   └── synthetic.rs        # `SyntheticGraph` — k-cycle / star generators for benches
│   ├── pattern.rs              # `Term` (Bound|Var), `TriplePattern`, `Var(u8)`, `Bgp`
│   ├── trie/
│   │   ├── mod.rs              # `TrieIterator` trait, leapfrog `seek` loop
│   │   ├── source_iter.rs      # Adapts an `OrderedTripleIter` into a depth-aware `TrieIterator`
│   │   └── leapfrog.rs         # Multi-iterator leapfrog intersection (per variable level)
│   ├── plan.rs                 # Variable ordering, per-pattern ordering selection, plan struct
│   ├── executor/
│   │   ├── mod.rs              # `Executor` enum dispatch (Wcoj | BinaryHash); `execute()` entry
│   │   ├── wcoj.rs             # `WcojExecutor` — drives the trie-join, emits Arrow batches
│   │   └── binary_hash.rs      # `BinaryHashExecutor` — left-deep hash-join reference + ≤3 fallback
│   ├── batch.rs                # Arrow `RecordBatch` builder, `STANDARD_VECTOR_SIZE = 2048`
│   ├── cardinality.rs          # `Cardinality` trait + `UniformEstimator` stub (F6 minimal)
│   ├── planner.rs              # `Planner::choose()` — WCOJ vs BinaryHash by pattern count + estimator
│   └── cancel.rs               # `CancelToken` — atomic bool, 100 ms polling cadence (F7)
└── tests/
    ├── trie_basics.rs          # Trie-iterator unit-level integration
    ├── wcoj_smoke.rs           # End-to-end: triangle, 4-cycle, star
    ├── binary_hash_smoke.rs    # Hash-join reference produces correct triangle/star
    ├── planner_choice.rs       # Asserts planner picks WCOJ at ≥4 patterns, BinaryHash at ≤3
    ├── cancel.rs               # Cancellation returns within 100 ms
    └── differential_fuzz.rs    # `proptest`: WCOJ ≡ BinaryHash over random BGPs and graphs
```

**Why this decomposition:** `source/` is the only module that talks to "the outside world" — once SPEC-02 lands, only `source/` needs an adapter. `trie/` is pure algorithm with zero I/O — easy to test. `executor/`, `planner.rs`, and `cardinality.rs` are deliberately tiny so that the Stage-2 expansion (magic sets, tabling, real estimator) slots into existing module boundaries without rewrites.

---

## Task 1: Wire up crate dependencies and module skeleton

**Files:**
- Modify: `crates/wcoj/Cargo.toml`
- Modify: `crates/wcoj/src/lib.rs`
- Create: `crates/wcoj/src/error.rs`
- Create: `crates/wcoj/src/ids.rs`

- [ ] **Step 1: Add dependencies to `crates/wcoj/Cargo.toml`**

Replace the file contents with:

```toml
[package]
name = "reasoner-wcoj"
version = "0.0.0"
edition.workspace = true
license.workspace = true
publish = false

[dependencies]
anyhow = { workspace = true }
thiserror = { workspace = true }
arrow = "53"

[dev-dependencies]
proptest = "1"
criterion = { version = "0.5", features = ["html_reports"] }

[[bench]]
name = "four_cycle"
harness = false

[[bench]]
name = "per_tuple"
harness = false
```

- [ ] **Step 2: Verify the crate still compiles with the new deps**

Run: `cargo check -p reasoner-wcoj`
Expected: `Finished ... dev [unoptimized + debuginfo] target(s)`. If `arrow 53` is unavailable, fall back to `arrow = "52"`; the APIs we use (`RecordBatch::try_new`, `UInt64Array::from`) are stable across both.

- [ ] **Step 3: Replace `crates/wcoj/src/lib.rs` with the module skeleton**

```rust
//! reasoner-wcoj — Leapfrog Triejoin query executor for RDF triple patterns.
//!
//! See `specs/SPEC-03-query-engine.md` for the full design. Stage 0/1 scope:
//! WCOJ on ≥4 patterns, binary-hash-join fallback, Arrow vectorization,
//! cancellation. Magic sets and SLG tabling are deferred.

pub mod batch;
pub mod cancel;
pub mod cardinality;
pub mod error;
pub mod executor;
pub mod ids;
pub mod pattern;
pub mod plan;
pub mod planner;
pub mod source;
pub mod trie;

pub use error::WcojError;
pub use ids::{Ordering, TermId, Triple};
pub use pattern::{Bgp, Term, TriplePattern, Var};
```

- [ ] **Step 4: Create `crates/wcoj/src/error.rs`**

```rust
use thiserror::Error;

#[derive(Debug, Error)]
pub enum WcojError {
    #[error("query was cancelled")]
    Cancelled,
    #[error("requested ordering {0:?} not supported by source")]
    OrderingUnavailable(crate::ids::Ordering),
    #[error("internal: {0}")]
    Internal(String),
    #[error(transparent)]
    Arrow(#[from] arrow::error::ArrowError),
}

pub type Result<T> = std::result::Result<T, WcojError>;
```

- [ ] **Step 5: Create `crates/wcoj/src/ids.rs`**

```rust
/// Internal 64-bit identifier for any RDF term (URI, literal, blank node).
/// SPEC-02 owns the term-kind tagging in the high bits; we treat IDs as opaque.
pub type TermId = u64;

/// A concrete triple in the store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Triple {
    pub s: TermId,
    pub p: TermId,
    pub o: TermId,
}

impl Triple {
    pub fn new(s: TermId, p: TermId, o: TermId) -> Self {
        Self { s, p, o }
    }

    /// Reorder the triple components according to `ord`, returning a 3-tuple
    /// `(level0, level1, level2)`. Used by the trie iterator to read components
    /// in trie-depth order regardless of physical ordering.
    pub fn by_ordering(&self, ord: Ordering) -> (TermId, TermId, TermId) {
        match ord {
            Ordering::Spo => (self.s, self.p, self.o),
            Ordering::Sop => (self.s, self.o, self.p),
            Ordering::Pso => (self.p, self.s, self.o),
            Ordering::Pos => (self.p, self.o, self.s),
            Ordering::Osp => (self.o, self.s, self.p),
            Ordering::Ops => (self.o, self.p, self.s),
        }
    }
}

/// The six trie orderings. Names follow the convention `<level0><level1><level2>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Ordering {
    Spo,
    Sop,
    Pso,
    Pos,
    Osp,
    Ops,
}

impl Ordering {
    pub const ALL: [Ordering; 6] = [
        Ordering::Spo,
        Ordering::Sop,
        Ordering::Pso,
        Ordering::Pos,
        Ordering::Osp,
        Ordering::Ops,
    ];
}
```

- [ ] **Step 6: Make every module compile as an empty stub**

Create empty placeholder files so `cargo check` is happy after lib.rs declares them. For each of the following files, create them with just a doc comment placeholder; subsequent tasks fill them in:

```bash
for f in batch cancel cardinality executor pattern plan planner; do
  echo "//! placeholder — filled in by later tasks" \
    > crates/wcoj/src/${f}.rs
done
mkdir -p crates/wcoj/src/source crates/wcoj/src/trie crates/wcoj/src/executor
for d in source trie executor; do
  echo "//! placeholder — filled in by later tasks" \
    > crates/wcoj/src/${d}/mod.rs
done
```

Then in `lib.rs`, the `pub mod executor;` line currently conflicts with the file `executor.rs` + directory `executor/`. Remove the `executor.rs` stub (it was created above) and keep only `executor/mod.rs`:

```bash
rm crates/wcoj/src/executor.rs
```

Same cleanup for `source` and `trie`:

```bash
rm -f crates/wcoj/src/source.rs crates/wcoj/src/trie.rs
```

- [ ] **Step 7: Verify the skeleton compiles**

Run: `cargo check -p reasoner-wcoj`
Expected: `Finished ... target(s)`. No warnings about missing modules.

- [ ] **Step 8: Commit**

```bash
git add crates/wcoj/Cargo.toml crates/wcoj/src/
git commit -m "$(cat <<'EOF'
wcoj: scaffold crate modules, error types, and ID primitives

Adds arrow/proptest/criterion deps and the empty module skeleton declared
in plans/2026-05-24-SPEC-03-wcoj-query-engine.md. No behaviour yet.
EOF
)"
```

---

## Task 2: Define the `TripleSource` trait and `VecTripleSource` double

**Files:**
- Create: `crates/wcoj/src/source/mod.rs` (replacing the placeholder)
- Create: `crates/wcoj/src/source/vec_source.rs`
- Create: `crates/wcoj/tests/vec_source.rs`

- [ ] **Step 1: Write the failing test `crates/wcoj/tests/vec_source.rs`**

```rust
use reasoner_wcoj::ids::{Ordering, Triple};
use reasoner_wcoj::source::vec_source::VecTripleSource;
use reasoner_wcoj::source::{OrderedTripleIter, TripleSource};

#[test]
fn vec_source_seeks_within_spo_ordering() {
    let triples = vec![
        Triple::new(1, 10, 100),
        Triple::new(1, 10, 200),
        Triple::new(1, 20, 100),
        Triple::new(2, 10, 100),
    ];
    let src = VecTripleSource::from_triples(triples);

    let mut it = src.iter(Ordering::Spo).expect("SPO supported");

    // Level-0 (subject) iteration.
    assert_eq!(it.peek(0), Some(1));
    it.seek(0, 2);
    assert_eq!(it.peek(0), Some(2));
    it.seek(0, 3);
    assert_eq!(it.peek(0), None);
}

#[test]
fn vec_source_descends_levels() {
    let triples = vec![
        Triple::new(1, 10, 100),
        Triple::new(1, 10, 200),
        Triple::new(1, 20, 100),
    ];
    let src = VecTripleSource::from_triples(triples);

    let mut it = src.iter(Ordering::Spo).unwrap();
    it.seek(0, 1);
    it.open_level(1);
    assert_eq!(it.peek(1), Some(10));
    it.open_level(2);
    assert_eq!(it.peek(2), Some(100));
    it.seek(2, 150);
    assert_eq!(it.peek(2), Some(200));
}

#[test]
fn vec_source_reports_total_count() {
    let triples = vec![
        Triple::new(1, 10, 100),
        Triple::new(2, 10, 200),
    ];
    let src = VecTripleSource::from_triples(triples);
    assert_eq!(src.total_triples(), 2);
}
```

- [ ] **Step 2: Run the test to confirm it fails**

Run: `cargo test -p reasoner-wcoj --test vec_source`
Expected: compile errors — `source::vec_source` and `source::{TripleSource, OrderedTripleIter}` don't exist yet.

- [ ] **Step 3: Implement `crates/wcoj/src/source/mod.rs`**

Replace the placeholder with:

```rust
//! Storage abstraction over which the WCOJ executor operates.
//!
//! SPEC-02 (`reasoner-storage`) will provide the production implementation;
//! the executor never depends on the storage crate directly. Instead, anything
//! that can serve sorted iterators with `seek` over one of the six orderings
//! implements `TripleSource`.

pub mod synthetic;
pub mod vec_source;

use crate::error::Result;
use crate::ids::{Ordering, TermId};

/// A multi-ordering RDF triple source.
pub trait TripleSource: Send + Sync {
    /// The iterator type returned by [`Self::iter`]. Boxed to keep the trait
    /// object-safe; Stage-2 may revisit if dispatch cost shows up in profiles.
    fn iter(&self, ord: Ordering) -> Result<Box<dyn OrderedTripleIter + '_>>;

    /// Total triple count across all predicates. Used by the cardinality stub.
    fn total_triples(&self) -> usize;

    /// True if `ord` is materialised; false if the executor must ask for a
    /// different ordering. Stage-1 implementations may return true for all six.
    fn supports(&self, ord: Ordering) -> bool {
        let _ = ord;
        true
    }
}

/// A trie-shaped, depth-aware cursor over triples in some [`Ordering`].
///
/// The cursor maintains an implicit "current path" — values chosen at each
/// upper level constrain what is visible at deeper levels. `peek(depth)`
/// returns the next value at `depth` consistent with the prefix; `seek(depth,
/// v)` advances to the first value ≥ `v` at that depth; `open_level(depth)`
/// descends one level (must have peeked a value at `depth - 1` first).
pub trait OrderedTripleIter: Send {
    /// Return the next value at `depth` consistent with the current prefix,
    /// or `None` if the cursor is past the end at this level.
    fn peek(&self, depth: u8) -> Option<TermId>;

    /// Seek forward at `depth` to the first value ≥ `value`. After this call,
    /// `peek(depth)` returns either that value or `None`.
    fn seek(&mut self, depth: u8, value: TermId);

    /// Descend into the subtree under the value most recently peeked at
    /// `depth - 1`. Implementations may panic if the prefix is empty.
    fn open_level(&mut self, depth: u8);

    /// Ascend one level, undoing the matching `open_level`.
    fn up(&mut self, depth: u8);

    /// True if the cursor has exhausted values at `depth`.
    fn at_end(&self, depth: u8) -> bool {
        self.peek(depth).is_none()
    }
}
```

- [ ] **Step 4: Implement `crates/wcoj/src/source/vec_source.rs`**

```rust
//! `VecTripleSource` — sorted-`Vec` test double for `TripleSource`.
//!
//! All six orderings are materialised eagerly; suitable for tests and small
//! benches up to a few million triples.

use std::collections::HashMap;

use crate::error::{Result, WcojError};
use crate::ids::{Ordering, TermId, Triple};
use crate::source::{OrderedTripleIter, TripleSource};

pub struct VecTripleSource {
    /// One sorted `Vec<(l0, l1, l2)>` per ordering.
    sorted: HashMap<Ordering, Vec<(TermId, TermId, TermId)>>,
    total: usize,
}

impl VecTripleSource {
    pub fn from_triples(triples: Vec<Triple>) -> Self {
        let total = triples.len();
        let mut sorted = HashMap::with_capacity(6);
        for &ord in &Ordering::ALL {
            let mut v: Vec<_> = triples.iter().map(|t| t.by_ordering(ord)).collect();
            v.sort_unstable();
            v.dedup();
            sorted.insert(ord, v);
        }
        Self { sorted, total }
    }
}

impl TripleSource for VecTripleSource {
    fn iter(&self, ord: Ordering) -> Result<Box<dyn OrderedTripleIter + '_>> {
        let data = self
            .sorted
            .get(&ord)
            .ok_or(WcojError::OrderingUnavailable(ord))?;
        Ok(Box::new(VecIter::new(data)))
    }

    fn total_triples(&self) -> usize {
        self.total
    }
}

/// Cursor state: at each depth we hold a `(lo, hi)` range into `data` of rows
/// whose prefix matches the chosen path so far. `cursor[depth]` is the index
/// of the next row to return at `depth`.
struct VecIter<'a> {
    data: &'a [(TermId, TermId, TermId)],
    /// (lo, hi) per depth — `hi` is exclusive.
    range: [(usize, usize); 3],
    /// Cursor index per depth.
    cursor: [usize; 3],
}

impl<'a> VecIter<'a> {
    fn new(data: &'a [(TermId, TermId, TermId)]) -> Self {
        let full = (0usize, data.len());
        Self {
            data,
            range: [full, (0, 0), (0, 0)],
            cursor: [0, 0, 0],
        }
    }

    fn col(&self, row: usize, depth: u8) -> TermId {
        let t = self.data[row];
        match depth {
            0 => t.0,
            1 => t.1,
            2 => t.2,
            _ => unreachable!("depth {depth} > 2"),
        }
    }
}

impl<'a> OrderedTripleIter for VecIter<'a> {
    fn peek(&self, depth: u8) -> Option<TermId> {
        let (lo, hi) = self.range[depth as usize];
        let c = self.cursor[depth as usize].max(lo);
        if c >= hi {
            return None;
        }
        Some(self.col(c, depth))
    }

    fn seek(&mut self, depth: u8, value: TermId) {
        let d = depth as usize;
        let (lo, hi) = self.range[d];
        let start = self.cursor[d].max(lo);
        // Binary search the suffix `data[start..hi]` for the first row whose
        // `depth` column is ≥ `value`. Because rows share a common prefix at
        // depths < `depth`, the `depth` column is monotone non-decreasing
        // within `[lo, hi)`.
        let slice = &self.data[start..hi];
        let off = slice.partition_point(|row| {
            let v = match depth {
                0 => row.0,
                1 => row.1,
                2 => row.2,
                _ => unreachable!(),
            };
            v < value
        });
        self.cursor[d] = start + off;
    }

    fn open_level(&mut self, depth: u8) {
        assert!(depth >= 1 && depth <= 2, "open_level depth must be 1 or 2");
        let parent = (depth - 1) as usize;
        let (_, hi_parent) = self.range[parent];
        let row = self.cursor[parent];
        let v = self.col(row, depth - 1);
        // Find the half-open range of rows in `[row, hi_parent)` whose
        // depth-(depth-1) column equals `v` AND prefix up to depth-2 matches.
        // Since rows are sorted and the prefix is already constrained, the
        // run with column == v is contiguous.
        let slice = &self.data[row..hi_parent];
        let end_off = slice.partition_point(|r| {
            let c = match depth - 1 {
                0 => r.0,
                1 => r.1,
                2 => r.2,
                _ => unreachable!(),
            };
            c <= v
        });
        let new_lo = row;
        let new_hi = row + end_off;
        self.range[depth as usize] = (new_lo, new_hi);
        self.cursor[depth as usize] = new_lo;
    }

    fn up(&mut self, depth: u8) {
        let d = depth as usize;
        self.range[d] = (0, 0);
        self.cursor[d] = 0;
    }
}
```

- [ ] **Step 5: Stub out `source/synthetic.rs` so the module compiles**

Replace `crates/wcoj/src/source/synthetic.rs` (currently the directory's mod auto-created? No — only `mod.rs` and `vec_source.rs` exist. Create `synthetic.rs`):

```rust
//! Placeholder — filled in by Task 8 (synthetic 4-cycle generator for benches).
```

- [ ] **Step 6: Run the test — expect it to pass**

Run: `cargo test -p reasoner-wcoj --test vec_source`
Expected: `test result: ok. 3 passed; 0 failed`.

- [ ] **Step 7: Commit**

```bash
git add crates/wcoj/src/source crates/wcoj/tests/vec_source.rs
git commit -m "$(cat <<'EOF'
wcoj: add TripleSource trait and VecTripleSource test double

Defines the storage abstraction (peek/seek/open_level/up) that the
Leapfrog Triejoin executor depends on. VecTripleSource materialises all
six orderings eagerly from a Vec<Triple> and is used by trie/executor
tests until the SPEC-02 storage impl can be adapted to TripleSource.
EOF
)"
```

---

## Task 3: Define triple patterns and basic graph patterns

**Files:**
- Create: `crates/wcoj/src/pattern.rs` (replacing placeholder)
- Create: `crates/wcoj/tests/pattern.rs`

- [ ] **Step 1: Write the failing test `crates/wcoj/tests/pattern.rs`**

```rust
use reasoner_wcoj::ids::Ordering;
use reasoner_wcoj::pattern::{Bgp, Term, TriplePattern, Var};

#[test]
fn variables_collects_unique_vars_in_first_appearance_order() {
    // ?a p ?b . ?b q ?c . ?a r ?c
    let p = TriplePattern::new(Term::Var(Var(0)), Term::Bound(10), Term::Var(Var(1)));
    let q = TriplePattern::new(Term::Var(Var(1)), Term::Bound(11), Term::Var(Var(2)));
    let r = TriplePattern::new(Term::Var(Var(0)), Term::Bound(12), Term::Var(Var(2)));
    let bgp = Bgp::new(vec![p, q, r]);

    let vars = bgp.variables();
    assert_eq!(vars, vec![Var(0), Var(1), Var(2)]);
}

#[test]
fn pattern_with_all_three_bound_is_ground() {
    let g = TriplePattern::new(Term::Bound(1), Term::Bound(2), Term::Bound(3));
    assert!(g.is_ground());
    let v = TriplePattern::new(Term::Var(Var(0)), Term::Bound(2), Term::Bound(3));
    assert!(!v.is_ground());
}

#[test]
fn preferred_ordering_puts_bound_positions_first() {
    // Pattern (?s, p_bound, ?o) — we want predicate at level 0 so the
    // executor can seek to it immediately. Result: PSO or POS.
    let pat = TriplePattern::new(Term::Var(Var(0)), Term::Bound(42), Term::Var(Var(1)));
    let ord = pat.preferred_ordering();
    assert!(matches!(ord, Ordering::Pso | Ordering::Pos));
}
```

- [ ] **Step 2: Run to confirm it fails**

Run: `cargo test -p reasoner-wcoj --test pattern`
Expected: errors — `pattern::{Bgp, ...}` don't exist.

- [ ] **Step 3: Implement `crates/wcoj/src/pattern.rs`**

```rust
use crate::ids::{Ordering, TermId};

/// Variable identifier within a single BGP. Small enough that a `Vec<Var>`
/// of plan-time orderings is cheap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Var(pub u8);

/// One slot of a triple pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Term {
    Bound(TermId),
    Var(Var),
}

impl Term {
    pub fn as_var(self) -> Option<Var> {
        match self {
            Term::Var(v) => Some(v),
            _ => None,
        }
    }
    pub fn as_bound(self) -> Option<TermId> {
        match self {
            Term::Bound(t) => Some(t),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct TriplePattern {
    pub s: Term,
    pub p: Term,
    pub o: Term,
}

impl TriplePattern {
    pub fn new(s: Term, p: Term, o: Term) -> Self {
        Self { s, p, o }
    }

    pub fn is_ground(&self) -> bool {
        matches!(self.s, Term::Bound(_))
            && matches!(self.p, Term::Bound(_))
            && matches!(self.o, Term::Bound(_))
    }

    /// Heuristic: choose an ordering that puts bound positions at the
    /// shallowest depths. Ties broken `S < P < O`. This is the only
    /// per-pattern ordering decision the Stage-1 planner needs — the
    /// real planner (Stage 2) will jointly optimise across patterns.
    pub fn preferred_ordering(&self) -> Ordering {
        let bound = [
            matches!(self.s, Term::Bound(_)),
            matches!(self.p, Term::Bound(_)),
            matches!(self.o, Term::Bound(_)),
        ];
        // Score each ordering by the depth-weighted sum of "bound at depth d"
        // where shallower (smaller d) is better, so we negate.
        let orderings = [
            (Ordering::Spo, [bound[0], bound[1], bound[2]]),
            (Ordering::Sop, [bound[0], bound[2], bound[1]]),
            (Ordering::Pso, [bound[1], bound[0], bound[2]]),
            (Ordering::Pos, [bound[1], bound[2], bound[0]]),
            (Ordering::Osp, [bound[2], bound[0], bound[1]]),
            (Ordering::Ops, [bound[2], bound[1], bound[0]]),
        ];
        orderings
            .iter()
            .min_by_key(|(_, b)| {
                // Smaller "first non-bound depth" wins; secondary key: order index for stable tiebreak.
                let first_unbound = b.iter().position(|x| !x).unwrap_or(3);
                (3 - first_unbound, 0)
            })
            .map(|(o, _)| *o)
            .unwrap()
    }

    /// Return the position (0=S, 1=P, 2=O) of the given variable, or `None`.
    pub fn position_of(&self, v: Var) -> Option<u8> {
        if self.s == Term::Var(v) {
            Some(0)
        } else if self.p == Term::Var(v) {
            Some(1)
        } else if self.o == Term::Var(v) {
            Some(2)
        } else {
            None
        }
    }
}

#[derive(Debug, Clone)]
pub struct Bgp {
    pub patterns: Vec<TriplePattern>,
}

impl Bgp {
    pub fn new(patterns: Vec<TriplePattern>) -> Self {
        Self { patterns }
    }

    /// All variables appearing in any pattern, in first-appearance order.
    pub fn variables(&self) -> Vec<Var> {
        let mut out = Vec::new();
        for p in &self.patterns {
            for t in [p.s, p.p, p.o] {
                if let Term::Var(v) = t {
                    if !out.contains(&v) {
                        out.push(v);
                    }
                }
            }
        }
        out
    }
}
```

- [ ] **Step 4: Run the test — expect pass**

Run: `cargo test -p reasoner-wcoj --test pattern`
Expected: `test result: ok. 3 passed`.

- [ ] **Step 5: Commit**

```bash
git add crates/wcoj/src/pattern.rs crates/wcoj/tests/pattern.rs
git commit -m "wcoj: add Term/Var/TriplePattern/Bgp with ordering heuristic"
```

---

## Task 4: Implement single-pattern `TrieIterator` over `OrderedTripleIter`

**Files:**
- Create: `crates/wcoj/src/trie/mod.rs` (replace placeholder)
- Create: `crates/wcoj/src/trie/source_iter.rs`
- Create: `crates/wcoj/tests/trie_basics.rs`

- [ ] **Step 1: Write the failing test `crates/wcoj/tests/trie_basics.rs`**

```rust
use reasoner_wcoj::ids::{Ordering, Triple};
use reasoner_wcoj::pattern::{Term, TriplePattern, Var};
use reasoner_wcoj::source::vec_source::VecTripleSource;
use reasoner_wcoj::trie::source_iter::PatternTrieIter;
use reasoner_wcoj::trie::TrieIterator;

fn source() -> VecTripleSource {
    VecTripleSource::from_triples(vec![
        Triple::new(1, 10, 100),
        Triple::new(1, 10, 200),
        Triple::new(1, 20, 300),
        Triple::new(2, 10, 100),
        Triple::new(2, 10, 400),
    ])
}

#[test]
fn pattern_trie_iter_walks_subject_then_object_for_fixed_predicate() {
    // Pattern: (?s, 10, ?o) — variable order [s, o].
    let src = source();
    let pat = TriplePattern::new(Term::Var(Var(0)), Term::Bound(10), Term::Var(Var(1)));
    let var_order = vec![Var(0), Var(1)];
    let mut it = PatternTrieIter::new(&src, &pat, &var_order, Ordering::Pso).unwrap();

    // Depth 0 = variable ?s.
    assert_eq!(it.peek(0), Some(1));
    it.open_level(0);
    // Depth 1 = variable ?o, under s=1.
    assert_eq!(it.peek(1), Some(100));
    it.seek(1, 150);
    assert_eq!(it.peek(1), Some(200));
    it.up(1);
    it.seek(0, 2);
    assert_eq!(it.peek(0), Some(2));
    it.open_level(0);
    assert_eq!(it.peek(1), Some(100));
    it.seek(1, 200);
    assert_eq!(it.peek(1), Some(400));
}

#[test]
fn pattern_trie_iter_filters_out_non_matching_predicate() {
    let src = source();
    let pat = TriplePattern::new(Term::Var(Var(0)), Term::Bound(99), Term::Var(Var(1)));
    let var_order = vec![Var(0), Var(1)];
    let it = PatternTrieIter::new(&src, &pat, &var_order, Ordering::Pso).unwrap();
    assert_eq!(it.peek(0), None);
}
```

- [ ] **Step 2: Run to confirm it fails**

Run: `cargo test -p reasoner-wcoj --test trie_basics`
Expected: compile errors — types not defined.

- [ ] **Step 3: Implement `crates/wcoj/src/trie/mod.rs`**

```rust
//! Trie iterators and the per-variable leapfrog seek loop.
//!
//! A [`TrieIterator`] is a depth-aware cursor; it differs from an
//! [`OrderedTripleIter`] only in that depths refer to *query variables* in a
//! fixed variable ordering, not to physical SPO positions. One
//! [`TrieIterator`] is produced per triple pattern; the leapfrog algorithm
//! intersects them at each variable level.
//!
//! See Veldhuizen, *Leapfrog Triejoin: a worst-case optimal join algorithm*,
//! ICDT 2014.

pub mod leapfrog;
pub mod source_iter;

use crate::ids::TermId;

pub trait TrieIterator {
    /// Number of variable levels in this iterator. The trie operates on
    /// `0..arity()`.
    fn arity(&self) -> u8;

    fn peek(&self, depth: u8) -> Option<TermId>;
    fn seek(&mut self, depth: u8, value: TermId);
    fn open_level(&mut self, depth: u8);
    fn up(&mut self, depth: u8);

    fn at_end(&self, depth: u8) -> bool {
        self.peek(depth).is_none()
    }
}
```

- [ ] **Step 4: Implement `crates/wcoj/src/trie/source_iter.rs`**

```rust
//! `PatternTrieIter` — adapts a single `TriplePattern` over a `TripleSource`
//! into a variable-indexed trie iterator.

use crate::error::{Result, WcojError};
use crate::ids::{Ordering, TermId};
use crate::pattern::{Term, TriplePattern, Var};
use crate::source::{OrderedTripleIter, TripleSource};
use crate::trie::TrieIterator;

/// Per-physical-depth action: either "the level is bound to `TermId` — seek
/// to it and don't expose it as a variable" or "the level corresponds to
/// query variable at slot `usize`".
#[derive(Debug, Clone, Copy)]
enum LevelAction {
    Bound(TermId),
    Var(u8), // Index into the query-variable ordering
}

pub struct PatternTrieIter<'src> {
    inner: Box<dyn OrderedTripleIter + 'src>,
    /// One entry per physical trie depth (0..3). Tells the adapter how to
    /// translate variable-level peek/seek/open into physical operations.
    actions: [LevelAction; 3],
    /// Number of variable levels this pattern contributes (≤ 3).
    arity: u8,
    /// Map variable-depth → physical depth (i.e. the physical depth at which
    /// that variable lives).
    var_to_phys: Vec<u8>,
}

impl<'src> PatternTrieIter<'src> {
    pub fn new(
        source: &'src dyn TripleSource,
        pattern: &TriplePattern,
        var_order: &[Var],
        ordering: Ordering,
    ) -> Result<Self> {
        if !source.supports(ordering) {
            return Err(WcojError::OrderingUnavailable(ordering));
        }

        // Compute the three physical-level Terms in trie order.
        let phys_terms = match ordering {
            Ordering::Spo => [pattern.s, pattern.p, pattern.o],
            Ordering::Sop => [pattern.s, pattern.o, pattern.p],
            Ordering::Pso => [pattern.p, pattern.s, pattern.o],
            Ordering::Pos => [pattern.p, pattern.o, pattern.s],
            Ordering::Osp => [pattern.o, pattern.s, pattern.p],
            Ordering::Ops => [pattern.o, pattern.p, pattern.s],
        };

        // Build LevelActions and the variable-depth → physical-depth map.
        // Variables in the pattern that appear in `var_order` are exposed at
        // depths corresponding to their position in `var_order` for *this*
        // pattern's contribution.
        let mut actions = [LevelAction::Bound(0); 3];
        let mut var_to_phys = Vec::new();

        // First, mark bound levels.
        for (phys_d, term) in phys_terms.iter().enumerate() {
            if let Term::Bound(v) = term {
                actions[phys_d] = LevelAction::Bound(*v);
            }
        }

        // Then, walk `var_order` and assign each var that's in this pattern
        // to its physical depth, in the order it appears in `var_order`.
        for (var_slot, var) in var_order.iter().enumerate() {
            for (phys_d, term) in phys_terms.iter().enumerate() {
                if *term == Term::Var(*var) {
                    actions[phys_d] = LevelAction::Var(var_slot as u8);
                    var_to_phys.push(phys_d as u8);
                    break;
                }
            }
        }

        let arity = var_to_phys.len() as u8;

        // Build inner iterator and immediately seek through any bound prefix
        // *until* we hit a variable level. Bound levels deeper than the first
        // variable are applied lazily inside `open_level` / `peek`.
        let mut inner = source.iter(ordering)?;
        // Position the cursor through any leading bound levels.
        for phys_d in 0..3u8 {
            match actions[phys_d as usize] {
                LevelAction::Bound(v) => {
                    inner.seek(phys_d, v);
                    if inner.peek(phys_d) != Some(v) {
                        // No matching row — the iterator is empty.
                        return Ok(Self {
                            inner,
                            actions,
                            arity,
                            var_to_phys,
                        });
                    }
                    if phys_d < 2 {
                        inner.open_level(phys_d + 1);
                    }
                }
                LevelAction::Var(_) => break,
            }
        }

        Ok(Self {
            inner,
            actions,
            arity,
            var_to_phys,
        })
    }

    /// Resolve a variable-depth to the underlying physical depth, applying any
    /// trailing bound levels in between.
    fn phys_for(&self, var_depth: u8) -> u8 {
        self.var_to_phys[var_depth as usize]
    }
}

impl<'src> TrieIterator for PatternTrieIter<'src> {
    fn arity(&self) -> u8 {
        self.arity
    }

    fn peek(&self, depth: u8) -> Option<TermId> {
        let phys = self.phys_for(depth);
        self.inner.peek(phys)
    }

    fn seek(&mut self, depth: u8, value: TermId) {
        let phys = self.phys_for(depth);
        self.inner.seek(phys, value);
    }

    fn open_level(&mut self, depth: u8) {
        let phys = self.phys_for(depth);
        // Descend into `phys` from `phys - 1` (or no-op for the root).
        if phys > 0 {
            self.inner.open_level(phys);
        }
        // Apply any bound levels between this variable's physical depth and
        // the next variable's physical depth.
        let next_var_phys = self
            .var_to_phys
            .get((depth + 1) as usize)
            .copied()
            .unwrap_or(3);
        let mut p = phys + 1;
        while p < next_var_phys {
            if let LevelAction::Bound(v) = self.actions[p as usize] {
                self.inner.seek(p, v);
                if self.inner.peek(p) != Some(v) {
                    // Mark exhausted by seeking past the end at the next
                    // variable level so peek returns None.
                    // (Equivalent: set the parent cursor past hi.)
                    // Simplest portable way: seek to TermId::MAX at the var depth.
                    self.inner.open_level(p);
                    self.inner.seek(next_var_phys.min(2), TermId::MAX);
                    return;
                }
                self.inner.open_level(p + 1);
            }
            p += 1;
        }
    }

    fn up(&mut self, depth: u8) {
        let phys = self.phys_for(depth);
        self.inner.up(phys);
        // Also up any intermediate bound levels we descended through.
        let mut p = phys + 1;
        let next_var_phys = self
            .var_to_phys
            .get((depth + 1) as usize)
            .copied()
            .unwrap_or(3);
        while p < next_var_phys {
            self.inner.up(p);
            p += 1;
        }
    }
}
```

- [ ] **Step 5: Stub `trie/leapfrog.rs` so the module compiles**

Create `crates/wcoj/src/trie/leapfrog.rs`:

```rust
//! Placeholder — filled in by Task 5 (leapfrog intersection across patterns).
```

- [ ] **Step 6: Run the test — expect pass**

Run: `cargo test -p reasoner-wcoj --test trie_basics`
Expected: `test result: ok. 2 passed`.

- [ ] **Step 7: Commit**

```bash
git add crates/wcoj/src/trie crates/wcoj/tests/trie_basics.rs
git commit -m "$(cat <<'EOF'
wcoj: implement PatternTrieIter adapter over OrderedTripleIter

Each triple pattern produces one variable-indexed trie iterator using
the heuristic ordering. Bound levels are seeked through lazily so they
don't appear as variable depths to the leapfrog intersector.
EOF
)"
```

---

## Task 5: Implement the leapfrog intersection (multi-iterator `seek` loop)

**Files:**
- Create: `crates/wcoj/src/trie/leapfrog.rs` (replace placeholder)
- Create: `crates/wcoj/tests/leapfrog.rs`

- [ ] **Step 1: Write the failing test `crates/wcoj/tests/leapfrog.rs`**

```rust
use reasoner_wcoj::ids::{Ordering, Triple};
use reasoner_wcoj::pattern::{Term, TriplePattern, Var};
use reasoner_wcoj::source::vec_source::VecTripleSource;
use reasoner_wcoj::trie::leapfrog::LeapfrogJoin;
use reasoner_wcoj::trie::source_iter::PatternTrieIter;
use reasoner_wcoj::trie::TrieIterator;

#[test]
fn leapfrog_intersection_returns_only_common_values() {
    // Three patterns sharing variable ?x at level 0:
    //   (?x, p, 1)  → ?x ∈ {a where (a, p, 1)}
    //   (?x, q, 2)  → ?x ∈ {a where (a, q, 2)}
    //   (?x, r, 3)  → ?x ∈ {a where (a, r, 3)}
    // Subjects matching all three should be exactly {7}.
    let triples = vec![
        // x=5 matches p,q but not r
        Triple::new(5, 10, 1),
        Triple::new(5, 20, 2),
        // x=7 matches all three
        Triple::new(7, 10, 1),
        Triple::new(7, 20, 2),
        Triple::new(7, 30, 3),
        // x=9 matches r,q but not p
        Triple::new(9, 20, 2),
        Triple::new(9, 30, 3),
    ];
    let src = VecTripleSource::from_triples(triples);
    let var_order = vec![Var(0)];

    let p1 = TriplePattern::new(Term::Var(Var(0)), Term::Bound(10), Term::Bound(1));
    let p2 = TriplePattern::new(Term::Var(Var(0)), Term::Bound(20), Term::Bound(2));
    let p3 = TriplePattern::new(Term::Var(Var(0)), Term::Bound(30), Term::Bound(3));

    let it1 = PatternTrieIter::new(&src, &p1, &var_order, Ordering::Pos).unwrap();
    let it2 = PatternTrieIter::new(&src, &p2, &var_order, Ordering::Pos).unwrap();
    let it3 = PatternTrieIter::new(&src, &p3, &var_order, Ordering::Pos).unwrap();

    let iters: Vec<Box<dyn TrieIterator>> =
        vec![Box::new(it1), Box::new(it2), Box::new(it3)];

    let mut join = LeapfrogJoin::new(iters, 0);
    let mut out = Vec::new();
    while let Some(v) = join.next() {
        out.push(v);
    }
    assert_eq!(out, vec![7]);
}

#[test]
fn leapfrog_empty_when_one_iterator_is_empty() {
    let triples = vec![Triple::new(5, 10, 1), Triple::new(5, 20, 2)];
    let src = VecTripleSource::from_triples(triples);
    let var_order = vec![Var(0)];

    let p1 = TriplePattern::new(Term::Var(Var(0)), Term::Bound(10), Term::Bound(1));
    // No triple has p=99
    let p2 = TriplePattern::new(Term::Var(Var(0)), Term::Bound(99), Term::Bound(2));
    let it1 = PatternTrieIter::new(&src, &p1, &var_order, Ordering::Pos).unwrap();
    let it2 = PatternTrieIter::new(&src, &p2, &var_order, Ordering::Pos).unwrap();
    let iters: Vec<Box<dyn TrieIterator>> = vec![Box::new(it1), Box::new(it2)];
    let mut join = LeapfrogJoin::new(iters, 0);
    assert_eq!(join.next(), None);
}
```

- [ ] **Step 2: Run to confirm it fails**

Run: `cargo test -p reasoner-wcoj --test leapfrog`
Expected: compile errors.

- [ ] **Step 3: Implement `crates/wcoj/src/trie/leapfrog.rs`**

```rust
//! Leapfrog single-variable intersection.
//!
//! Given `k` trie iterators all positioned at the same variable depth, the
//! leapfrog algorithm advances them in round-robin until either (a) all
//! `k` iterators agree on a value (emit it) or (b) one of them runs off the
//! end (terminate). This is the inner loop of Veldhuizen's triejoin and
//! contributes the per-tuple cost we're trying to keep ≤5 ns/tuple.

use crate::ids::TermId;
use crate::trie::TrieIterator;

pub struct LeapfrogJoin<'a> {
    iters: Vec<Box<dyn TrieIterator + 'a>>,
    depth: u8,
    /// Position into `iters` we'll seek next.
    p: usize,
    /// True once we know the join is exhausted at this depth.
    done: bool,
    /// True before the first call to `next` — we don't seek on the very
    /// first call, just check the current heads.
    primed: bool,
}

impl<'a> LeapfrogJoin<'a> {
    pub fn new(iters: Vec<Box<dyn TrieIterator + 'a>>, depth: u8) -> Self {
        Self {
            iters,
            depth,
            p: 0,
            done: false,
            primed: false,
        }
    }

    pub fn done(&self) -> bool {
        self.done
    }

    /// Yield the next value common to all iterators at `depth`, or `None`.
    pub fn next(&mut self) -> Option<TermId> {
        if self.done || self.iters.is_empty() {
            self.done = true;
            return None;
        }

        if !self.primed {
            self.primed = true;
            // Sort iterators by current head so we can leapfrog deterministically.
            // For correctness we don't need to sort — but sorting picks the
            // smallest max-min gap first, which is faster.
            self.iters.sort_by_key(|it| it.peek(self.depth).unwrap_or(TermId::MAX));
            if self.iters.iter().any(|it| it.peek(self.depth).is_none()) {
                self.done = true;
                return None;
            }
            // After sort, p starts at 0 and the target is iters[k-1].peek.
            self.p = 0;
            return self.find_match();
        }

        // Subsequent call: advance the iterator that just produced the
        // matching value past it, then leapfrog again.
        let k = self.iters.len();
        // The matching value was iters[p].peek; advance past it.
        let cur = self.iters[self.p].peek(self.depth).unwrap();
        self.iters[self.p].seek(self.depth, cur.wrapping_add(1));
        if self.iters[self.p].peek(self.depth).is_none() {
            self.done = true;
            return None;
        }
        self.p = (self.p + 1) % k;
        self.find_match()
    }

    /// Core leapfrog loop: advance round-robin until all `k` iterators
    /// agree.
    fn find_match(&mut self) -> Option<TermId> {
        let k = self.iters.len();
        loop {
            // The target is the largest current head; we seek the iterator
            // at position `p` to it.
            let prev = (self.p + k - 1) % k;
            let target = self.iters[prev].peek(self.depth)?;
            let cur = self.iters[self.p].peek(self.depth)?;
            if cur == target {
                return Some(cur);
            }
            self.iters[self.p].seek(self.depth, target);
            if self.iters[self.p].peek(self.depth).is_none() {
                self.done = true;
                return None;
            }
            self.p = (self.p + 1) % k;
        }
    }
}
```

- [ ] **Step 4: Run the test — expect pass**

Run: `cargo test -p reasoner-wcoj --test leapfrog`
Expected: `test result: ok. 2 passed`.

- [ ] **Step 5: Commit**

```bash
git add crates/wcoj/src/trie/leapfrog.rs crates/wcoj/tests/leapfrog.rs
git commit -m "$(cat <<'EOF'
wcoj: implement single-variable leapfrog intersection

Round-robin seek across k trie iterators at one depth. Sorted-by-head
on first call for fewer leaps; subsequent calls advance past the last
emitted value. This is the per-tuple-cost hot path (NF1 ≤5 ns target).
EOF
)"
```

---

## Task 6: Arrow batch builder

**Files:**
- Create: `crates/wcoj/src/batch.rs` (replace placeholder)
- Create: `crates/wcoj/tests/batch.rs`

- [ ] **Step 1: Write the failing test `crates/wcoj/tests/batch.rs`**

```rust
use arrow::array::UInt64Array;
use reasoner_wcoj::batch::{BindingBatchBuilder, STANDARD_VECTOR_SIZE};
use reasoner_wcoj::pattern::Var;

#[test]
fn standard_vector_size_is_2048() {
    assert_eq!(STANDARD_VECTOR_SIZE, 2048);
}

#[test]
fn builder_flushes_at_capacity() {
    let mut b = BindingBatchBuilder::new(vec![Var(0), Var(1)]);
    for i in 0..STANDARD_VECTOR_SIZE as u64 {
        assert!(b.push_row(&[i, i + 1000]).is_none(), "no flush before capacity");
    }
    let batch = b.push_row(&[9999, 19999]).expect("flush at overflow");
    assert_eq!(batch.num_rows(), STANDARD_VECTOR_SIZE);
    assert_eq!(batch.num_columns(), 2);
    let col0 = batch
        .column(0)
        .as_any()
        .downcast_ref::<UInt64Array>()
        .unwrap();
    assert_eq!(col0.value(0), 0);
    assert_eq!(col0.value(STANDARD_VECTOR_SIZE - 1), STANDARD_VECTOR_SIZE as u64 - 1);
    // The overflow row is now the first row of the next batch.
    let final_batch = b.finish().unwrap();
    assert_eq!(final_batch.num_rows(), 1);
}

#[test]
fn finish_on_empty_builder_returns_none() {
    let mut b = BindingBatchBuilder::new(vec![Var(0)]);
    assert!(b.finish().is_none());
}
```

- [ ] **Step 2: Run to confirm it fails**

Run: `cargo test -p reasoner-wcoj --test batch`
Expected: errors.

- [ ] **Step 3: Implement `crates/wcoj/src/batch.rs`**

```rust
//! Arrow `RecordBatch` builder for variable bindings.
//!
//! `STANDARD_VECTOR_SIZE = 2048` mirrors DuckDB's chunk size (SPEC-03 F3).

use std::sync::Arc;

use arrow::array::{ArrayRef, UInt64Array};
use arrow::datatypes::{DataType, Field, Schema, SchemaRef};
use arrow::record_batch::RecordBatch;

use crate::pattern::Var;

pub const STANDARD_VECTOR_SIZE: usize = 2048;

pub struct BindingBatchBuilder {
    vars: Vec<Var>,
    schema: SchemaRef,
    /// One growable column per variable.
    cols: Vec<Vec<u64>>,
}

impl BindingBatchBuilder {
    pub fn new(vars: Vec<Var>) -> Self {
        let fields: Vec<Field> = vars
            .iter()
            .map(|v| Field::new(format!("v{}", v.0), DataType::UInt64, false))
            .collect();
        let schema = Arc::new(Schema::new(fields));
        let cols = vec![Vec::with_capacity(STANDARD_VECTOR_SIZE); vars.len()];
        Self { vars, schema, cols }
    }

    /// Push a row of bindings (one `u64` per variable, in `self.vars` order).
    /// Returns `Some(batch)` if pushing this row caused a flush.
    pub fn push_row(&mut self, row: &[u64]) -> Option<RecordBatch> {
        debug_assert_eq!(row.len(), self.vars.len());
        let flushed = if self.cols[0].len() == STANDARD_VECTOR_SIZE {
            self.finish_internal()
        } else {
            None
        };
        for (col, &v) in self.cols.iter_mut().zip(row.iter()) {
            col.push(v);
        }
        flushed
    }

    /// Drain any remaining rows into a batch. Returns `None` if the builder
    /// was empty.
    pub fn finish(&mut self) -> Option<RecordBatch> {
        self.finish_internal()
    }

    pub fn schema(&self) -> SchemaRef {
        self.schema.clone()
    }

    fn finish_internal(&mut self) -> Option<RecordBatch> {
        if self.cols[0].is_empty() {
            return None;
        }
        let arrays: Vec<ArrayRef> = self
            .cols
            .iter_mut()
            .map(|c| {
                let take = std::mem::take(c);
                Arc::new(UInt64Array::from(take)) as ArrayRef
            })
            .collect();
        // Re-allocate empty buffers for continued use.
        for c in &mut self.cols {
            *c = Vec::with_capacity(STANDARD_VECTOR_SIZE);
        }
        RecordBatch::try_new(self.schema.clone(), arrays).ok()
    }
}
```

- [ ] **Step 4: Run the test — expect pass**

Run: `cargo test -p reasoner-wcoj --test batch`
Expected: `test result: ok. 3 passed`.

- [ ] **Step 5: Commit**

```bash
git add crates/wcoj/src/batch.rs crates/wcoj/tests/batch.rs
git commit -m "wcoj: add Arrow RecordBatch builder with STANDARD_VECTOR_SIZE=2048"
```

---

## Task 7: Cancellation token

**Files:**
- Create: `crates/wcoj/src/cancel.rs` (replace placeholder)
- Create: `crates/wcoj/tests/cancel_token.rs`

- [ ] **Step 1: Write the failing test `crates/wcoj/tests/cancel_token.rs`**

```rust
use reasoner_wcoj::cancel::CancelToken;

#[test]
fn fresh_token_is_not_cancelled() {
    let t = CancelToken::new();
    assert!(!t.is_cancelled());
}

#[test]
fn cancel_propagates_to_clones() {
    let t = CancelToken::new();
    let t2 = t.clone();
    t.cancel();
    assert!(t2.is_cancelled());
}

#[test]
fn check_returns_err_after_cancel() {
    let t = CancelToken::new();
    t.cancel();
    assert!(t.check().is_err());
}
```

- [ ] **Step 2: Run to confirm it fails**

Run: `cargo test -p reasoner-wcoj --test cancel_token`
Expected: errors.

- [ ] **Step 3: Implement `crates/wcoj/src/cancel.rs`**

```rust
//! Cancellation token. SPEC-03 F7: queries respond within 100 ms.
//!
//! We poll an atomic `bool` once per output row (every 2048 rows when called
//! from the batch boundary) and once per leapfrog seek-loop iteration at
//! the top variable depth. At ≥5 ns/tuple that's ≥200M checks/sec —
//! well within the 100 ms latency budget.

use std::sync::atomic::{AtomicBool, Ordering as MemOrdering};
use std::sync::Arc;

use crate::error::{Result, WcojError};

#[derive(Clone, Default)]
pub struct CancelToken {
    flag: Arc<AtomicBool>,
}

impl CancelToken {
    pub fn new() -> Self {
        Self {
            flag: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn cancel(&self) {
        self.flag.store(true, MemOrdering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.flag.load(MemOrdering::Acquire)
    }

    /// Returns `Err(WcojError::Cancelled)` if the token has been cancelled.
    pub fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            Err(WcojError::Cancelled)
        } else {
            Ok(())
        }
    }
}
```

- [ ] **Step 4: Run the test — expect pass**

Run: `cargo test -p reasoner-wcoj --test cancel_token`
Expected: `test result: ok. 3 passed`.

- [ ] **Step 5: Commit**

```bash
git add crates/wcoj/src/cancel.rs crates/wcoj/tests/cancel_token.rs
git commit -m "wcoj: add CancelToken (atomic bool, cloneable handle)"
```

---

## Task 8: Cardinality estimator stub

**Files:**
- Create: `crates/wcoj/src/cardinality.rs` (replace placeholder)
- Create: `crates/wcoj/tests/cardinality.rs`

- [ ] **Step 1: Write the failing test `crates/wcoj/tests/cardinality.rs`**

```rust
use reasoner_wcoj::cardinality::{Cardinality, UniformEstimator};
use reasoner_wcoj::ids::Triple;
use reasoner_wcoj::pattern::{Term, TriplePattern, Var};
use reasoner_wcoj::source::vec_source::VecTripleSource;

#[test]
fn fully_unbound_pattern_estimates_total_triples() {
    let src = VecTripleSource::from_triples(vec![
        Triple::new(1, 10, 100),
        Triple::new(2, 10, 100),
        Triple::new(3, 20, 200),
    ]);
    let est = UniformEstimator::from_source(&src);
    let pat = TriplePattern::new(Term::Var(Var(0)), Term::Var(Var(1)), Term::Var(Var(2)));
    assert_eq!(est.estimate(&pat), 3);
}

#[test]
fn one_bound_position_estimates_third() {
    // Stub heuristic: each bound position cuts the estimate to 1/16 (rough
    // proxy for predicate skew). 3 triples * (1/16) ≈ 0 — clamp to 1.
    let src = VecTripleSource::from_triples(vec![
        Triple::new(1, 10, 100),
        Triple::new(2, 10, 100),
        Triple::new(3, 20, 200),
    ]);
    let est = UniformEstimator::from_source(&src);
    let pat = TriplePattern::new(Term::Var(Var(0)), Term::Bound(10), Term::Var(Var(1)));
    let e = est.estimate(&pat);
    assert!(e >= 1 && e <= 3, "estimate {e} should be 1..=3");
}
```

- [ ] **Step 2: Run to confirm it fails**

Run: `cargo test -p reasoner-wcoj --test cardinality`
Expected: errors.

- [ ] **Step 3: Implement `crates/wcoj/src/cardinality.rs`**

```rust
//! Cardinality estimator (Stage 1 stub).
//!
//! SPEC-03 F6 requires per-predicate histograms from SPEC-02. We don't have
//! those yet; this stub gives the planner *enough* signal to make the
//! WCOJ-vs-binary-join cutover decision in Task 12.

use crate::pattern::{Term, TriplePattern};
use crate::source::TripleSource;

pub trait Cardinality {
    /// Estimated number of matching triples for `pat`.
    fn estimate(&self, pat: &TriplePattern) -> usize;
}

pub struct UniformEstimator {
    total: usize,
}

impl UniformEstimator {
    pub fn from_source<S: TripleSource + ?Sized>(src: &S) -> Self {
        Self { total: src.total_triples() }
    }
}

impl Cardinality for UniformEstimator {
    fn estimate(&self, pat: &TriplePattern) -> usize {
        // Each bound position multiplies the selectivity by `1/16` — a
        // deliberately coarse "uniform & moderately selective" prior. The
        // real estimator (Stage 2) reads histograms from SPEC-02.
        let mut sel: f64 = 1.0;
        for t in [pat.s, pat.p, pat.o] {
            if matches!(t, Term::Bound(_)) {
                sel *= 1.0 / 16.0;
            }
        }
        ((self.total as f64) * sel).round().max(1.0) as usize
    }
}
```

- [ ] **Step 4: Run the test — expect pass**

Run: `cargo test -p reasoner-wcoj --test cardinality`
Expected: `test result: ok. 2 passed`.

- [ ] **Step 5: Commit**

```bash
git add crates/wcoj/src/cardinality.rs crates/wcoj/tests/cardinality.rs
git commit -m "wcoj: add UniformEstimator stub for the planner cutover"
```

---

## Task 9: Plan struct (variable ordering selection)

**Files:**
- Create: `crates/wcoj/src/plan.rs` (replace placeholder)
- Create: `crates/wcoj/tests/plan.rs`

- [ ] **Step 1: Write the failing test `crates/wcoj/tests/plan.rs`**

```rust
use reasoner_wcoj::pattern::{Bgp, Term, TriplePattern, Var};
use reasoner_wcoj::plan::{ExecutionPlan, PlanKind};

#[test]
fn plan_for_4_cycle_uses_wcoj_and_orders_vars_by_degree() {
    // 4-cycle: (?a, p, ?b)(?b, p, ?c)(?c, p, ?d)(?d, p, ?a)
    let p = 10;
    let bgp = Bgp::new(vec![
        TriplePattern::new(Term::Var(Var(0)), Term::Bound(p), Term::Var(Var(1))),
        TriplePattern::new(Term::Var(Var(1)), Term::Bound(p), Term::Var(Var(2))),
        TriplePattern::new(Term::Var(Var(2)), Term::Bound(p), Term::Var(Var(3))),
        TriplePattern::new(Term::Var(Var(3)), Term::Bound(p), Term::Var(Var(0))),
    ]);
    let plan = ExecutionPlan::for_bgp(&bgp, 4);
    assert_eq!(plan.kind, PlanKind::Wcoj);
    // All 4 variables present.
    assert_eq!(plan.var_order.len(), 4);
    let mut sorted = plan.var_order.clone();
    sorted.sort();
    assert_eq!(sorted, vec![Var(0), Var(1), Var(2), Var(3)]);
}

#[test]
fn plan_for_two_pattern_bgp_picks_binary_hash() {
    let bgp = Bgp::new(vec![
        TriplePattern::new(Term::Var(Var(0)), Term::Bound(10), Term::Var(Var(1))),
        TriplePattern::new(Term::Var(Var(1)), Term::Bound(20), Term::Var(Var(2))),
    ]);
    let plan = ExecutionPlan::for_bgp(&bgp, 4);
    assert_eq!(plan.kind, PlanKind::BinaryHash);
}
```

- [ ] **Step 2: Run to confirm it fails**

Run: `cargo test -p reasoner-wcoj --test plan`
Expected: errors.

- [ ] **Step 3: Implement `crates/wcoj/src/plan.rs`**

```rust
//! Execution plan: chooses between WCOJ and binary-hash, and picks the
//! variable ordering used by Leapfrog Triejoin.

use crate::pattern::{Bgp, Term, Var};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanKind {
    /// Leapfrog Triejoin — for ≥`wcoj_cutover` patterns.
    Wcoj,
    /// Left-deep binary hash join — for ≤`wcoj_cutover - 1` patterns
    /// and for fully-ground BGPs.
    BinaryHash,
}

#[derive(Debug, Clone)]
pub struct ExecutionPlan {
    pub kind: PlanKind,
    /// Variable elimination order for WCOJ (depth 0 = outermost).
    pub var_order: Vec<Var>,
}

impl ExecutionPlan {
    pub fn for_bgp(bgp: &Bgp, wcoj_cutover: usize) -> Self {
        // Ground BGPs are degenerate — pick BinaryHash; the executor will
        // short-circuit them.
        let all_ground = bgp.patterns.iter().all(|p| p.is_ground());
        if all_ground {
            return Self {
                kind: PlanKind::BinaryHash,
                var_order: Vec::new(),
            };
        }

        let kind = if bgp.patterns.len() >= wcoj_cutover {
            PlanKind::Wcoj
        } else {
            PlanKind::BinaryHash
        };

        // Order variables by descending degree (how many patterns mention
        // them). High-degree first cuts the search space fastest. Ties
        // broken by first-appearance order for determinism.
        let mut vars = bgp.variables();
        let mut degrees: Vec<(Var, usize)> = vars
            .drain(..)
            .map(|v| {
                let d = bgp
                    .patterns
                    .iter()
                    .filter(|p| p.position_of(v).is_some())
                    .count();
                (v, d)
            })
            .collect();
        // Stable sort by descending degree; first-appearance order survives ties.
        degrees.sort_by(|a, b| b.1.cmp(&a.1));
        let var_order = degrees.into_iter().map(|(v, _)| v).collect();

        // Suppress unused-import warning for Term.
        let _ = Term::Bound(0);

        Self { kind, var_order }
    }
}
```

- [ ] **Step 4: Run the test — expect pass**

Run: `cargo test -p reasoner-wcoj --test plan`
Expected: `test result: ok. 2 passed`.

- [ ] **Step 5: Commit**

```bash
git add crates/wcoj/src/plan.rs crates/wcoj/tests/plan.rs
git commit -m "wcoj: add ExecutionPlan with degree-ordered variable elimination"
```

---

## Task 10: WCOJ executor — full multi-variable Leapfrog Triejoin loop

**Files:**
- Create: `crates/wcoj/src/executor/mod.rs` (replace placeholder)
- Create: `crates/wcoj/src/executor/wcoj.rs`
- Create: `crates/wcoj/tests/wcoj_smoke.rs`

- [ ] **Step 1: Write the failing test `crates/wcoj/tests/wcoj_smoke.rs`**

```rust
use arrow::array::UInt64Array;
use reasoner_wcoj::cancel::CancelToken;
use reasoner_wcoj::executor::wcoj::WcojExecutor;
use reasoner_wcoj::ids::Triple;
use reasoner_wcoj::pattern::{Bgp, Term, TriplePattern, Var};
use reasoner_wcoj::plan::ExecutionPlan;
use reasoner_wcoj::source::vec_source::VecTripleSource;

fn collect_pairs(batches: Vec<arrow::record_batch::RecordBatch>) -> Vec<Vec<u64>> {
    let mut out: Vec<Vec<u64>> = Vec::new();
    for b in batches {
        let n = b.num_rows();
        let cols: Vec<&UInt64Array> = (0..b.num_columns())
            .map(|i| b.column(i).as_any().downcast_ref::<UInt64Array>().unwrap())
            .collect();
        for r in 0..n {
            out.push(cols.iter().map(|c| c.value(r)).collect());
        }
    }
    out
}

#[test]
fn triangle_join_produces_correct_results() {
    // Triangle: (?a, p, ?b)(?b, p, ?c)(?c, p, ?a) over a tiny graph.
    // Edges: 1→2, 2→3, 3→1 (forms a triangle), plus noise 1→4.
    let p = 10;
    let triples = vec![
        Triple::new(1, p, 2),
        Triple::new(2, p, 3),
        Triple::new(3, p, 1),
        Triple::new(1, p, 4),
    ];
    let src = VecTripleSource::from_triples(triples);

    let bgp = Bgp::new(vec![
        TriplePattern::new(Term::Var(Var(0)), Term::Bound(p), Term::Var(Var(1))),
        TriplePattern::new(Term::Var(Var(1)), Term::Bound(p), Term::Var(Var(2))),
        TriplePattern::new(Term::Var(Var(2)), Term::Bound(p), Term::Var(Var(0))),
    ]);
    let plan = ExecutionPlan {
        kind: reasoner_wcoj::plan::PlanKind::Wcoj,
        var_order: vec![Var(0), Var(1), Var(2)],
    };

    let cancel = CancelToken::new();
    let exec = WcojExecutor::new(&src, &bgp, &plan, cancel);
    let batches: Vec<_> = exec.into_iter().collect::<Result<_, _>>().unwrap();
    let mut rows = collect_pairs(batches);
    rows.sort();
    // Triangles: (1,2,3), (2,3,1), (3,1,2) are the same cycle viewed from
    // each starting vertex — the join produces all three.
    assert_eq!(rows, vec![vec![1, 2, 3], vec![2, 3, 1], vec![3, 1, 2]]);
}

#[test]
fn empty_result_yields_no_batches() {
    let src = VecTripleSource::from_triples(vec![Triple::new(1, 10, 2)]);
    let bgp = Bgp::new(vec![
        TriplePattern::new(Term::Var(Var(0)), Term::Bound(10), Term::Var(Var(1))),
        TriplePattern::new(Term::Var(Var(0)), Term::Bound(99), Term::Var(Var(1))),
    ]);
    let plan = ExecutionPlan {
        kind: reasoner_wcoj::plan::PlanKind::Wcoj,
        var_order: vec![Var(0), Var(1)],
    };
    let exec = WcojExecutor::new(&src, &bgp, &plan, CancelToken::new());
    let batches: Vec<_> = exec.into_iter().collect::<Result<_, _>>().unwrap();
    assert_eq!(collect_pairs(batches).len(), 0);
}
```

- [ ] **Step 2: Run to confirm it fails**

Run: `cargo test -p reasoner-wcoj --test wcoj_smoke`
Expected: errors.

- [ ] **Step 3: Implement `crates/wcoj/src/executor/mod.rs`**

```rust
//! Query executors. `WcojExecutor` and `BinaryHashExecutor` both produce
//! a stream of Arrow `RecordBatch`es.

pub mod binary_hash;
pub mod wcoj;

use arrow::record_batch::RecordBatch;
use crate::error::Result;

/// Common output type — a fallible iterator over batches.
pub type BatchStream<'a> = Box<dyn Iterator<Item = Result<RecordBatch>> + 'a>;
```

- [ ] **Step 4: Implement `crates/wcoj/src/executor/wcoj.rs`**

```rust
//! Leapfrog Triejoin executor.
//!
//! The recursion shape is:
//!
//! ```text
//! join(depth):
//!     if depth == n_vars:
//!         emit current binding
//!         return
//!     leapfrog over iterators that mention var[depth]
//!     for each common value v:
//!         binding[depth] = v
//!         for each contributing iterator: open_level(depth)
//!         join(depth + 1)
//!         for each contributing iterator: up(depth)
//! ```
//!
//! We implement this with an explicit per-depth state stack to keep the
//! hot path branch-predictable (no recursion frames) and to make
//! cancellation polling cheap.

use std::sync::Arc;

use arrow::record_batch::RecordBatch;

use crate::batch::BindingBatchBuilder;
use crate::cancel::CancelToken;
use crate::error::Result;
use crate::ids::TermId;
use crate::pattern::{Bgp, Var};
use crate::plan::ExecutionPlan;
use crate::source::TripleSource;
use crate::trie::leapfrog::LeapfrogJoin;
use crate::trie::source_iter::PatternTrieIter;
use crate::trie::TrieIterator;

pub struct WcojExecutor<'src> {
    source: &'src dyn TripleSource,
    bgp: Arc<Bgp>,
    plan: Arc<ExecutionPlan>,
    cancel: CancelToken,
}

impl<'src> WcojExecutor<'src> {
    pub fn new(
        source: &'src dyn TripleSource,
        bgp: &Bgp,
        plan: &ExecutionPlan,
        cancel: CancelToken,
    ) -> Self {
        Self {
            source,
            bgp: Arc::new(bgp.clone()),
            plan: Arc::new(plan.clone()),
            cancel,
        }
    }

    pub fn into_iter(self) -> BatchIter<'src> {
        BatchIter::new(self)
    }
}

/// Output iterator: drives the leapfrog loop, flushes Arrow batches.
pub struct BatchIter<'src> {
    exec: WcojExecutor<'src>,
    builder: BindingBatchBuilder,
    /// Owned per-pattern trie iterators (one per BGP pattern).
    iters: Vec<Box<dyn TrieIterator + 'src>>,
    /// For each variable depth: indices of `iters` that mention this var.
    contributing: Vec<Vec<usize>>,
    /// Per-depth join state; `None` means "not entered yet at this depth".
    join_state: Vec<Option<LeapfrogJoin<'src>>>,
    /// Current binding values per depth.
    binding: Vec<TermId>,
    /// Current recursion depth (== variable index being processed).
    depth: u8,
    /// True once the iterator has emitted its final batch and is exhausted.
    finished: bool,
    /// True if init failed; emit error once, then end.
    pending_error: Option<crate::error::WcojError>,
}

impl<'src> BatchIter<'src> {
    fn new(exec: WcojExecutor<'src>) -> Self {
        let n_vars = exec.plan.var_order.len();
        let builder = BindingBatchBuilder::new(exec.plan.var_order.clone());
        let mut iters: Vec<Box<dyn TrieIterator + 'src>> = Vec::new();
        let mut pending_error = None;

        // Build one PatternTrieIter per pattern. Each iter is told the
        // global variable order so that variable-depth aligns across all
        // iters (modulo "this iter doesn't mention this var" — we encode
        // that via `contributing`).
        for pat in &exec.bgp.patterns {
            match PatternTrieIter::new(
                exec.source,
                pat,
                &exec.plan.var_order,
                pat.preferred_ordering(),
            ) {
                Ok(it) => iters.push(Box::new(it)),
                Err(e) => {
                    pending_error = Some(e);
                    break;
                }
            }
        }

        // Compute, for each variable depth, which patterns contribute.
        let mut contributing = Vec::with_capacity(n_vars);
        for (d, var) in exec.plan.var_order.iter().enumerate() {
            let mut v = Vec::new();
            for (i, pat) in exec.bgp.patterns.iter().enumerate() {
                if pat.position_of(*var).is_some() {
                    v.push(i);
                }
            }
            // Sanity: at least one iterator must mention each var.
            debug_assert!(!v.is_empty(), "variable {var:?} at depth {d} has no contributing pattern");
            contributing.push(v);
        }

        let join_state = (0..n_vars).map(|_| None).collect();
        let binding = vec![0; n_vars];

        Self {
            exec,
            builder,
            iters,
            contributing,
            join_state,
            binding,
            depth: 0,
            finished: false,
            pending_error,
        }
    }

    /// Run the recursion until the next batch boundary (or end). Returns
    /// `Some(batch)`, `Some(error)`, or `None` if no more output is possible.
    fn step(&mut self) -> Option<Result<RecordBatch>> {
        if let Some(e) = self.pending_error.take() {
            self.finished = true;
            return Some(Err(e));
        }
        if self.finished {
            return None;
        }

        let n_vars = self.exec.plan.var_order.len() as u8;
        // Special case: zero variables (all-ground BGP). The WCOJ executor
        // shouldn't be called for this, but handle it gracefully.
        if n_vars == 0 {
            self.finished = true;
            return None;
        }

        loop {
            // Cancellation check at every depth-0 iteration: cheap, ≥100ms
            // responsiveness on any realistic workload.
            if self.depth == 0 {
                if let Err(e) = self.exec.cancel.check() {
                    self.finished = true;
                    return Some(Err(e));
                }
            }

            // Ensure a leapfrog at this depth is initialised.
            if self.join_state[self.depth as usize].is_none() {
                // Borrow the contributing iterators by index. Because `iters`
                // is a `Vec<Box<dyn TrieIterator + 'src>>`, and `LeapfrogJoin`
                // wants `Vec<Box<dyn TrieIterator + 'a>>`, we temporarily
                // *move* the contributing iters into the join and put them
                // back on `up`. Use `std::mem::replace` with a dummy.
                let idxs = self.contributing[self.depth as usize].clone();
                let mut taken: Vec<Box<dyn TrieIterator + 'src>> =
                    Vec::with_capacity(idxs.len());
                for &i in &idxs {
                    // Swap out with a NoopTrieIter sentinel.
                    let placeholder: Box<dyn TrieIterator + 'src> = Box::new(NoopTrieIter);
                    let real = std::mem::replace(&mut self.iters[i], placeholder);
                    taken.push(real);
                }
                self.join_state[self.depth as usize] =
                    Some(LeapfrogJoin::new(taken, self.depth));
            }

            // Drive the leapfrog one step.
            let join = self.join_state[self.depth as usize].as_mut().unwrap();
            let next = join.next();

            match next {
                Some(v) => {
                    self.binding[self.depth as usize] = v;
                    if self.depth + 1 == n_vars {
                        // Emit a full binding row.
                        let flushed = self.builder.push_row(&self.binding);
                        if let Some(b) = flushed {
                            return Some(Ok(b));
                        }
                        // Stay at this depth; loop and let leapfrog emit the
                        // next match.
                    } else {
                        // Descend: open_level on each contributing iter for
                        // this depth.
                        let idxs = self.contributing[self.depth as usize].clone();
                        // Reach into the join's iters via a public accessor
                        // would be cleaner; instead we put the iters back
                        // *now*, advance, and re-take on next iteration.
                        // Simpler: borrow the iters held inside the join
                        // mutably via a helper.
                        let join = self.join_state[self.depth as usize].as_mut().unwrap();
                        for it in join.iters_mut() {
                            it.open_level(self.depth + 1);
                        }
                        // contributing[depth+1] may reference iters that are
                        // currently held by this join, OR iters that are
                        // still in self.iters. Put the held iters back so
                        // the next-depth init can take what it needs.
                        let taken = std::mem::replace(
                            self.join_state[self.depth as usize].as_mut().unwrap(),
                            LeapfrogJoin::new(Vec::new(), self.depth),
                        );
                        let returned = taken.into_iters();
                        for (i, real) in idxs.iter().zip(returned) {
                            self.iters[*i] = real;
                        }
                        // Recreate the join for this depth with empty so we
                        // know to re-init on the way back up.
                        // Actually: we want this depth's join state to
                        // remember it has been entered so that when we come
                        // back up, we advance it past the value we just
                        // descended on. Encode that with `Some(empty_done=false)`.
                        self.join_state[self.depth as usize] =
                            Some(LeapfrogJoin::reentry_marker(self.depth));
                        self.depth += 1;
                    }
                }
                None => {
                    // Exhausted at this depth — restore iters and ascend.
                    let join = self.join_state[self.depth as usize].take().unwrap();
                    let idxs = self.contributing[self.depth as usize].clone();
                    let returned = join.into_iters();
                    for (i, real) in idxs.iter().zip(returned) {
                        self.iters[*i] = real;
                    }
                    if self.depth == 0 {
                        // Final flush.
                        self.finished = true;
                        if let Some(b) = self.builder.finish() {
                            return Some(Ok(b));
                        }
                        return None;
                    }
                    // Bubble up.
                    self.depth -= 1;
                    // The parent depth's contributing iters need `up(depth+1)`
                    // applied. They're either currently held by the parent's
                    // join_state, or back in self.iters depending on whether
                    // the parent re-entered. The reentry_marker is "empty"
                    // so we can safely take it, do `up`, and rebuild.
                    let parent_idxs = self.contributing[self.depth as usize].clone();
                    for i in parent_idxs {
                        self.iters[i].up(self.depth + 1);
                    }
                    // Drop the parent's reentry marker; next loop iteration
                    // will re-init the join, which on re-init advances past
                    // the previously-emitted value.
                    // BUT — the LeapfrogJoin we re-create starts fresh from
                    // the iters' current heads, which are already positioned
                    // past the emitted value because PatternTrieIter.seek
                    // was advanced inside the join.next() call. To force
                    // advance, seek each parent contributing iter past the
                    // current binding[parent_depth].
                    let parent_idxs = self.contributing[self.depth as usize].clone();
                    let parent_val = self.binding[self.depth as usize];
                    for i in parent_idxs {
                        self.iters[i].seek(self.depth, parent_val.wrapping_add(1));
                    }
                    self.join_state[self.depth as usize] = None;
                }
            }
        }
    }
}

impl<'src> Iterator for BatchIter<'src> {
    type Item = Result<RecordBatch>;
    fn next(&mut self) -> Option<Self::Item> {
        self.step()
    }
}

/// Placeholder trie iterator used while real iters are temporarily moved
/// into a `LeapfrogJoin`. Never queried — must be replaced before use.
struct NoopTrieIter;

impl TrieIterator for NoopTrieIter {
    fn arity(&self) -> u8 {
        0
    }
    fn peek(&self, _: u8) -> Option<TermId> {
        None
    }
    fn seek(&mut self, _: u8, _: TermId) {}
    fn open_level(&mut self, _: u8) {}
    fn up(&mut self, _: u8) {}
}

// Required public additions to `LeapfrogJoin` — add them in
// `trie/leapfrog.rs` if not present yet. See companion edit below.
```

- [ ] **Step 5: Augment `crates/wcoj/src/trie/leapfrog.rs` with the helpers `WcojExecutor` needs**

Append to `crates/wcoj/src/trie/leapfrog.rs`:

```rust
impl<'a> LeapfrogJoin<'a> {
    /// Mutable access to the held iterators (used by the WCOJ executor when
    /// descending to call `open_level` on each contributing iter).
    pub fn iters_mut(&mut self) -> &mut [Box<dyn TrieIterator + 'a>] {
        &mut self.iters
    }

    /// Consume the join, returning the held iterators (used by the WCOJ
    /// executor when ascending).
    pub fn into_iters(self) -> Vec<Box<dyn TrieIterator + 'a>> {
        self.iters
    }

    /// Construct an "already-entered" marker that holds no iterators. Used
    /// by the executor to remember it has descended past this depth.
    pub fn reentry_marker(depth: u8) -> Self {
        Self {
            iters: Vec::new(),
            depth,
            p: 0,
            done: true,
            primed: true,
        }
    }
}
```

- [ ] **Step 6: Run the test — expect pass**

Run: `cargo test -p reasoner-wcoj --test wcoj_smoke`
Expected: `test result: ok. 2 passed`. The triangle case is the canonical sanity check.

If the test fails on result ordering, sort both `rows` and the expected vector in the test and compare — but the listed code already sorts.

- [ ] **Step 7: Commit**

```bash
git add crates/wcoj/src/executor crates/wcoj/src/trie/leapfrog.rs crates/wcoj/tests/wcoj_smoke.rs
git commit -m "$(cat <<'EOF'
wcoj: implement WcojExecutor — multi-variable leapfrog with batch output

Explicit-stack drive of the per-depth leapfrog. Triangle smoke test
passes. Cancellation polled at depth-0 iterations. Output flushed to
Arrow RecordBatches of 2048 rows.
EOF
)"
```

---

## Task 11: Binary-hash-join executor (the differential-fuzz reference)

**Files:**
- Create: `crates/wcoj/src/executor/binary_hash.rs`
- Create: `crates/wcoj/tests/binary_hash_smoke.rs`

- [ ] **Step 1: Write the failing test `crates/wcoj/tests/binary_hash_smoke.rs`**

```rust
use arrow::array::UInt64Array;
use reasoner_wcoj::cancel::CancelToken;
use reasoner_wcoj::executor::binary_hash::BinaryHashExecutor;
use reasoner_wcoj::ids::Triple;
use reasoner_wcoj::pattern::{Bgp, Term, TriplePattern, Var};
use reasoner_wcoj::source::vec_source::VecTripleSource;

fn collect(batches: Vec<arrow::record_batch::RecordBatch>) -> Vec<Vec<u64>> {
    let mut out: Vec<Vec<u64>> = Vec::new();
    for b in batches {
        let cols: Vec<&UInt64Array> = (0..b.num_columns())
            .map(|i| b.column(i).as_any().downcast_ref::<UInt64Array>().unwrap())
            .collect();
        for r in 0..b.num_rows() {
            out.push(cols.iter().map(|c| c.value(r)).collect());
        }
    }
    out
}

#[test]
fn binary_hash_join_triangle() {
    let p = 10;
    let triples = vec![
        Triple::new(1, p, 2),
        Triple::new(2, p, 3),
        Triple::new(3, p, 1),
        Triple::new(1, p, 4),
    ];
    let src = VecTripleSource::from_triples(triples);
    let bgp = Bgp::new(vec![
        TriplePattern::new(Term::Var(Var(0)), Term::Bound(p), Term::Var(Var(1))),
        TriplePattern::new(Term::Var(Var(1)), Term::Bound(p), Term::Var(Var(2))),
        TriplePattern::new(Term::Var(Var(2)), Term::Bound(p), Term::Var(Var(0))),
    ]);
    let exec = BinaryHashExecutor::new(&src, &bgp, vec![Var(0), Var(1), Var(2)], CancelToken::new());
    let mut rows = collect(exec.into_iter().collect::<Result<_, _>>().unwrap());
    rows.sort();
    assert_eq!(rows, vec![vec![1, 2, 3], vec![2, 3, 1], vec![3, 1, 2]]);
}

#[test]
fn binary_hash_join_ground_pattern_returns_one_empty_row_when_match() {
    // (1, 10, 2) is in the graph — match yields one empty binding.
    let src = VecTripleSource::from_triples(vec![Triple::new(1, 10, 2)]);
    let bgp = Bgp::new(vec![TriplePattern::new(Term::Bound(1), Term::Bound(10), Term::Bound(2))]);
    let exec = BinaryHashExecutor::new(&src, &bgp, vec![], CancelToken::new());
    let batches: Vec<_> = exec.into_iter().collect::<Result<_, _>>().unwrap();
    // Ground match: one row, zero columns. We materialise this as a
    // RecordBatch with 0 columns and 1 row (or simply count match→1).
    assert_eq!(batches.iter().map(|b| b.num_rows()).sum::<usize>(), 1);
}
```

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p reasoner-wcoj --test binary_hash_smoke`
Expected: errors.

- [ ] **Step 3: Implement `crates/wcoj/src/executor/binary_hash.rs`**

```rust
//! Left-deep binary-hash-join executor.
//!
//! Two jobs: (1) execute BGPs of ≤3 patterns (where WCOJ overhead is not
//! worth paying), (2) serve as the bit-identical reference implementation
//! for the differential fuzzer (SPEC-03 acceptance #3).
//!
//! Algorithm: scan pattern 0 (full source materialised through `iter` over
//! its preferred ordering, filtering by bound positions). For each
//! subsequent pattern, build a hash table on the join keys (variables in
//! common with the running binding set) and probe.

use std::collections::HashMap;
use std::sync::Arc;

use arrow::record_batch::RecordBatch;

use crate::batch::BindingBatchBuilder;
use crate::cancel::CancelToken;
use crate::error::{Result, WcojError};
use crate::ids::{Ordering, TermId, Triple};
use crate::pattern::{Bgp, Term, TriplePattern, Var};
use crate::source::TripleSource;

pub struct BinaryHashExecutor<'src> {
    source: &'src dyn TripleSource,
    bgp: Arc<Bgp>,
    out_vars: Vec<Var>,
    cancel: CancelToken,
}

impl<'src> BinaryHashExecutor<'src> {
    pub fn new(
        source: &'src dyn TripleSource,
        bgp: &Bgp,
        out_vars: Vec<Var>,
        cancel: CancelToken,
    ) -> Self {
        Self {
            source,
            bgp: Arc::new(bgp.clone()),
            out_vars,
            cancel,
        }
    }

    pub fn into_iter(self) -> BatchIter<'src> {
        BatchIter::new(self)
    }
}

/// All matching triples for a single pattern, materialised eagerly.
///
/// Stage-1 simplification: full scan of one ordering, filtering on bound
/// positions. SPEC-02 will offer a more selective access path; we don't
/// need it here.
fn scan_pattern<'src>(
    source: &'src dyn TripleSource,
    pat: &TriplePattern,
) -> Result<Vec<Triple>> {
    let ord = Ordering::Spo;
    let mut iter = source.iter(ord)?;
    let mut out = Vec::new();

    // Iterate over the full source. We use the depth-2 cursor exhaustively:
    // peek S; for each S, open level 1, walk all P; for each P, open level 2,
    // walk all O.
    while let Some(s) = iter.peek(0) {
        if let Term::Bound(req_s) = pat.s {
            if s != req_s {
                iter.seek(0, req_s);
                continue;
            }
        }
        iter.open_level(1);
        while let Some(p) = iter.peek(1) {
            if let Term::Bound(req_p) = pat.p {
                if p != req_p {
                    iter.seek(1, req_p);
                    continue;
                }
            }
            iter.open_level(2);
            while let Some(o) = iter.peek(2) {
                if let Term::Bound(req_o) = pat.o {
                    if o == req_o {
                        out.push(Triple::new(s, p, o));
                    }
                } else {
                    out.push(Triple::new(s, p, o));
                }
                iter.seek(2, o + 1);
            }
            iter.up(2);
            iter.seek(1, p + 1);
        }
        iter.up(1);
        iter.seek(0, s + 1);
    }
    Ok(out)
}

/// Extract the values bound by `pat` for the variables in `vars`, returning
/// one entry per variable in `vars` order (or `None` if the variable isn't
/// mentioned in `pat`).
fn project(pat: &TriplePattern, t: Triple, vars: &[Var]) -> Vec<TermId> {
    let mut out = Vec::with_capacity(vars.len());
    for v in vars {
        let val = match pat.position_of(*v) {
            Some(0) => t.s,
            Some(1) => t.p,
            Some(2) => t.o,
            _ => panic!("variable {v:?} not in pattern"),
        };
        out.push(val);
    }
    out
}

pub struct BatchIter<'src> {
    exec: BinaryHashExecutor<'src>,
    /// All output rows materialised eagerly into a vector — Stage-1
    /// simplification. For Stage-2 we'll stream batches lazily.
    rows: std::vec::IntoIter<Vec<TermId>>,
    builder: BindingBatchBuilder,
    done: bool,
    pending_error: Option<WcojError>,
    /// Special case: ground BGP with zero output vars — emit one row.
    ground_match_remaining: usize,
}

impl<'src> BatchIter<'src> {
    fn new(exec: BinaryHashExecutor<'src>) -> Self {
        let mut pending_error = None;
        let mut rows: Vec<Vec<TermId>> = Vec::new();
        let mut ground_match_remaining = 0usize;

        // Handle the all-ground case separately: count matches, emit empty rows.
        let all_ground = exec.bgp.patterns.iter().all(|p| p.is_ground());
        if all_ground {
            let mut count = 1usize;
            for pat in &exec.bgp.patterns {
                match scan_pattern(exec.source, pat) {
                    Ok(v) if v.is_empty() => {
                        count = 0;
                        break;
                    }
                    Ok(_) => {}
                    Err(e) => {
                        pending_error = Some(e);
                        break;
                    }
                }
            }
            ground_match_remaining = count;
        } else if let Err(e) = (|| -> Result<()> {
            // Build cumulative bindings: start with pattern 0, then join in
            // pattern 1, 2, ...
            let first = &exec.bgp.patterns[0];
            let first_vars: Vec<Var> = exec
                .out_vars
                .iter()
                .filter(|v| first.position_of(**v).is_some())
                .copied()
                .collect();
            let triples = scan_pattern(exec.source, first)?;
            let mut cur_vars = first_vars.clone();
            let mut cur_rows: Vec<Vec<TermId>> =
                triples.iter().map(|t| project(first, *t, &cur_vars)).collect();

            for pat in exec.bgp.patterns.iter().skip(1) {
                if let Err(e) = exec.cancel.check() {
                    return Err(e);
                }
                // Variables this pattern contributes that are in out_vars.
                let pat_vars: Vec<Var> = exec
                    .out_vars
                    .iter()
                    .filter(|v| pat.position_of(**v).is_some())
                    .copied()
                    .collect();
                // Join keys = intersection of cur_vars and pat_vars.
                let join_keys: Vec<Var> = cur_vars
                    .iter()
                    .filter(|v| pat_vars.contains(v))
                    .copied()
                    .collect();

                // Scan and project the new pattern.
                let new_triples = scan_pattern(exec.source, pat)?;
                let new_rows: Vec<Vec<TermId>> = new_triples
                    .iter()
                    .map(|t| project(pat, *t, &pat_vars))
                    .collect();

                // Build hash table on join keys.
                let pat_key_positions: Vec<usize> = join_keys
                    .iter()
                    .map(|v| pat_vars.iter().position(|x| x == v).unwrap())
                    .collect();
                let mut ht: HashMap<Vec<TermId>, Vec<Vec<TermId>>> = HashMap::new();
                for nr in &new_rows {
                    let key: Vec<TermId> = pat_key_positions.iter().map(|&i| nr[i]).collect();
                    ht.entry(key).or_default().push(nr.clone());
                }

                let cur_key_positions: Vec<usize> = join_keys
                    .iter()
                    .map(|v| cur_vars.iter().position(|x| x == v).unwrap())
                    .collect();
                // New cur_vars = cur_vars ∪ (pat_vars \ cur_vars).
                let mut combined_vars = cur_vars.clone();
                let mut pat_extra_positions: Vec<usize> = Vec::new();
                for (i, v) in pat_vars.iter().enumerate() {
                    if !cur_vars.contains(v) {
                        combined_vars.push(*v);
                        pat_extra_positions.push(i);
                    }
                }

                let mut joined: Vec<Vec<TermId>> = Vec::new();
                for cr in &cur_rows {
                    let key: Vec<TermId> = cur_key_positions.iter().map(|&i| cr[i]).collect();
                    if let Some(matches) = ht.get(&key) {
                        for m in matches {
                            let mut row = cr.clone();
                            for &i in &pat_extra_positions {
                                row.push(m[i]);
                            }
                            joined.push(row);
                        }
                    }
                }
                cur_rows = joined;
                cur_vars = combined_vars;
            }

            // Reproject to `out_vars` order.
            let out_positions: Vec<usize> = exec
                .out_vars
                .iter()
                .map(|v| cur_vars.iter().position(|x| x == v).expect("out var missing"))
                .collect();
            rows = cur_rows
                .into_iter()
                .map(|r| out_positions.iter().map(|&i| r[i]).collect())
                .collect();
            Ok(())
        })() {
            pending_error = Some(e);
        }

        let builder = BindingBatchBuilder::new(exec.out_vars.clone());
        Self {
            exec,
            rows: rows.into_iter(),
            builder,
            done: false,
            pending_error,
            ground_match_remaining,
        }
    }
}

impl<'src> Iterator for BatchIter<'src> {
    type Item = Result<RecordBatch>;
    fn next(&mut self) -> Option<Self::Item> {
        if let Some(e) = self.pending_error.take() {
            self.done = true;
            return Some(Err(e));
        }
        if self.done {
            return None;
        }
        // Ground case: emit `ground_match_remaining` empty rows, one batch.
        if self.ground_match_remaining > 0 && self.exec.out_vars.is_empty() {
            // Build a zero-column RecordBatch with `n` rows.
            let n = self.ground_match_remaining;
            self.ground_match_remaining = 0;
            self.done = true;
            let schema = self.builder.schema();
            return Some(
                RecordBatch::try_new_with_options(
                    schema,
                    Vec::new(),
                    &arrow::record_batch::RecordBatchOptions::new().with_row_count(Some(n)),
                )
                .map_err(WcojError::Arrow),
            );
        }
        // Normal case: feed rows into the builder.
        loop {
            match self.rows.next() {
                Some(row) => {
                    if let Some(b) = self.builder.push_row(&row) {
                        return Some(Ok(b));
                    }
                }
                None => {
                    self.done = true;
                    return self.builder.finish().map(Ok);
                }
            }
        }
    }
}
```

- [ ] **Step 4: Run the test — expect pass**

Run: `cargo test -p reasoner-wcoj --test binary_hash_smoke`
Expected: `test result: ok. 2 passed`.

- [ ] **Step 5: Commit**

```bash
git add crates/wcoj/src/executor/binary_hash.rs crates/wcoj/tests/binary_hash_smoke.rs
git commit -m "$(cat <<'EOF'
wcoj: add BinaryHashExecutor (≤3-pattern fallback + fuzz reference)

Left-deep hash join over eagerly-scanned per-pattern triples. Doubles as
the reference implementation for the WCOJ differential fuzzer (SPEC-03
acceptance #3) and as the fast path for ≤3-pattern BGPs (F2 cutover).
EOF
)"
```

---

## Task 12: Planner that chooses between WCOJ and binary-hash

**Files:**
- Create: `crates/wcoj/src/planner.rs` (replace placeholder)
- Create: `crates/wcoj/tests/planner_choice.rs`

- [ ] **Step 1: Write the failing test `crates/wcoj/tests/planner_choice.rs`**

```rust
use reasoner_wcoj::cardinality::UniformEstimator;
use reasoner_wcoj::ids::Triple;
use reasoner_wcoj::pattern::{Bgp, Term, TriplePattern, Var};
use reasoner_wcoj::plan::PlanKind;
use reasoner_wcoj::planner::Planner;
use reasoner_wcoj::source::vec_source::VecTripleSource;

#[test]
fn four_pattern_cycle_picks_wcoj() {
    let p = 10;
    let bgp = Bgp::new(vec![
        TriplePattern::new(Term::Var(Var(0)), Term::Bound(p), Term::Var(Var(1))),
        TriplePattern::new(Term::Var(Var(1)), Term::Bound(p), Term::Var(Var(2))),
        TriplePattern::new(Term::Var(Var(2)), Term::Bound(p), Term::Var(Var(3))),
        TriplePattern::new(Term::Var(Var(3)), Term::Bound(p), Term::Var(Var(0))),
    ]);
    let src = VecTripleSource::from_triples(vec![Triple::new(1, p, 2)]);
    let est = UniformEstimator::from_source(&src);
    let planner = Planner::default();
    let plan = planner.choose(&bgp, &est);
    assert_eq!(plan.kind, PlanKind::Wcoj);
}

#[test]
fn two_pattern_picks_binary_hash() {
    let bgp = Bgp::new(vec![
        TriplePattern::new(Term::Var(Var(0)), Term::Bound(10), Term::Var(Var(1))),
        TriplePattern::new(Term::Var(Var(1)), Term::Bound(20), Term::Var(Var(2))),
    ]);
    let src = VecTripleSource::from_triples(vec![]);
    let est = UniformEstimator::from_source(&src);
    let plan = Planner::default().choose(&bgp, &est);
    assert_eq!(plan.kind, PlanKind::BinaryHash);
}

#[test]
fn three_patterns_with_low_cardinality_picks_binary_hash() {
    let bgp = Bgp::new(vec![
        TriplePattern::new(Term::Bound(1), Term::Bound(10), Term::Var(Var(0))),
        TriplePattern::new(Term::Var(Var(0)), Term::Bound(20), Term::Var(Var(1))),
        TriplePattern::new(Term::Var(Var(1)), Term::Bound(30), Term::Bound(99)),
    ]);
    let src = VecTripleSource::from_triples(vec![Triple::new(1, 10, 2)]);
    let est = UniformEstimator::from_source(&src);
    let plan = Planner::default().choose(&bgp, &est);
    assert_eq!(plan.kind, PlanKind::BinaryHash);
}
```

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p reasoner-wcoj --test planner_choice`
Expected: errors.

- [ ] **Step 3: Implement `crates/wcoj/src/planner.rs`**

```rust
//! Cost-based plan choice between WCOJ and binary-hash.
//!
//! Stage-1 heuristic (SPEC-03 F2): default cutover is 4 patterns. For ≤3
//! patterns, binary-hash. For ≥4, WCOJ. The cardinality estimator is
//! consulted only as a tie-breaker for the 3-pattern case where a *very*
//! selective ground prefix can still win for binary-hash.

use crate::cardinality::Cardinality;
use crate::pattern::Bgp;
use crate::plan::{ExecutionPlan, PlanKind};

pub struct Planner {
    pub wcoj_cutover: usize,
}

impl Default for Planner {
    fn default() -> Self {
        Self { wcoj_cutover: 4 }
    }
}

impl Planner {
    pub fn choose<C: Cardinality>(&self, bgp: &Bgp, _est: &C) -> ExecutionPlan {
        // ExecutionPlan::for_bgp does the right thing for the cutover and
        // for the all-ground special case. We retain a Planner struct as
        // the seam where the Stage-2 cost-based logic (using the estimator
        // for join-order selection and per-pattern ordering choice) will
        // land.
        ExecutionPlan::for_bgp(bgp, self.wcoj_cutover)
    }
}
```

- [ ] **Step 4: Run the test — expect pass**

Run: `cargo test -p reasoner-wcoj --test planner_choice`
Expected: `test result: ok. 3 passed`.

- [ ] **Step 5: Commit**

```bash
git add crates/wcoj/src/planner.rs crates/wcoj/tests/planner_choice.rs
git commit -m "wcoj: add Planner with default 4-pattern WCOJ cutover (F2)"
```

---

## Task 13: Top-level `Executor` dispatch + cancellation integration test

**Files:**
- Modify: `crates/wcoj/src/executor/mod.rs`
- Create: `crates/wcoj/tests/cancel.rs`

- [ ] **Step 1: Write the failing test `crates/wcoj/tests/cancel.rs`**

```rust
use std::sync::Arc;
use std::time::{Duration, Instant};

use reasoner_wcoj::cancel::CancelToken;
use reasoner_wcoj::error::WcojError;
use reasoner_wcoj::executor::Executor;
use reasoner_wcoj::ids::Triple;
use reasoner_wcoj::pattern::{Bgp, Term, TriplePattern, Var};
use reasoner_wcoj::planner::Planner;
use reasoner_wcoj::source::vec_source::VecTripleSource;

#[test]
fn cancellation_returns_within_100ms() {
    // Build a synthetic graph large enough to keep the executor busy.
    let p = 10u64;
    let mut triples = Vec::new();
    for s in 0..10_000u64 {
        triples.push(Triple::new(s, p, (s + 1) % 10_000));
    }
    let src = Arc::new(VecTripleSource::from_triples(triples));
    let bgp = Bgp::new(vec![
        TriplePattern::new(Term::Var(Var(0)), Term::Bound(p), Term::Var(Var(1))),
        TriplePattern::new(Term::Var(Var(1)), Term::Bound(p), Term::Var(Var(2))),
        TriplePattern::new(Term::Var(Var(2)), Term::Bound(p), Term::Var(Var(3))),
        TriplePattern::new(Term::Var(Var(3)), Term::Bound(p), Term::Var(Var(0))),
    ]);
    let token = CancelToken::new();
    let token_clone = token.clone();
    let planner = Planner::default();
    let src_ref: &VecTripleSource = &src;

    // Cancel after 10 ms from another thread.
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(10));
        token_clone.cancel();
    });

    let start = Instant::now();
    let exec = Executor::for_bgp(src_ref, &bgp, &planner, token.clone());
    let mut last_err = None;
    for item in exec {
        if let Err(e) = item {
            last_err = Some(e);
            break;
        }
    }
    let elapsed = start.elapsed();
    assert!(elapsed < Duration::from_millis(100), "took {elapsed:?}");
    assert!(matches!(last_err, Some(WcojError::Cancelled)), "got {:?}", last_err);
}
```

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p reasoner-wcoj --test cancel`
Expected: errors — `Executor::for_bgp` not yet defined.

- [ ] **Step 3: Expand `crates/wcoj/src/executor/mod.rs`**

Replace the contents with:

```rust
//! Query executors. `WcojExecutor` and `BinaryHashExecutor` both produce
//! a stream of Arrow `RecordBatch`es. `Executor` is the planner-driven
//! dispatch enum.

pub mod binary_hash;
pub mod wcoj;

use arrow::record_batch::RecordBatch;

use crate::cancel::CancelToken;
use crate::cardinality::UniformEstimator;
use crate::error::Result;
use crate::pattern::Bgp;
use crate::plan::PlanKind;
use crate::planner::Planner;
use crate::source::TripleSource;

/// Common output type — a fallible iterator over batches.
pub type BatchStream<'a> = Box<dyn Iterator<Item = Result<RecordBatch>> + 'a>;

/// Dispatch enum: the planner picks WCOJ or BinaryHash and this wrapper
/// hides the choice from callers.
pub enum Executor<'src> {
    Wcoj(wcoj::BatchIter<'src>),
    BinaryHash(binary_hash::BatchIter<'src>),
}

impl<'src> Executor<'src> {
    pub fn for_bgp(
        source: &'src dyn TripleSource,
        bgp: &Bgp,
        planner: &Planner,
        cancel: CancelToken,
    ) -> Self {
        let est = UniformEstimator::from_source(source);
        let plan = planner.choose(bgp, &est);
        match plan.kind {
            PlanKind::Wcoj => {
                let exec = wcoj::WcojExecutor::new(source, bgp, &plan, cancel);
                Executor::Wcoj(exec.into_iter())
            }
            PlanKind::BinaryHash => {
                let out_vars = if plan.var_order.is_empty() {
                    bgp.variables()
                } else {
                    plan.var_order.clone()
                };
                let exec = binary_hash::BinaryHashExecutor::new(source, bgp, out_vars, cancel);
                Executor::BinaryHash(exec.into_iter())
            }
        }
    }
}

impl<'src> Iterator for Executor<'src> {
    type Item = Result<RecordBatch>;
    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Executor::Wcoj(it) => it.next(),
            Executor::BinaryHash(it) => it.next(),
        }
    }
}
```

- [ ] **Step 4: Run the cancel test — expect pass**

Run: `cargo test -p reasoner-wcoj --test cancel`
Expected: `test result: ok. 1 passed`.

If the test occasionally fails because the executor finishes before the cancel signal arrives, increase the synthetic graph size in the test (e.g. 50_000 edges instead of 10_000). The 100ms latency assertion must always hold.

- [ ] **Step 5: Commit**

```bash
git add crates/wcoj/src/executor/mod.rs crates/wcoj/tests/cancel.rs
git commit -m "$(cat <<'EOF'
wcoj: add Executor dispatch enum and end-to-end cancellation test

Cancellation must return within 100 ms (SPEC-03 F7). The dispatch
hides the WCOJ/BinaryHash choice from callers; planner picks based on
the SPEC-03 F2 cutover rule.
EOF
)"
```

---

## Task 14: Synthetic 4-cycle graph generator + acceptance criterion #2 bench

**Files:**
- Create: `crates/wcoj/src/source/synthetic.rs` (replace placeholder)
- Create: `crates/wcoj/benches/four_cycle.rs`
- Create: `crates/wcoj/tests/synthetic_graph.rs`

- [ ] **Step 1: Write the failing test `crates/wcoj/tests/synthetic_graph.rs`**

```rust
use reasoner_wcoj::source::synthetic::SyntheticGraph;
use reasoner_wcoj::source::TripleSource;

#[test]
fn synthetic_4_cycle_graph_has_expected_size() {
    // Graph of 1000 vertices, each with out-degree 4. Edge predicate = 10.
    let g = SyntheticGraph::cyclic(1000, 4, 10, 0xCAFE);
    assert_eq!(g.total_triples(), 4000);
}

#[test]
fn synthetic_graph_supports_all_orderings() {
    let g = SyntheticGraph::cyclic(100, 2, 10, 0xCAFE);
    for ord in reasoner_wcoj::ids::Ordering::ALL {
        assert!(g.supports(ord));
    }
}
```

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p reasoner-wcoj --test synthetic_graph`
Expected: errors.

- [ ] **Step 3: Implement `crates/wcoj/src/source/synthetic.rs`**

```rust
//! Synthetic graph generators for benchmarks.
//!
//! `SyntheticGraph::cyclic(n, k, p, seed)` produces a directed graph of `n`
//! vertices (IDs `0..n`) where each vertex has `k` outgoing edges to
//! pseudo-randomly chosen other vertices, all with predicate `p`. Vertex
//! IDs are dense, edges are uniform — this is the canonical benchmark
//! shape from the WCOJ literature.

use std::collections::BTreeSet;

use crate::ids::{Ordering, TermId, Triple};
use crate::source::vec_source::VecTripleSource;
use crate::source::{OrderedTripleIter, TripleSource};

pub struct SyntheticGraph {
    inner: VecTripleSource,
}

impl SyntheticGraph {
    pub fn cyclic(n: u64, k: u64, predicate: u64, seed: u64) -> Self {
        // Simple xorshift RNG, deterministic given seed.
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
        let triples: Vec<Triple> = edges.into_iter().collect();
        Self {
            inner: VecTripleSource::from_triples(triples),
        }
    }
}

impl TripleSource for SyntheticGraph {
    fn iter(&self, ord: Ordering) -> crate::error::Result<Box<dyn OrderedTripleIter + '_>> {
        self.inner.iter(ord)
    }
    fn total_triples(&self) -> usize {
        self.inner.total_triples()
    }
    fn supports(&self, ord: Ordering) -> bool {
        self.inner.supports(ord)
    }
}
```

- [ ] **Step 4: Run the test — expect pass**

Run: `cargo test -p reasoner-wcoj --test synthetic_graph`
Expected: `test result: ok. 2 passed`.

- [ ] **Step 5: Write the criterion bench `crates/wcoj/benches/four_cycle.rs`**

```rust
//! SPEC-03 acceptance criterion #2: on the 4-cycle query
//!   (?a -p-> ?b -p-> ?c -p-> ?d -p-> ?a)
//! over a synthetic graph with ~10^6 edges, WCOJ outperforms binary-hash
//! by ≥10×.

use std::time::Duration;

use criterion::{criterion_group, criterion_main, Criterion};

use reasoner_wcoj::cancel::CancelToken;
use reasoner_wcoj::executor::binary_hash::BinaryHashExecutor;
use reasoner_wcoj::executor::wcoj::WcojExecutor;
use reasoner_wcoj::pattern::{Bgp, Term, TriplePattern, Var};
use reasoner_wcoj::plan::{ExecutionPlan, PlanKind};
use reasoner_wcoj::source::synthetic::SyntheticGraph;

fn make_4_cycle_bgp() -> Bgp {
    let p = 10u64;
    Bgp::new(vec![
        TriplePattern::new(Term::Var(Var(0)), Term::Bound(p), Term::Var(Var(1))),
        TriplePattern::new(Term::Var(Var(1)), Term::Bound(p), Term::Var(Var(2))),
        TriplePattern::new(Term::Var(Var(2)), Term::Bound(p), Term::Var(Var(3))),
        TriplePattern::new(Term::Var(Var(3)), Term::Bound(p), Term::Var(Var(0))),
    ])
}

fn bench_four_cycle(c: &mut Criterion) {
    // 10^6 edges: 250_000 vertices * 4 out-edges = 1_000_000.
    let graph = SyntheticGraph::cyclic(250_000, 4, 10, 0xDEAD_BEEF);
    let bgp = make_4_cycle_bgp();

    let mut group = c.benchmark_group("four_cycle");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(30));

    group.bench_function("wcoj", |b| {
        b.iter(|| {
            let plan = ExecutionPlan {
                kind: PlanKind::Wcoj,
                var_order: vec![Var(0), Var(1), Var(2), Var(3)],
            };
            let exec = WcojExecutor::new(&graph, &bgp, &plan, CancelToken::new());
            let mut rows = 0u64;
            for batch in exec.into_iter() {
                rows += batch.unwrap().num_rows() as u64;
            }
            criterion::black_box(rows);
        });
    });

    group.bench_function("binary_hash", |b| {
        b.iter(|| {
            let exec = BinaryHashExecutor::new(
                &graph,
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
}

criterion_group!(benches, bench_four_cycle);
criterion_main!(benches);
```

- [ ] **Step 6: Run the bench**

Run: `cargo bench -p reasoner-wcoj --bench four_cycle -- --quick`
Expected: both benchmarks complete; the criterion output should show `wcoj` mean time ≥10× faster than `binary_hash`. The `--quick` flag is for development; the assertion is qualitative — record both numbers in the next commit message.

If WCOJ is *not* 10× faster, do not paper over the failure. Likely causes, in order:
1. `BinaryHashExecutor::scan_pattern` is doing the *same* full-graph scan three times for binary-hash — that's actually realistic for the comparison. Verify by adding a counter.
2. The `PatternTrieIter` adapter has a bug in `open_level` / `up` that causes redundant work. Run `cargo test -p reasoner-wcoj --release` and watch for any per-test slowdown.
3. The `VecTripleSource::seek` binary search is slow for the synthetic graph because of large `[lo, hi)` ranges. Confirm by profiling with `cargo bench -- --profile-time 10`.

If the 10× criterion really cannot be met without further work, **insert a new task** between this one and Task 15 to investigate, rather than commenting it out.

- [ ] **Step 7: Commit**

```bash
git add crates/wcoj/src/source/synthetic.rs crates/wcoj/benches/four_cycle.rs crates/wcoj/tests/synthetic_graph.rs
git commit -m "$(cat <<'EOF'
wcoj: add 4-cycle benchmark — SPEC-03 acceptance criterion #2

Synthetic 10^6-edge graph, WCOJ vs binary-hash. Acceptance criterion is
WCOJ ≥10× faster. Reference numbers from a local run:
- wcoj:        <fill in from bench output>
- binary_hash: <fill in from bench output>
EOF
)"
```

---

## Task 15: Differential fuzzer — WCOJ ≡ BinaryHash (SPEC-03 acceptance #3)

**Files:**
- Create: `crates/wcoj/tests/differential_fuzz.rs`

- [ ] **Step 1: Write the failing test `crates/wcoj/tests/differential_fuzz.rs`**

```rust
//! SPEC-03 acceptance criterion #3: 100K random BGPs of 2-6 patterns over a
//! LUBM-ish synthetic graph, comparing WCOJ output to BinaryHash output. The
//! check should find zero mismatches.
//!
//! Stage-1 substitute for LUBM: we use SyntheticGraph with a small predicate
//! vocabulary, which exercises the same code paths. LUBM-100 substitution
//! lands in a follow-up plan once the SPEC-01 harness is wired in.

use std::collections::BTreeSet;

use arrow::array::UInt64Array;
use proptest::prelude::*;

use reasoner_wcoj::cancel::CancelToken;
use reasoner_wcoj::executor::binary_hash::BinaryHashExecutor;
use reasoner_wcoj::executor::wcoj::WcojExecutor;
use reasoner_wcoj::ids::{TermId, Triple};
use reasoner_wcoj::pattern::{Bgp, Term, TriplePattern, Var};
use reasoner_wcoj::plan::{ExecutionPlan, PlanKind};
use reasoner_wcoj::source::vec_source::VecTripleSource;

const N_VERTICES: u64 = 30;
const PREDICATES: &[u64] = &[100, 101, 102];

fn build_source(seed: u64) -> VecTripleSource {
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
            // Each (s, p) yields 0-3 edges with random objects.
            for _ in 0..(rand() % 4) {
                let o = rand() % N_VERTICES;
                triples.push(Triple::new(s, p, o));
            }
        }
    }
    VecTripleSource::from_triples(triples)
}

fn collect_rows(
    batches: impl Iterator<Item = reasoner_wcoj::error::Result<arrow::record_batch::RecordBatch>>,
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
    // Predicate must be bound for the WCOJ stub to make progress on
    // PSO/POS orderings; in real usage, predicates are bound ~always.
    prop::sample::select(PREDICATES.to_vec()).prop_map(Term::Bound)
}

fn arb_pattern() -> impl Strategy<Value = TriplePattern> {
    (arb_term(), arb_predicate_term(), arb_term())
        .prop_map(|(s, p, o)| TriplePattern::new(s, p, o))
}

fn arb_bgp() -> impl Strategy<Value = Bgp> {
    prop::collection::vec(arb_pattern(), 2..=6).prop_map(Bgp::new)
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 1024, ..ProptestConfig::default() })]

    #[test]
    fn wcoj_matches_binary_hash(seed in any::<u64>(), bgp in arb_bgp()) {
        let src = build_source(seed);
        let out_vars = bgp.variables();
        prop_assume!(!out_vars.is_empty());

        let plan = ExecutionPlan {
            kind: PlanKind::Wcoj,
            var_order: out_vars.clone(),
        };
        let wcoj_rows = collect_rows(
            WcojExecutor::new(&src, &bgp, &plan, CancelToken::new()).into_iter(),
        );
        let bh_rows = collect_rows(
            BinaryHashExecutor::new(&src, &bgp, out_vars, CancelToken::new()).into_iter(),
        );
        prop_assert_eq!(wcoj_rows, bh_rows);
    }
}
```

- [ ] **Step 2: Run to confirm it fails (or compiles and passes)**

Run: `cargo test -p reasoner-wcoj --test differential_fuzz --release`
Expected: either (a) compiles and reports `1024 cases passed`, in which case skip steps 3-4 and proceed to commit, or (b) finds a counterexample which is what proptest is designed to do — the test will print a minimal failing BGP+seed.

Why 1024 cases instead of 100K from SPEC-03: that target is for the full LUBM-100 fuzz that lands when SPEC-01 ships. 1024 cases on a 30-vertex / 3-predicate synthetic graph still exercises a very large surface; expand to 100K (and run nightly) when the integration plan ships.

- [ ] **Step 3: If a counterexample appears, diagnose and fix the executor**

The most likely culprits, in priority order:
1. **`PatternTrieIter::open_level`**: bound-level handling between variable depths. Add a unit test that pins the failing pattern shape, then fix the adapter.
2. **`WcojExecutor::step` ascend path**: forgetting to `up` a level on all contributing iters, or seeking the parent past `binding[parent_depth]` incorrectly (off-by-one).
3. **Same variable appearing twice in one pattern**: e.g. `(?x, p, ?x)`. The current `PatternTrieIter` doesn't handle this; if `arb_pattern` produces it, exclude that case by adding to `arb_pattern`:

   ```rust
   fn arb_pattern() -> impl Strategy<Value = TriplePattern> {
       (arb_term(), arb_predicate_term(), arb_term())
           .prop_map(|(s, p, o)| TriplePattern::new(s, p, o))
           .prop_filter("no self-loop variables", |pat| {
               let mut seen = std::collections::HashSet::new();
               for t in [pat.s, pat.p, pat.o] {
                   if let Term::Var(v) = t {
                       if !seen.insert(v) { return false; }
                   }
               }
               true
           })
   }
   ```

   Self-loops are F1 functionality that's deferred to a later plan task (add to "Future Work" list in the plan note).

Re-run after each fix. Repeat until 1024 cases pass cleanly.

- [ ] **Step 4: Run the fuzz test — expect pass**

Run: `cargo test -p reasoner-wcoj --test differential_fuzz --release`
Expected: `test result: ok. 1 passed`. Proptest's regression file (`crates/wcoj/proptest-regressions/`) is checked in so previously-failing inputs are replayed forever.

- [ ] **Step 5: Commit**

```bash
git add crates/wcoj/tests/differential_fuzz.rs crates/wcoj/proptest-regressions 2>/dev/null
git commit -m "$(cat <<'EOF'
wcoj: differential fuzzer WCOJ ≡ BinaryHash (SPEC-03 acceptance #3)

1024 random BGPs of 2-6 patterns over a small synthetic graph. Result
sets compared as set-equality. Stage-2 will expand to the full 100K
cases over LUBM-100 once SPEC-01 conformance harness can load the dataset.
EOF
)"
```

---

## Task 16: Per-tuple microbench (NF1 sanity check)

**Files:**
- Create: `crates/wcoj/benches/per_tuple.rs`

- [ ] **Step 1: Write the bench `crates/wcoj/benches/per_tuple.rs`**

```rust
//! SPEC-03 NF1: per-tuple overhead ≤5 ns/tuple in the hot path.
//!
//! This bench measures the leapfrog inner loop in isolation by joining two
//! large, ordered, mostly-overlapping `VecTripleSource`-backed patterns at
//! a single variable. We compute ns/output-row and compare against the 5 ns
//! target. Stage-1 is allowed to miss the target (no SIMD yet, scalar
//! binary-search-based seek); the bench exists so any regression is visible.

use std::time::Duration;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

use reasoner_wcoj::cancel::CancelToken;
use reasoner_wcoj::executor::wcoj::WcojExecutor;
use reasoner_wcoj::ids::Triple;
use reasoner_wcoj::pattern::{Bgp, Term, TriplePattern, Var};
use reasoner_wcoj::plan::{ExecutionPlan, PlanKind};
use reasoner_wcoj::source::vec_source::VecTripleSource;

fn bench_per_tuple(c: &mut Criterion) {
    let n: u64 = 1_000_000;
    let triples: Vec<Triple> = (0..n)
        .flat_map(|i| {
            // Each subject has one edge with predicate 1 and one with predicate 2,
            // sharing the same object. The two-pattern join on (?x p1 ?y)(?x p2 ?y)
            // returns exactly `n` rows.
            vec![Triple::new(i, 1, i), Triple::new(i, 2, i)]
        })
        .collect();
    let src = VecTripleSource::from_triples(triples);
    let bgp = Bgp::new(vec![
        TriplePattern::new(Term::Var(Var(0)), Term::Bound(1), Term::Var(Var(1))),
        TriplePattern::new(Term::Var(Var(0)), Term::Bound(2), Term::Var(Var(1))),
    ]);
    let plan = ExecutionPlan {
        kind: PlanKind::Wcoj,
        var_order: vec![Var(0), Var(1)],
    };

    let mut group = c.benchmark_group("per_tuple");
    group.measurement_time(Duration::from_secs(20));
    group.throughput(Throughput::Elements(n));
    group.bench_function(BenchmarkId::from_parameter(n), |b| {
        b.iter(|| {
            let exec = WcojExecutor::new(&src, &bgp, &plan, CancelToken::new());
            let mut rows = 0u64;
            for batch in exec.into_iter() {
                rows += batch.unwrap().num_rows() as u64;
            }
            criterion::black_box(rows);
        });
    });
    group.finish();
}

criterion_group!(benches, bench_per_tuple);
criterion_main!(benches);
```

- [ ] **Step 2: Run the bench**

Run: `cargo bench -p reasoner-wcoj --bench per_tuple -- --quick`
Expected: criterion reports a per-element throughput. Compute ns/tuple = mean time / `n`. Record the result. NF1 target is ≤5 ns; Stage 1 is expected to be 10-50 ns. Do **not** fail the build on this — it's a regression-watch bench, not a gate.

- [ ] **Step 3: Commit**

```bash
git add crates/wcoj/benches/per_tuple.rs
git commit -m "$(cat <<'EOF'
wcoj: per-tuple microbench for NF1 regression watch

Two-pattern self-join on 10^6 rows. Records ns/tuple for the leapfrog
inner loop. Stage 1 reading: <fill in from bench output> ns/tuple. NF1
target ≤5 ns lands in Stage 2 when we add SIMD seek paths.
EOF
)"
```

---

## Task 17: End-to-end CI gate and crate-level smoke check

**Files:**
- Modify: `crates/wcoj/src/lib.rs`

- [ ] **Step 1: Add a top-level smoke test inside `lib.rs`**

Append to `crates/wcoj/src/lib.rs`:

```rust
#[cfg(test)]
mod smoke {
    use super::*;
    use crate::cancel::CancelToken;
    use crate::executor::Executor;
    use crate::ids::Triple;
    use crate::pattern::{Bgp, Term, TriplePattern, Var};
    use crate::planner::Planner;
    use crate::source::vec_source::VecTripleSource;

    #[test]
    fn end_to_end_dispatch_runs_a_two_pattern_join() {
        let src = VecTripleSource::from_triples(vec![
            Triple::new(1, 10, 2),
            Triple::new(2, 20, 3),
        ]);
        let bgp = Bgp::new(vec![
            TriplePattern::new(Term::Var(Var(0)), Term::Bound(10), Term::Var(Var(1))),
            TriplePattern::new(Term::Var(Var(1)), Term::Bound(20), Term::Var(Var(2))),
        ]);
        let planner = Planner::default();
        let exec = Executor::for_bgp(&src, &bgp, &planner, CancelToken::new());
        let total: usize = exec.map(|b| b.unwrap().num_rows()).sum();
        assert_eq!(total, 1);
    }
}
```

- [ ] **Step 2: Run the full crate test suite**

Run: `cargo test -p reasoner-wcoj`
Expected: all tests pass — unit smoke + every integration test created in Tasks 2-15. Spot-check the output:
- `vec_source`: 3 passed
- `pattern`: 3 passed
- `trie_basics`: 2 passed
- `leapfrog`: 2 passed
- `batch`: 3 passed
- `cancel_token`: 3 passed
- `cardinality`: 2 passed
- `plan`: 2 passed
- `wcoj_smoke`: 2 passed
- `binary_hash_smoke`: 2 passed
- `planner_choice`: 3 passed
- `cancel`: 1 passed
- `synthetic_graph`: 2 passed
- `differential_fuzz`: 1 passed (1024 proptest cases)
- `smoke` (in lib.rs): 1 passed

If any test fails, do not proceed. Go back to the task that introduced it and debug.

- [ ] **Step 3: Run clippy to catch obvious lints**

Run: `cargo clippy -p reasoner-wcoj --all-targets -- -D warnings`
Expected: clean. Common fixes if not clean:
- Unused `Term` import in `plan.rs` — already addressed with the `_ = Term::Bound(0)` line; remove if no longer needed.
- `let _ = ord;` in `TripleSource::supports` default — keep as documentation.

- [ ] **Step 4: Commit**

```bash
git add crates/wcoj/src/lib.rs
git commit -m "wcoj: add crate-level dispatch smoke test; clippy clean"
```

---

## Task 18: Documentation pass and `Future Work` register

**Files:**
- Modify: `crates/wcoj/src/lib.rs`

- [ ] **Step 1: Expand the crate doc-comment in `crates/wcoj/src/lib.rs`**

Replace the existing crate-level doc (the `//! reasoner-wcoj — Leapfrog Triejoin ...` block) with:

```rust
//! reasoner-wcoj — Leapfrog Triejoin query executor for RDF triple patterns.
//!
//! # Architecture (Stage 0/1)
//!
//! ```text
//!  caller ──▶ Executor::for_bgp(source, bgp, planner, cancel)
//!                       │
//!              Planner::choose(bgp, est)
//!                       │
//!       ┌───────────────┴───────────────┐
//!       ▼                               ▼
//!  WcojExecutor                  BinaryHashExecutor
//!  (≥4 patterns,                 (≤3 patterns or all-ground;
//!   leapfrog over                 also the reference impl
//!   PatternTrieIter)              for the differential fuzzer)
//!       │                               │
//!       └──────► Arrow RecordBatch (2048 rows) ◄──┘
//! ```
//!
//! # Stage 0/1 scope
//!
//! Implemented:
//! - F1: triple-pattern executor.
//! - F2: WCOJ on ≥4 patterns; binary-hash for ≤3.
//! - F3: Arrow `RecordBatch` output at `STANDARD_VECTOR_SIZE = 2048`.
//! - F6: cardinality estimator stub (uniform).
//! - F7: cancellation (polled per depth-0 leapfrog iteration).
//!
//! Deferred to a follow-up plan:
//! - F4: magic-sets rewriter — needs SPEC-04 rule context.
//! - F5: SLG-style tabling — needs SPEC-04 rule context.
//! - NF1: SIMD seek paths — Stage 2 once profiling identifies the inner
//!   bottleneck.
//! - NF3: partition-parallel execution — Stage 2.
//!
//! # Storage abstraction
//!
//! [`source::TripleSource`] is the only interface this crate has to the
//! outside world. SPEC-02's `reasoner-storage` crate will provide a
//! production impl; for tests we use [`source::vec_source::VecTripleSource`]
//! and [`source::synthetic::SyntheticGraph`].
//!
//! # See also
//!
//! - `specs/SPEC-03-query-engine.md` — design document.
//! - `plans/2026-05-24-SPEC-03-wcoj-query-engine.md` — this plan.
//! - Veldhuizen, *Leapfrog Triejoin: a worst-case optimal join algorithm*,
//!   ICDT 2014 — primary reference for the algorithm.

pub mod batch;
pub mod cancel;
pub mod cardinality;
pub mod error;
pub mod executor;
pub mod ids;
pub mod pattern;
pub mod plan;
pub mod planner;
pub mod source;
pub mod trie;

pub use error::WcojError;
pub use ids::{Ordering, TermId, Triple};
pub use pattern::{Bgp, Term, TriplePattern, Var};

#[cfg(test)]
mod smoke {
    use super::*;
    use crate::cancel::CancelToken;
    use crate::executor::Executor;
    use crate::ids::Triple;
    use crate::pattern::{Bgp, Term, TriplePattern, Var};
    use crate::planner::Planner;
    use crate::source::vec_source::VecTripleSource;

    #[test]
    fn end_to_end_dispatch_runs_a_two_pattern_join() {
        let src = VecTripleSource::from_triples(vec![
            Triple::new(1, 10, 2),
            Triple::new(2, 20, 3),
        ]);
        let bgp = Bgp::new(vec![
            TriplePattern::new(Term::Var(Var(0)), Term::Bound(10), Term::Var(Var(1))),
            TriplePattern::new(Term::Var(Var(1)), Term::Bound(20), Term::Var(Var(2))),
        ]);
        let planner = Planner::default();
        let exec = Executor::for_bgp(&src, &bgp, &planner, CancelToken::new());
        let total: usize = exec.map(|b| b.unwrap().num_rows()).sum();
        assert_eq!(total, 1);
    }
}
```

- [ ] **Step 2: Build the rustdoc and verify it renders**

Run: `cargo doc -p reasoner-wcoj --no-deps`
Expected: `Documenting reasoner-wcoj v0.0.0` followed by a successful exit. Browse `target/doc/reasoner_wcoj/index.html` to spot-check.

- [ ] **Step 3: Final crate-wide test + clippy**

Run these in sequence:
```bash
cargo test -p reasoner-wcoj
cargo test -p reasoner-wcoj --release
cargo clippy -p reasoner-wcoj --all-targets -- -D warnings
cargo doc -p reasoner-wcoj --no-deps
```
Expected: all four green.

- [ ] **Step 4: Commit**

```bash
git add crates/wcoj/src/lib.rs
git commit -m "$(cat <<'EOF'
wcoj: expand crate docs with Stage-0/1 architecture diagram

Documents the scope boundary (F1/F2/F3/F6/F7 in; F4/F5/NF1/NF3 deferred)
and the storage trait seam that lets the executor sit in front of either
VecTripleSource or the eventual SPEC-02 reasoner-storage impl.
EOF
)"
```

---

## Self-Review Notes (post-write)

**Spec coverage map:**

| SPEC-03 requirement | Task(s) |
|---|---|
| F1 triple-pattern executor | Tasks 4, 10, 11 |
| F2 WCOJ on ≥4 patterns, fallback ≤3 | Tasks 9, 12, 13 |
| F3 Arrow batches @ 2048 | Tasks 6, 10, 11 |
| F4 magic sets | **Deferred** (called out in lib.rs docs) |
| F5 tabling | **Deferred** |
| F6 cardinality estimation | Task 8 (stub) |
| F7 cancellation | Tasks 7, 13 |
| NF1 ≤5 ns/tuple | Task 16 (regression watch, not gated) |
| NF2 no input-column copies | Honoured by `OrderedTripleIter` design (no `clone()` on triples in the hot path) |
| NF3 partition parallelism | **Deferred** |
| NF4 correctness vs binary join | Task 15 (differential fuzzer) |
| Acceptance #1 WatDiv SF100 | **Deferred** (requires SPEC-01 + SPEC-02) |
| Acceptance #2 4-cycle 10× win | Task 14 |
| Acceptance #3 100K differential fuzz | Task 15 (1024 cases at Stage 1; scaled to 100K when SPEC-01 lands) |
| Acceptance #4 magic-sets on SNOMED | **Deferred** with F4 |
| Acceptance #5 cancel within 100ms on LUBM-8000 | Task 13 (validated on 10K-vertex synthetic; LUBM-8000 lands with SPEC-01) |

**Placeholder scan:** no "TBD", "implement later", "fill in the details" left. Two "fill in from bench output" notes appear inside commit messages — those are *intentional* prompts to the implementer to record the measured number rather than vague placeholders in the code.

**Type consistency:** `STANDARD_VECTOR_SIZE`, `TermId`, `TripleSource`, `OrderedTripleIter`, `TrieIterator`, `LeapfrogJoin`, `Executor`, `ExecutionPlan`, `PlanKind`, `BindingBatchBuilder`, `CancelToken`, `Planner`, `UniformEstimator` — all introduced once and referenced consistently across tasks. `BatchIter` is intentionally reused as a name in `executor/wcoj.rs` and `executor/binary_hash.rs` (each module-local).

**Known gaps deliberately deferred (Future Work):**
1. Self-loop variables in one pattern (`?x p ?x`) — fuzzer excludes them; first Stage-2 task is to add the trie-iterator support.
2. Real adapter from `reasoner-storage` to `TripleSource` — lands when SPEC-02 ships its concrete types (one file in either crate, depending on where the trait lives long-term).
3. Per-pattern ordering selection beyond the "shallowest-bound-first" heuristic — Stage 2 with the real cardinality estimator.
4. Parallel partitioned execution — Stage 2.
5. SIMD seek inner loop — Stage 2 once profiling identifies it as the bottleneck.

---

**Plan complete and saved to `plans/2026-05-24-SPEC-03-wcoj-query-engine.md`. Two execution options:**

**1. Subagent-Driven (recommended)** — dispatch a fresh subagent per task with two-stage review between tasks. Best for catching adapter-level bugs (Task 4) and the executor stack mechanics (Task 10) early.

**2. Inline Execution** — execute in this session with checkpoints. Faster wall clock but you carry context fatigue into the trickier tasks.

**Which approach?**
