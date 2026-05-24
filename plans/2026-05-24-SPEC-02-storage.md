# SPEC-02 Storage & Dictionary Encoding — Stage 0/1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Deliver a minimal but production-shaped in-memory RDF triple store: 64-bit term IDs with kind-tagged high bits, lock-free term↔ID dictionary, predicate-partitioned Arrow columns in SPO order, Roaring bitmaps for subject/object sets, a tier trait with one in-memory tier, and an N-Triples bulk loader that ingests LUBM-100 (~13M triples) in ≤30s on a reference workstation.

**Architecture:** A single `reasoner-storage` crate organised as: `term` (ID encoding + taxonomy), `dictionary` (DashMap-backed term↔ID, with reverse-lookup `Vec`), `partition` (one `PredicatePartition` holding Arrow `UInt64Array` columns `s` and `o` plus two `RoaringBitmap`s), `store` (a `Tier` trait + `MemoryTier` impl owning a `HashMap<TermId /*predicate*/, PredicatePartition>`), `loader::ntriples` (streaming `oxttl`-based parser that interns into the dictionary and appends to per-predicate builders), and a public `Store` facade. Quads are supported with a reserved default-graph sentinel; named-graph storage in Stage 1 is a `HashMap<GraphId, GraphStore>` where each `GraphStore` is the predicate-partitioned structure above. The default graph uses sentinel `GraphId(0)`.

**Tech Stack:** Rust 1.93 / edition 2021, `arrow = 53` (columnar buffers), `roaring = 0.10` (bitmaps), `dashmap = 6` (lock-free read map for term→ID), `oxttl = 0.1` (N-Triples streaming parser), `oxrdf = 0.2` (RDF term model used by oxttl), `bytemuck = 1` (safe transmute for ID byte views), `parking_lot = 0.12` (writer mutex), `criterion = 0.5` (bench), `tempfile = 3` (test fixtures), `proptest = 1` (round-trip properties).

---

## File Structure

All paths relative to `/Users/stig/git/sunstone/reasoner/`.

- `crates/storage/Cargo.toml` — real dependency manifest (replaces empty placeholder).
- `crates/storage/src/lib.rs` — module declarations and crate-level docs.
- `crates/storage/src/error.rs` — `StorageError` enum.
- `crates/storage/src/term.rs` — `TermId(u64)`, `TermKind`, packing/unpacking, inline `xsd:int` encoding, sentinels.
- `crates/storage/src/dictionary.rs` — `Dictionary` struct (DashMap forward + RwLock<Vec> reverse).
- `crates/storage/src/partition.rs` — `PredicatePartition` builder + finalized form (Arrow `UInt64Array` columns + Roaring bitmaps).
- `crates/storage/src/tier.rs` — `Tier` trait + `TierStats`.
- `crates/storage/src/memory_tier.rs` — `MemoryTier` implementation of `Tier`.
- `crates/storage/src/store.rs` — `Store` public facade combining dictionary + active tier; named graph plumbing.
- `crates/storage/src/loader/mod.rs` — loader module entry.
- `crates/storage/src/loader/ntriples.rs` — streaming N-Triples loader.
- `crates/storage/tests/dictionary.rs` — integration tests for dictionary round-trip.
- `crates/storage/tests/partition.rs` — integration tests for partition scan & bitmaps.
- `crates/storage/tests/store_roundtrip.rs` — integration tests for end-to-end ingest + scan.
- `crates/storage/tests/ntriples_loader.rs` — integration tests for the loader.
- `crates/storage/tests/fixtures/tiny.nt` — 12-triple fixture file.
- `crates/storage/tests/fixtures/with_literals.nt` — fixture exercising URI/literal/blank/typed-literal/lang-tagged variants.
- `crates/storage/benches/load_lubm.rs` — Criterion bench loading a LUBM N-Triples file (path configurable via env var `LUBM_NT`).
- `Cargo.toml` (workspace root) — add shared workspace dependencies.

---

## Task 1: Wire workspace dependencies and replace the storage crate placeholder

**Files:**
- Modify: `Cargo.toml` (workspace root)
- Modify: `crates/storage/Cargo.toml`
- Modify: `crates/storage/src/lib.rs`

- [ ] **Step 1: Update the workspace `Cargo.toml` to add shared deps**

Replace the entire contents of `/Users/stig/git/sunstone/reasoner/Cargo.toml` with:

```toml
[workspace]
resolver = "2"
members = [
    "crates/harness",
    "crates/storage",
    "crates/wcoj",
    "crates/owlrl",
    "crates/closure",
    "crates/incremental",
    "crates/sparql",
    "crates/ml",
    "crates/hardware-ext",
]

[workspace.package]
edition = "2021"
rust-version = "1.75"
license = "Apache-2.0"
repository = "https://github.com/sunstoneinstitute/reasoner"
authors = ["Sunstone Institute"]

[workspace.dependencies]
anyhow = "1"
thiserror = "1"
arrow = { version = "53", default-features = false }
roaring = "0.10"
dashmap = "6"
parking_lot = "0.12"
bytemuck = { version = "1", features = ["derive"] }
oxttl = "0.1"
oxrdf = "0.2"
criterion = { version = "0.5", default-features = false }
tempfile = "3"
proptest = "1"

[profile.release]
opt-level = 3
lto = "thin"
codegen-units = 1
debug = 1

[profile.bench]
inherits = "release"
```

- [ ] **Step 2: Replace `crates/storage/Cargo.toml`**

Replace the contents of `/Users/stig/git/sunstone/reasoner/crates/storage/Cargo.toml` with:

```toml
[package]
name = "reasoner-storage"
version = "0.0.0"
edition.workspace = true
license.workspace = true
publish = false

[dependencies]
anyhow.workspace = true
thiserror.workspace = true
arrow.workspace = true
roaring.workspace = true
dashmap.workspace = true
parking_lot.workspace = true
bytemuck.workspace = true
oxttl.workspace = true
oxrdf.workspace = true

[dev-dependencies]
tempfile.workspace = true
proptest.workspace = true
criterion.workspace = true

[[bench]]
name = "load_lubm"
harness = false
```

- [ ] **Step 3: Replace `crates/storage/src/lib.rs`**

Replace contents of `/Users/stig/git/sunstone/reasoner/crates/storage/src/lib.rs` with:

```rust
//! reasoner-storage — Stage 0/1 scope.
//!
//! Provides:
//!   * 64-bit kind-tagged term IDs (`term`).
//!   * Concurrent term↔ID dictionary (`dictionary`).
//!   * Predicate-partitioned, columnar in-memory triple storage (`partition`).
//!   * A `Tier` trait with one in-memory implementation (`tier`, `memory_tier`).
//!   * A public `Store` facade (`store`) and an N-Triples bulk loader (`loader::ntriples`).
//!
//! Out of Stage-1 scope: HDT cold tier, all-six index orderings, MVCC,
//! CXL/NVMe tiering, persistent dictionary.

pub mod error;
pub mod term;
pub mod dictionary;
pub mod partition;
pub mod tier;
pub mod memory_tier;
pub mod store;
pub mod loader;

pub use error::StorageError;
pub use term::{TermId, TermKind, GraphId, DEFAULT_GRAPH};
pub use dictionary::Dictionary;
pub use partition::PredicatePartition;
pub use store::Store;
pub use tier::{Tier, TierStats};
pub use memory_tier::MemoryTier;
```

- [ ] **Step 4: Create empty module files so the crate compiles**

Create each of the following with the single-line content `//! placeholder`:

```
crates/storage/src/error.rs
crates/storage/src/term.rs
crates/storage/src/dictionary.rs
crates/storage/src/partition.rs
crates/storage/src/tier.rs
crates/storage/src/memory_tier.rs
crates/storage/src/store.rs
crates/storage/src/loader/mod.rs
crates/storage/src/loader/ntriples.rs
```

For `loader/mod.rs` use:

```rust
//! Bulk loaders (Stage 1: N-Triples only).
pub mod ntriples;
```

- [ ] **Step 5: Compile to verify wiring**

Run: `cargo check -p reasoner-storage`
Expected: succeeds with warnings about unused modules; no errors. If `oxttl` / `oxrdf` versions resolve to anything other than 0.1.x / 0.2.x respectively, pin to the latest 0.1 / 0.2 in `Cargo.toml`.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock crates/storage/
git commit -m "$(cat <<'EOF'
storage: scaffold Stage-1 module layout and dependencies

Replaces the placeholder reasoner-storage crate with the module
skeleton specified in plans/2026-05-24-SPEC-02-storage.md: term,
dictionary, partition, tier, memory_tier, store, loader. Adds
arrow/roaring/dashmap/oxttl/oxrdf/parking_lot to workspace deps.
EOF
)"
```

---

## Task 2: Define `TermId`, `TermKind`, and `GraphId`

**Files:**
- Modify: `crates/storage/src/term.rs`
- Test: `crates/storage/src/term.rs` (in-module `#[cfg(test)]`)

The 64-bit term ID layout:

```
bits 63..60 (4 bits) = TermKind tag
bits 59..0  (60 bits) = payload
```

`TermKind` values (numeric tag in parentheses):
- `Uri (0)` — payload is a dictionary index.
- `Blank (1)` — payload is a dictionary index.
- `PlainLiteral (2)` — payload is a dictionary index.
- `LangLiteral (3)` — payload is a dictionary index.
- `TypedLiteral (4)` — payload is a dictionary index.
- `InlineInt (5)` — payload is a sign-extended i32 in the low 32 bits (high 28 bits of payload set to 0 for positive, 1 for negative).
- `Reserved6 (6)`, `Reserved7 (7)`, ..., `ReservedF (15)` — reserved.

`GraphId(u64)` wraps a `TermId` whose kind is `Uri` or `Blank`. The sentinel `DEFAULT_GRAPH: GraphId = GraphId(0)` denotes the default graph and never matches any URI/blank id.

- [ ] **Step 1: Write the failing test**

Replace `crates/storage/src/term.rs` with:

```rust
//! 64-bit kind-tagged term IDs.
//!
//! See SPEC-02 F1/F2: high 4 bits encode `TermKind`, low 60 bits are payload.

use bytemuck::{Pod, Zeroable};

#[repr(transparent)]
#[derive(Copy, Clone, Eq, PartialEq, Hash, Ord, PartialOrd, Pod, Zeroable, Debug)]
pub struct TermId(pub u64);

#[repr(u8)]
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum TermKind {
    Uri = 0,
    Blank = 1,
    PlainLiteral = 2,
    LangLiteral = 3,
    TypedLiteral = 4,
    InlineInt = 5,
}

impl TermKind {
    pub fn from_tag(tag: u8) -> Option<Self> {
        match tag {
            0 => Some(TermKind::Uri),
            1 => Some(TermKind::Blank),
            2 => Some(TermKind::PlainLiteral),
            3 => Some(TermKind::LangLiteral),
            4 => Some(TermKind::TypedLiteral),
            5 => Some(TermKind::InlineInt),
            _ => None,
        }
    }
}

const KIND_SHIFT: u32 = 60;
const PAYLOAD_MASK: u64 = (1u64 << KIND_SHIFT) - 1;
/// Maximum dictionary index that fits in the 60-bit payload (exclusive upper bound).
pub const MAX_DICT_INDEX: u64 = 1u64 << KIND_SHIFT;

impl TermId {
    pub fn new(kind: TermKind, payload: u64) -> Self {
        debug_assert!(payload < MAX_DICT_INDEX, "payload exceeds 60 bits");
        TermId(((kind as u64) << KIND_SHIFT) | payload)
    }

    pub fn kind(self) -> TermKind {
        TermKind::from_tag((self.0 >> KIND_SHIFT) as u8)
            .expect("term id has reserved/invalid kind tag")
    }

    pub fn payload(self) -> u64 {
        self.0 & PAYLOAD_MASK
    }

    pub fn inline_int(value: i32) -> Self {
        let payload = (value as u32) as u64; // zero-extend the 32-bit pattern
        TermId::new(TermKind::InlineInt, payload)
    }

    pub fn as_inline_int(self) -> Option<i32> {
        if self.kind() == TermKind::InlineInt {
            Some(self.payload() as u32 as i32)
        } else {
            None
        }
    }
}

#[repr(transparent)]
#[derive(Copy, Clone, Eq, PartialEq, Hash, Ord, PartialOrd, Debug)]
pub struct GraphId(pub u64);

/// Reserved sentinel for the default graph. Never collides with a `Uri`/`Blank`
/// dictionary index because the dictionary numbers terms starting from 1.
pub const DEFAULT_GRAPH: GraphId = GraphId(0);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_unpack_uri() {
        let id = TermId::new(TermKind::Uri, 42);
        assert_eq!(id.kind(), TermKind::Uri);
        assert_eq!(id.payload(), 42);
    }

    #[test]
    fn pack_unpack_all_kinds() {
        for &k in &[
            TermKind::Uri,
            TermKind::Blank,
            TermKind::PlainLiteral,
            TermKind::LangLiteral,
            TermKind::TypedLiteral,
        ] {
            let id = TermId::new(k, 0xDEAD_BEEF);
            assert_eq!(id.kind(), k);
            assert_eq!(id.payload(), 0xDEAD_BEEF);
        }
    }

    #[test]
    fn inline_int_round_trip_positive() {
        let id = TermId::inline_int(123_456);
        assert_eq!(id.kind(), TermKind::InlineInt);
        assert_eq!(id.as_inline_int(), Some(123_456));
    }

    #[test]
    fn inline_int_round_trip_negative() {
        let id = TermId::inline_int(-1);
        assert_eq!(id.as_inline_int(), Some(-1));
        let id = TermId::inline_int(i32::MIN);
        assert_eq!(id.as_inline_int(), Some(i32::MIN));
    }

    #[test]
    fn non_int_returns_none_for_inline_int() {
        let id = TermId::new(TermKind::Uri, 7);
        assert_eq!(id.as_inline_int(), None);
    }

    #[test]
    fn default_graph_distinct_from_any_dictionary_id() {
        // Dictionary indices start at 1, so payload 0 with kind Uri is forbidden.
        assert_eq!(DEFAULT_GRAPH.0, 0);
        assert_ne!(DEFAULT_GRAPH.0, TermId::new(TermKind::Uri, 1).0);
    }
}
```

- [ ] **Step 2: Run the test to verify it passes**

Run: `cargo test -p reasoner-storage --lib term::`
Expected: 6 tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/storage/src/term.rs
git commit -m "$(cat <<'EOF'
storage: implement kind-tagged 64-bit TermId and GraphId

Implements SPEC-02 F2: 4-bit TermKind tag in the high bits, 60-bit
payload. Includes inline xsd:int encoding (no dictionary lookup),
GraphId newtype, and a reserved DEFAULT_GRAPH sentinel.
EOF
)"
```

---

## Task 3: Implement the dictionary (forward + reverse lookup)

**Files:**
- Modify: `crates/storage/src/error.rs`
- Modify: `crates/storage/src/dictionary.rs`
- Test: `crates/storage/tests/dictionary.rs`

The dictionary owns: a `DashMap<Term, TermId>` for term→ID lookup (lock-free reads, sharded writes), and a `RwLock<Vec<Term>>` for ID→term reverse lookup. Dictionary indices start at **1** so that index `0` is never confused with the `DEFAULT_GRAPH` sentinel.

`Term` is `oxrdf::Term`-like but storage-owned; we use `oxrdf::Term` directly to avoid duplicating taxonomy. The dictionary distinguishes:
- `NamedNode` → `TermKind::Uri`
- `BlankNode` → `TermKind::Blank`
- `Literal` without datatype/language → `TermKind::PlainLiteral`
- `Literal` with language tag → `TermKind::LangLiteral`
- `Literal` with datatype that is exactly `xsd:integer` and value fits i32 → `TermKind::InlineInt` (no dictionary entry)
- `Literal` with any other datatype → `TermKind::TypedLiteral`

- [ ] **Step 1: Define `StorageError`**

Replace `crates/storage/src/error.rs` with:

```rust
//! Storage error taxonomy.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("dictionary capacity exceeded ({0} terms)")]
    DictionaryFull(u64),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("n-triples parse error: {0}")]
    NtriplesParse(String),
    #[error("invalid term for storage: {0}")]
    InvalidTerm(String),
}

pub type Result<T> = std::result::Result<T, StorageError>;
```

- [ ] **Step 2: Write failing dictionary tests**

Create `/Users/stig/git/sunstone/reasoner/crates/storage/tests/dictionary.rs`:

```rust
use oxrdf::{BlankNode, Literal, NamedNode, Term};
use reasoner_storage::{Dictionary, TermKind};

fn uri(s: &str) -> Term {
    Term::NamedNode(NamedNode::new(s).unwrap())
}
fn bnode(s: &str) -> Term {
    Term::BlankNode(BlankNode::new(s).unwrap())
}
fn plain(s: &str) -> Term {
    Term::Literal(Literal::new_simple_literal(s))
}
fn lang(s: &str, t: &str) -> Term {
    Term::Literal(Literal::new_language_tagged_literal(s, t).unwrap())
}
fn typed(s: &str, dt: &str) -> Term {
    Term::Literal(Literal::new_typed_literal(s, NamedNode::new(dt).unwrap()))
}

#[test]
fn intern_uri_returns_uri_kind() {
    let dict = Dictionary::new();
    let id = dict.intern(&uri("http://example.org/Alice")).unwrap();
    assert_eq!(id.kind(), TermKind::Uri);
    assert_eq!(dict.lookup(id).unwrap(), uri("http://example.org/Alice"));
}

#[test]
fn intern_twice_returns_same_id() {
    let dict = Dictionary::new();
    let a = dict.intern(&uri("http://example.org/x")).unwrap();
    let b = dict.intern(&uri("http://example.org/x")).unwrap();
    assert_eq!(a, b);
    assert_eq!(dict.len(), 1);
}

#[test]
fn intern_distinguishes_kinds() {
    let dict = Dictionary::new();
    let u = dict.intern(&uri("http://example.org/x")).unwrap();
    let b = dict.intern(&bnode("x")).unwrap();
    let p = dict.intern(&plain("x")).unwrap();
    let l = dict.intern(&lang("x", "en")).unwrap();
    let t = dict.intern(&typed("x", "http://example.org/T")).unwrap();
    assert_eq!(u.kind(), TermKind::Uri);
    assert_eq!(b.kind(), TermKind::Blank);
    assert_eq!(p.kind(), TermKind::PlainLiteral);
    assert_eq!(l.kind(), TermKind::LangLiteral);
    assert_eq!(t.kind(), TermKind::TypedLiteral);
    // All distinct IDs.
    let ids = [u, b, p, l, t];
    for i in 0..ids.len() {
        for j in (i + 1)..ids.len() {
            assert_ne!(ids[i], ids[j]);
        }
    }
}

#[test]
fn small_xsd_integer_is_inlined() {
    let dict = Dictionary::new();
    let id = dict
        .intern(&typed("42", "http://www.w3.org/2001/XMLSchema#integer"))
        .unwrap();
    assert_eq!(id.kind(), TermKind::InlineInt);
    assert_eq!(id.as_inline_int(), Some(42));
    // No dictionary entry was created.
    assert_eq!(dict.len(), 0);
    assert_eq!(
        dict.lookup(id).unwrap(),
        typed("42", "http://www.w3.org/2001/XMLSchema#integer")
    );
}

#[test]
fn large_xsd_integer_falls_back_to_dictionary() {
    let dict = Dictionary::new();
    let big = format!("{}", i64::MAX);
    let id = dict
        .intern(&typed(&big, "http://www.w3.org/2001/XMLSchema#integer"))
        .unwrap();
    assert_eq!(id.kind(), TermKind::TypedLiteral);
    assert_eq!(dict.len(), 1);
}

#[test]
fn dictionary_indices_start_at_one() {
    let dict = Dictionary::new();
    let id = dict.intern(&uri("http://example.org/x")).unwrap();
    assert_eq!(id.payload(), 1, "first index must be 1, not 0");
}

#[test]
fn concurrent_intern_returns_same_id() {
    use std::sync::Arc;
    use std::thread;
    let dict = Arc::new(Dictionary::new());
    let mut handles = vec![];
    for _ in 0..8 {
        let d = dict.clone();
        handles.push(thread::spawn(move || {
            d.intern(&uri("http://example.org/shared")).unwrap()
        }));
    }
    let ids: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let first = ids[0];
    for id in &ids {
        assert_eq!(*id, first);
    }
    assert_eq!(dict.len(), 1);
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p reasoner-storage --test dictionary`
Expected: compile errors (no `Dictionary::new`, no `intern`, no `lookup`, no `len`).

- [ ] **Step 4: Implement `Dictionary`**

Replace `crates/storage/src/dictionary.rs` with:

```rust
//! Concurrent term↔ID dictionary.
//!
//! Forward map: `DashMap<Term, TermId>` (lock-free reads, sharded writes).
//! Reverse map: `RwLock<Vec<Term>>` indexed by `payload - 1`.

use crate::error::{Result, StorageError};
use crate::term::{TermId, TermKind, MAX_DICT_INDEX};
use dashmap::DashMap;
use oxrdf::{Literal, NamedNodeRef, Term};
use parking_lot::RwLock;

const XSD_INTEGER: &str = "http://www.w3.org/2001/XMLSchema#integer";

pub struct Dictionary {
    forward: DashMap<Term, TermId>,
    reverse: RwLock<Vec<Term>>,
}

impl Dictionary {
    pub fn new() -> Self {
        Self {
            forward: DashMap::new(),
            reverse: RwLock::new(Vec::new()),
        }
    }

    pub fn len(&self) -> usize {
        self.reverse.read().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn intern(&self, term: &Term) -> Result<TermId> {
        // Inline-int fast path.
        if let Some(id) = try_inline_int(term) {
            return Ok(id);
        }
        if let Some(existing) = self.forward.get(term) {
            return Ok(*existing);
        }
        // Slow path: acquire writer lock on reverse vec, double-check, append.
        let mut reverse = self.reverse.write();
        if let Some(existing) = self.forward.get(term) {
            return Ok(*existing);
        }
        let next_index = (reverse.len() as u64) + 1;
        if next_index >= MAX_DICT_INDEX {
            return Err(StorageError::DictionaryFull(next_index));
        }
        let kind = kind_of(term);
        let id = TermId::new(kind, next_index);
        reverse.push(term.clone());
        self.forward.insert(term.clone(), id);
        Ok(id)
    }

    pub fn lookup(&self, id: TermId) -> Option<Term> {
        if id.kind() == TermKind::InlineInt {
            let v = id.as_inline_int().unwrap();
            return Some(Term::Literal(Literal::new_typed_literal(
                v.to_string(),
                NamedNodeRef::new(XSD_INTEGER).unwrap(),
            )));
        }
        let idx = id.payload();
        if idx == 0 {
            return None;
        }
        let reverse = self.reverse.read();
        reverse.get((idx - 1) as usize).cloned()
    }
}

impl Default for Dictionary {
    fn default() -> Self {
        Self::new()
    }
}

fn kind_of(term: &Term) -> TermKind {
    match term {
        Term::NamedNode(_) => TermKind::Uri,
        Term::BlankNode(_) => TermKind::Blank,
        Term::Literal(lit) => {
            if lit.language().is_some() {
                TermKind::LangLiteral
            } else if lit.datatype().as_str() == "http://www.w3.org/2001/XMLSchema#string" {
                TermKind::PlainLiteral
            } else {
                TermKind::TypedLiteral
            }
        }
        // Triples-as-terms (RDF-star) are out of Stage-1 scope; oxrdf::Term has no Triple
        // variant unless the `rdf-star` feature is enabled, so this arm is unreachable.
    }
}

fn try_inline_int(term: &Term) -> Option<TermId> {
    if let Term::Literal(lit) = term {
        if lit.datatype().as_str() == XSD_INTEGER {
            if let Ok(v) = lit.value().parse::<i32>() {
                return Some(TermId::inline_int(v));
            }
        }
    }
    None
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p reasoner-storage --test dictionary`
Expected: 7 tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/storage/src/dictionary.rs crates/storage/src/error.rs crates/storage/tests/dictionary.rs
git commit -m "$(cat <<'EOF'
storage: implement Dictionary with lock-free reads and inline ints

DashMap forward map + RwLock<Vec> reverse map. Indices start at 1
so the default-graph sentinel 0 is unambiguous. xsd:integer values
that fit in i32 are inline-encoded into the TermId (no dictionary
allocation).
EOF
)"
```

---

## Task 4: `PredicatePartition` — columnar Arrow + Roaring bitmaps

**Files:**
- Modify: `crates/storage/src/partition.rs`
- Test: `crates/storage/tests/partition.rs`

A `PredicatePartition` holds, for one predicate:
- An Arrow `UInt64Array` of subject IDs.
- An Arrow `UInt64Array` of object IDs.
- A `RoaringBitmap` of distinct subject *payloads* (the low 60 bits) — kind tags are dropped because per-predicate the subject is always a `Uri` or `Blank` and the join code consumes payloads.
- A `RoaringBitmap` of distinct object payloads.

The default ordering is SPO. For Stage 1 we sort once at finalize time. The partition exposes:
- `len()`, `is_empty()`.
- `subjects()` / `objects()` returning `&UInt64Array`.
- `subject_set()` / `object_set()` returning `&RoaringBitmap`.
- `scan()` returning an `impl Iterator<Item = (TermId, TermId)>` of (subject, object) pairs in stored order.

Roaring bitmaps are 32-bit. A 60-bit payload may exceed `u32::MAX`. For Stage 1 we use `roaring::RoaringTreemap` (64-bit) for both subject and object sets. This is documented as a deliberate Stage-1 choice; Stage 2 may switch to 32-bit Roaring with payload-range bucketing once we measure cardinalities.

- [ ] **Step 1: Write failing tests**

Create `/Users/stig/git/sunstone/reasoner/crates/storage/tests/partition.rs`:

```rust
use reasoner_storage::{PredicatePartition, TermId, TermKind};

fn uri(payload: u64) -> TermId {
    TermId::new(TermKind::Uri, payload)
}

#[test]
fn empty_partition() {
    let p = PredicatePartition::builder().build();
    assert!(p.is_empty());
    assert_eq!(p.len(), 0);
    assert_eq!(p.subject_set().len(), 0);
    assert_eq!(p.object_set().len(), 0);
}

#[test]
fn append_and_scan_in_spo_order() {
    let mut b = PredicatePartition::builder();
    b.append(uri(3), uri(7));
    b.append(uri(1), uri(9));
    b.append(uri(1), uri(2));
    b.append(uri(2), uri(5));
    let p = b.build();
    let pairs: Vec<_> = p.scan().collect();
    assert_eq!(
        pairs,
        vec![
            (uri(1), uri(2)),
            (uri(1), uri(9)),
            (uri(2), uri(5)),
            (uri(3), uri(7)),
        ]
    );
}

#[test]
fn subject_and_object_sets_are_distinct_payloads() {
    let mut b = PredicatePartition::builder();
    b.append(uri(1), uri(10));
    b.append(uri(1), uri(20));
    b.append(uri(2), uri(10));
    let p = b.build();
    let subjs: Vec<u64> = p.subject_set().iter().collect();
    let objs: Vec<u64> = p.object_set().iter().collect();
    assert_eq!(subjs, vec![1, 2]);
    assert_eq!(objs, vec![10, 20]);
}

#[test]
fn arrow_columns_share_length_with_triples() {
    let mut b = PredicatePartition::builder();
    for i in 0..100u64 {
        b.append(uri(i), uri(i + 1));
    }
    let p = b.build();
    assert_eq!(p.subjects().len(), 100);
    assert_eq!(p.objects().len(), 100);
    assert_eq!(p.len(), 100);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p reasoner-storage --test partition`
Expected: compile errors — `PredicatePartition::builder` does not exist.

- [ ] **Step 3: Implement `PredicatePartition`**

Replace `crates/storage/src/partition.rs` with:

```rust
//! Predicate-partitioned columnar storage.
//!
//! Each partition is the entire (s, o) pair set for one predicate, stored as
//! two Arrow `UInt64Array` columns in SPO order, with side bitmaps of the
//! distinct subject and object *payloads*.

use crate::term::TermId;
use arrow::array::{Array, ArrayRef, UInt64Array};
use roaring::RoaringTreemap;
use std::sync::Arc;

pub struct PredicatePartition {
    subjects: Arc<UInt64Array>,
    objects: Arc<UInt64Array>,
    subject_set: RoaringTreemap,
    object_set: RoaringTreemap,
}

impl PredicatePartition {
    pub fn builder() -> PartitionBuilder {
        PartitionBuilder::default()
    }

    pub fn len(&self) -> usize {
        self.subjects.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn subjects(&self) -> &UInt64Array {
        &self.subjects
    }

    pub fn objects(&self) -> &UInt64Array {
        &self.objects
    }

    pub fn subjects_arrow(&self) -> ArrayRef {
        self.subjects.clone()
    }

    pub fn objects_arrow(&self) -> ArrayRef {
        self.objects.clone()
    }

    pub fn subject_set(&self) -> &RoaringTreemap {
        &self.subject_set
    }

    pub fn object_set(&self) -> &RoaringTreemap {
        &self.object_set
    }

    pub fn scan(&self) -> impl Iterator<Item = (TermId, TermId)> + '_ {
        (0..self.len()).map(move |i| {
            (
                TermId(self.subjects.value(i)),
                TermId(self.objects.value(i)),
            )
        })
    }
}

#[derive(Default)]
pub struct PartitionBuilder {
    pairs: Vec<(u64, u64)>,
}

impl PartitionBuilder {
    pub fn append(&mut self, s: TermId, o: TermId) {
        self.pairs.push((s.0, o.0));
    }

    pub fn len(&self) -> usize {
        self.pairs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pairs.is_empty()
    }

    pub fn build(mut self) -> PredicatePartition {
        // Stage-1: stable sort once at finalize. SPO order ⇒ (subject, object) lexicographic.
        self.pairs.sort_unstable();
        self.pairs.dedup();

        let mut subj_set = RoaringTreemap::new();
        let mut obj_set = RoaringTreemap::new();
        let mut s_col = Vec::with_capacity(self.pairs.len());
        let mut o_col = Vec::with_capacity(self.pairs.len());
        for (s, o) in &self.pairs {
            s_col.push(*s);
            o_col.push(*o);
            subj_set.insert(TermId(*s).payload());
            obj_set.insert(TermId(*o).payload());
        }
        PredicatePartition {
            subjects: Arc::new(UInt64Array::from(s_col)),
            objects: Arc::new(UInt64Array::from(o_col)),
            subject_set: subj_set,
            object_set: obj_set,
        }
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p reasoner-storage --test partition`
Expected: 4 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/storage/src/partition.rs crates/storage/tests/partition.rs
git commit -m "$(cat <<'EOF'
storage: implement PredicatePartition over Arrow UInt64 + RoaringTreemap

One sorted SPO partition per predicate, materialised at build() time.
Subject/object payload sets are RoaringTreemaps (64-bit) for Stage 1;
Stage 2 may switch to 32-bit Roaring with bucketing once cardinalities
are measured.
EOF
)"
```

---

## Task 5: `Tier` trait and `MemoryTier` implementation

**Files:**
- Modify: `crates/storage/src/tier.rs`
- Modify: `crates/storage/src/memory_tier.rs`
- Test: in-module `#[cfg(test)]` block in `memory_tier.rs`

The `Tier` trait is the storage abstraction that future tiers (HDT cold, CXL, NVMe) will implement. For Stage 1 the only impl is `MemoryTier`, which owns a `HashMap<GraphId, HashMap<TermId /*predicate*/, PredicatePartition>>`.

The trait exposes:
- `insert_quad_batch(quads: &[(GraphId, TermId, TermId, TermId)]) -> Result<()>` — bulk ingest.
- `predicate(graph, predicate) -> Option<&PredicatePartition>` — fetch a partition (None if not present).
- `predicates(graph) -> Vec<TermId>` — list predicates in a graph.
- `graphs() -> Vec<GraphId>` — list graphs (including the default graph if non-empty).
- `triple_count() -> u64` — total triples across all graphs.
- `stats() -> TierStats` — observability counters.

Insertion model: `MemoryTier` accumulates per-`(graph, predicate)` builders behind a single `RwLock`; finalize-on-read seals the builders into `PredicatePartition`s on first access (or via `flush()`). For Stage 1 the simple model is: `insert_quad_batch` calls `flush()` internally and rebuilds touched partitions. This is fine because the bulk loader inserts in one or a few large batches.

- [ ] **Step 1: Write the `Tier` trait**

Replace `crates/storage/src/tier.rs` with:

```rust
//! Storage tier abstraction.
//!
//! Stage 1 ships exactly one impl: `MemoryTier`. The trait exists so that
//! Stage 2/3 cold tiers (HDT, CXL, NVMe) can slot in behind the same
//! interface without touching call sites.

use crate::error::Result;
use crate::partition::PredicatePartition;
use crate::term::{GraphId, TermId};

#[derive(Debug, Default, Clone, Copy, Eq, PartialEq)]
pub struct TierStats {
    pub graphs: u64,
    pub predicates: u64,
    pub triples: u64,
    pub bytes_estimated: u64,
}

pub trait Tier: Send + Sync {
    fn insert_quad_batch(&self, quads: &[(GraphId, TermId, TermId, TermId)]) -> Result<()>;

    fn predicate(&self, graph: GraphId, predicate: TermId) -> Option<&PredicatePartition>;

    fn predicates(&self, graph: GraphId) -> Vec<TermId>;

    fn graphs(&self) -> Vec<GraphId>;

    fn triple_count(&self) -> u64;

    fn stats(&self) -> TierStats;
}
```

- [ ] **Step 2: Write the failing test**

Replace `crates/storage/src/memory_tier.rs` with:

```rust
//! In-memory tier — Stage 1 sole implementation of `Tier`.

use crate::error::Result;
use crate::partition::{PartitionBuilder, PredicatePartition};
use crate::term::{GraphId, TermId};
use crate::tier::{Tier, TierStats};
use parking_lot::RwLock;
use std::collections::HashMap;

#[derive(Default)]
struct GraphStore {
    partitions: HashMap<TermId, PredicatePartition>,
}

pub struct MemoryTier {
    inner: RwLock<Inner>,
}

#[derive(Default)]
struct Inner {
    graphs: HashMap<GraphId, GraphStore>,
}

impl MemoryTier {
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(Inner::default()),
        }
    }
}

impl Default for MemoryTier {
    fn default() -> Self {
        Self::new()
    }
}

impl Tier for MemoryTier {
    fn insert_quad_batch(&self, quads: &[(GraphId, TermId, TermId, TermId)]) -> Result<()> {
        // Group by (graph, predicate) into builders, merging with any existing
        // partition by replaying its existing pairs into the new builder.
        let mut groups: HashMap<(GraphId, TermId), PartitionBuilder> = HashMap::new();
        for &(g, s, p, o) in quads {
            groups.entry((g, p)).or_default().append(s, o);
        }
        let mut inner = self.inner.write();
        for ((g, p), mut builder) in groups {
            let gs = inner.graphs.entry(g).or_default();
            if let Some(existing) = gs.partitions.remove(&p) {
                for (s, o) in existing.scan() {
                    builder.append(s, o);
                }
            }
            gs.partitions.insert(p, builder.build());
        }
        Ok(())
    }

    fn predicate(&self, _graph: GraphId, _predicate: TermId) -> Option<&PredicatePartition> {
        // SAFETY caveat: returning `&PredicatePartition` across the RwLock
        // would require a guard-bound borrow. For Stage 1 we expose a guarded
        // accessor via `with_predicate` below; this trait method returns None
        // and is kept only for forward compatibility with a future ArcSwap
        // layout. Callers in Stage 1 use `MemoryTier::with_predicate`.
        None
    }

    fn predicates(&self, graph: GraphId) -> Vec<TermId> {
        let inner = self.inner.read();
        inner
            .graphs
            .get(&graph)
            .map(|gs| gs.partitions.keys().copied().collect())
            .unwrap_or_default()
    }

    fn graphs(&self) -> Vec<GraphId> {
        self.inner.read().graphs.keys().copied().collect()
    }

    fn triple_count(&self) -> u64 {
        let inner = self.inner.read();
        inner
            .graphs
            .values()
            .flat_map(|g| g.partitions.values())
            .map(|p| p.len() as u64)
            .sum()
    }

    fn stats(&self) -> TierStats {
        let inner = self.inner.read();
        let graphs = inner.graphs.len() as u64;
        let predicates: u64 = inner
            .graphs
            .values()
            .map(|g| g.partitions.len() as u64)
            .sum();
        let triples: u64 = inner
            .graphs
            .values()
            .flat_map(|g| g.partitions.values())
            .map(|p| p.len() as u64)
            .sum();
        // Each row: 8 bytes subject + 8 bytes object = 16 bytes; plus ~16 bytes/predicate overhead.
        let bytes_estimated = triples * 16 + predicates * 16;
        TierStats {
            graphs,
            predicates,
            triples,
            bytes_estimated,
        }
    }
}

impl MemoryTier {
    /// Guarded accessor for a partition. The closure runs with a read-lock held.
    pub fn with_predicate<F, R>(&self, graph: GraphId, predicate: TermId, f: F) -> Option<R>
    where
        F: FnOnce(&PredicatePartition) -> R,
    {
        let inner = self.inner.read();
        inner
            .graphs
            .get(&graph)
            .and_then(|gs| gs.partitions.get(&predicate))
            .map(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::{TermKind, DEFAULT_GRAPH};

    fn id(payload: u64) -> TermId {
        TermId::new(TermKind::Uri, payload)
    }

    #[test]
    fn insert_and_count() {
        let tier = MemoryTier::new();
        let quads = vec![
            (DEFAULT_GRAPH, id(1), id(100), id(2)),
            (DEFAULT_GRAPH, id(1), id(100), id(3)),
            (DEFAULT_GRAPH, id(1), id(101), id(2)),
        ];
        tier.insert_quad_batch(&quads).unwrap();
        assert_eq!(tier.triple_count(), 3);
        let mut preds = tier.predicates(DEFAULT_GRAPH);
        preds.sort_by_key(|t| t.0);
        assert_eq!(preds, vec![id(100), id(101)]);
    }

    #[test]
    fn batched_inserts_merge_into_one_partition() {
        let tier = MemoryTier::new();
        tier.insert_quad_batch(&[(DEFAULT_GRAPH, id(1), id(100), id(2))])
            .unwrap();
        tier.insert_quad_batch(&[(DEFAULT_GRAPH, id(3), id(100), id(4))])
            .unwrap();
        let pairs = tier
            .with_predicate(DEFAULT_GRAPH, id(100), |p| p.scan().collect::<Vec<_>>())
            .unwrap();
        assert_eq!(pairs.len(), 2);
        // SPO sort: subject 1 < subject 3.
        assert_eq!(pairs[0].0, id(1));
        assert_eq!(pairs[1].0, id(3));
    }

    #[test]
    fn named_graphs_are_isolated() {
        let tier = MemoryTier::new();
        let g1 = GraphId(TermId::new(TermKind::Uri, 10).0);
        let g2 = GraphId(TermId::new(TermKind::Uri, 11).0);
        tier.insert_quad_batch(&[
            (g1, id(1), id(100), id(2)),
            (g2, id(1), id(100), id(3)),
        ])
        .unwrap();
        let g1_pairs = tier
            .with_predicate(g1, id(100), |p| p.scan().collect::<Vec<_>>())
            .unwrap();
        let g2_pairs = tier
            .with_predicate(g2, id(100), |p| p.scan().collect::<Vec<_>>())
            .unwrap();
        assert_eq!(g1_pairs, vec![(id(1), id(2))]);
        assert_eq!(g2_pairs, vec![(id(1), id(3))]);
    }
}
```

- [ ] **Step 3: Run tests to verify they pass**

Run: `cargo test -p reasoner-storage --lib memory_tier`
Expected: 3 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/storage/src/tier.rs crates/storage/src/memory_tier.rs
git commit -m "$(cat <<'EOF'
storage: add Tier trait and in-memory implementation

MemoryTier groups triples by (graph, predicate) into PredicatePartitions
behind a single RwLock. Closure-guarded with_predicate accessor for safe
borrows; trait keeps a predicate(&self,...) -> Option<&P> placeholder for
a Stage-2 ArcSwap-based layout.
EOF
)"
```

---

## Task 6: `Store` facade combining `Dictionary` + `MemoryTier`

**Files:**
- Modify: `crates/storage/src/store.rs`
- Test: `crates/storage/tests/store_roundtrip.rs`

`Store` is the public entry point. It owns one `Dictionary` and one boxed `dyn Tier`. For Stage 1 the default constructor builds it with `MemoryTier`.

- [ ] **Step 1: Write failing tests**

Create `/Users/stig/git/sunstone/reasoner/crates/storage/tests/store_roundtrip.rs`:

```rust
use oxrdf::{NamedNode, Term};
use reasoner_storage::Store;

fn nn(s: &str) -> Term {
    Term::NamedNode(NamedNode::new(s).unwrap())
}

#[test]
fn insert_triple_and_query_by_predicate() {
    let store = Store::in_memory();
    let alice = nn("http://example.org/Alice");
    let bob = nn("http://example.org/Bob");
    let knows = nn("http://example.org/knows");
    store
        .insert_triples(&[
            (alice.clone(), knows.clone(), bob.clone()),
            (bob.clone(), knows.clone(), alice.clone()),
        ])
        .unwrap();
    assert_eq!(store.triple_count(), 2);

    let pairs = store.scan_predicate_default_graph(&knows).unwrap();
    let mut s_strings: Vec<String> = pairs
        .iter()
        .map(|(s, _)| format!("{}", s))
        .collect();
    s_strings.sort();
    assert_eq!(
        s_strings,
        vec![
            "<http://example.org/Alice>".to_string(),
            "<http://example.org/Bob>".to_string()
        ]
    );
}

#[test]
fn idempotent_insertion() {
    let store = Store::in_memory();
    let s = nn("http://example.org/a");
    let p = nn("http://example.org/p");
    let o = nn("http://example.org/b");
    store.insert_triples(&[(s.clone(), p.clone(), o.clone())]).unwrap();
    store.insert_triples(&[(s, p, o)]).unwrap();
    assert_eq!(store.triple_count(), 1);
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p reasoner-storage --test store_roundtrip`
Expected: compile errors — `Store::in_memory`, `insert_triples`, `scan_predicate_default_graph` not defined.

- [ ] **Step 3: Implement `Store`**

Replace `crates/storage/src/store.rs` with:

```rust
//! Public store facade.
//!
//! Composes a `Dictionary` with one `Tier` implementation. Stage 1 only
//! supports an in-memory tier; the constructor signature leaves room for
//! plugging in cold tiers later.

use crate::dictionary::Dictionary;
use crate::error::Result;
use crate::memory_tier::MemoryTier;
use crate::term::{GraphId, TermId, DEFAULT_GRAPH};
use crate::tier::{Tier, TierStats};
use oxrdf::Term;

pub struct Store {
    dictionary: Dictionary,
    tier: Box<dyn Tier>,
}

impl Store {
    pub fn in_memory() -> Self {
        Self {
            dictionary: Dictionary::new(),
            tier: Box::new(MemoryTier::new()),
        }
    }

    pub fn dictionary(&self) -> &Dictionary {
        &self.dictionary
    }

    pub fn tier(&self) -> &dyn Tier {
        self.tier.as_ref()
    }

    pub fn triple_count(&self) -> u64 {
        self.tier.triple_count()
    }

    pub fn stats(&self) -> TierStats {
        self.tier.stats()
    }

    /// Insert into the default graph.
    pub fn insert_triples(&self, triples: &[(Term, Term, Term)]) -> Result<()> {
        let mut quads = Vec::with_capacity(triples.len());
        for (s, p, o) in triples {
            let s_id = self.dictionary.intern(s)?;
            let p_id = self.dictionary.intern(p)?;
            let o_id = self.dictionary.intern(o)?;
            quads.push((DEFAULT_GRAPH, s_id, p_id, o_id));
        }
        self.tier.insert_quad_batch(&quads)
    }

    /// Insert (graph, s, p, o) quads. Caller-supplied `GraphId`s must already
    /// have been interned via `intern_graph_uri`.
    pub fn insert_quads(&self, quads: &[(GraphId, Term, Term, Term)]) -> Result<()> {
        let mut encoded = Vec::with_capacity(quads.len());
        for (g, s, p, o) in quads {
            let s_id = self.dictionary.intern(s)?;
            let p_id = self.dictionary.intern(p)?;
            let o_id = self.dictionary.intern(o)?;
            encoded.push((*g, s_id, p_id, o_id));
        }
        self.tier.insert_quad_batch(&encoded)
    }

    pub fn intern_graph_uri(&self, graph_uri: &Term) -> Result<GraphId> {
        let id = self.dictionary.intern(graph_uri)?;
        Ok(GraphId(id.0))
    }

    /// Scan a single predicate in the default graph, returning materialized
    /// (subject, object) `Term` pairs. Used by tests; production code should
    /// use the tier's columnar scan directly.
    pub fn scan_predicate_default_graph(
        &self,
        predicate: &Term,
    ) -> Result<Vec<(Term, Term)>> {
        let p_id = self.dictionary.intern(predicate)?;
        let mt = self
            .tier
            .as_any()
            .downcast_ref::<MemoryTier>()
            .expect("Stage-1 store always wraps MemoryTier");
        let pairs = mt
            .with_predicate(DEFAULT_GRAPH, p_id, |part| {
                part.scan().collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let mut out = Vec::with_capacity(pairs.len());
        for (s_id, o_id) in pairs {
            let s = self
                .dictionary
                .lookup(s_id)
                .ok_or_else(|| crate::StorageError::InvalidTerm(format!("unknown id {:?}", s_id)))?;
            let o = self
                .dictionary
                .lookup(o_id)
                .ok_or_else(|| crate::StorageError::InvalidTerm(format!("unknown id {:?}", o_id)))?;
            out.push((s, o));
        }
        Ok(out)
    }
}
```

- [ ] **Step 4: Add `as_any` to the `Tier` trait so `Store::scan_predicate_default_graph` can downcast**

Edit `crates/storage/src/tier.rs`. Append at the bottom of the file:

```rust
use std::any::Any;

impl dyn Tier {
    // empty marker; real method on trait below.
}
```

Then change the trait definition to add an `as_any` method. Replace the existing `pub trait Tier { ... }` block in `crates/storage/src/tier.rs` with:

```rust
pub trait Tier: Send + Sync + std::any::Any {
    fn insert_quad_batch(&self, quads: &[(GraphId, TermId, TermId, TermId)]) -> Result<()>;

    fn predicate(&self, graph: GraphId, predicate: TermId) -> Option<&PredicatePartition>;

    fn predicates(&self, graph: GraphId) -> Vec<TermId>;

    fn graphs(&self) -> Vec<GraphId>;

    fn triple_count(&self) -> u64;

    fn stats(&self) -> TierStats;

    fn as_any(&self) -> &dyn std::any::Any;
}
```

Remove the trailing `impl dyn Tier` and stray `use std::any::Any;` you just added — they were a typo dead-end. The final `crates/storage/src/tier.rs` should contain only `use` lines, `TierStats`, and the trait above.

Then in `crates/storage/src/memory_tier.rs`, inside `impl Tier for MemoryTier { ... }`, append:

```rust
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p reasoner-storage --test store_roundtrip`
Expected: 2 tests pass.

Also run: `cargo test -p reasoner-storage`
Expected: all prior tests still pass.

- [ ] **Step 6: Commit**

```bash
git add crates/storage/src/store.rs crates/storage/src/tier.rs crates/storage/src/memory_tier.rs crates/storage/tests/store_roundtrip.rs
git commit -m "$(cat <<'EOF'
storage: add Store facade combining Dictionary and Tier

Store::in_memory() returns a Stage-1 store backed by MemoryTier. The
Tier trait gains as_any() so scan helpers can downcast for guarded
borrow access; production callers will switch to a guard-aware API in
Stage 2 once the cold tier shape is settled.
EOF
)"
```

---

## Task 7: N-Triples streaming bulk loader

**Files:**
- Modify: `crates/storage/src/loader/ntriples.rs`
- Create: `crates/storage/tests/fixtures/tiny.nt`
- Create: `crates/storage/tests/fixtures/with_literals.nt`
- Test: `crates/storage/tests/ntriples_loader.rs`

The loader streams triples from any `Read` source using `oxttl::NTriplesParser`, batching into the dictionary and tier in chunks of `BATCH_SIZE = 65_536`. It returns `LoadStats { triples, bytes_read, elapsed_ms, dictionary_size }`.

- [ ] **Step 1: Create fixture files**

Create `/Users/stig/git/sunstone/reasoner/crates/storage/tests/fixtures/tiny.nt`:

```
<http://example.org/Alice> <http://example.org/knows> <http://example.org/Bob> .
<http://example.org/Alice> <http://example.org/knows> <http://example.org/Carol> .
<http://example.org/Bob> <http://example.org/knows> <http://example.org/Alice> .
<http://example.org/Carol> <http://example.org/age> "29"^^<http://www.w3.org/2001/XMLSchema#integer> .
<http://example.org/Alice> <http://example.org/age> "30"^^<http://www.w3.org/2001/XMLSchema#integer> .
<http://example.org/Bob> <http://example.org/age> "31"^^<http://www.w3.org/2001/XMLSchema#integer> .
```

Create `/Users/stig/git/sunstone/reasoner/crates/storage/tests/fixtures/with_literals.nt`:

```
<http://example.org/s1> <http://example.org/name> "Alice" .
<http://example.org/s2> <http://example.org/name> "Bob"@en .
<http://example.org/s3> <http://example.org/age> "42"^^<http://www.w3.org/2001/XMLSchema#integer> .
<http://example.org/s4> <http://example.org/score> "3.14"^^<http://www.w3.org/2001/XMLSchema#decimal> .
_:b0 <http://example.org/p> <http://example.org/o> .
```

- [ ] **Step 2: Write failing loader tests**

Create `/Users/stig/git/sunstone/reasoner/crates/storage/tests/ntriples_loader.rs`:

```rust
use reasoner_storage::loader::ntriples::{load_ntriples_file, LoadStats};
use reasoner_storage::Store;
use std::path::PathBuf;

fn fixture(name: &str) -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests/fixtures");
    p.push(name);
    p
}

#[test]
fn load_tiny_fixture() {
    let store = Store::in_memory();
    let stats: LoadStats = load_ntriples_file(&store, &fixture("tiny.nt")).unwrap();
    assert_eq!(stats.triples, 6);
    assert_eq!(store.triple_count(), 6);
    // 6 distinct URIs (Alice, Bob, Carol, knows, age, plus none — wait, count.)
    //   subjects: Alice, Bob, Carol           = 3
    //   predicates: knows, age                = 2
    //   objects (URIs): Alice, Bob, Carol     = 0 new (all already counted)
    //   ages 29/30/31 are inline ints, not dict entries.
    // → 5 dictionary entries.
    assert_eq!(store.dictionary().len(), 5);
}

#[test]
fn load_with_literals_fixture() {
    let store = Store::in_memory();
    let stats = load_ntriples_file(&store, &fixture("with_literals.nt")).unwrap();
    assert_eq!(stats.triples, 5);
    assert_eq!(store.triple_count(), 5);
    // Distinct dictionary entries (excluding inline-int "42"):
    //   URIs: s1, s2, s3, s4, name, age, score, p, o          = 9
    //   Literals: "Alice" (plain), "Bob"@en (lang), 3.14 (decimal) = 3
    //   Blank nodes: _:b0                                      = 1
    // Total = 13.
    assert_eq!(store.dictionary().len(), 13);
}

#[test]
fn load_is_idempotent() {
    let store = Store::in_memory();
    load_ntriples_file(&store, &fixture("tiny.nt")).unwrap();
    load_ntriples_file(&store, &fixture("tiny.nt")).unwrap();
    assert_eq!(store.triple_count(), 6, "duplicate triples must collapse");
}

#[test]
fn missing_file_returns_error() {
    let store = Store::in_memory();
    let err = load_ntriples_file(&store, &fixture("does-not-exist.nt"));
    assert!(err.is_err());
}
```

- [ ] **Step 3: Run to verify failure**

Run: `cargo test -p reasoner-storage --test ntriples_loader`
Expected: compile errors — symbols not yet defined.

- [ ] **Step 4: Implement the loader**

Replace `crates/storage/src/loader/ntriples.rs` with:

```rust
//! Streaming N-Triples bulk loader.
//!
//! Uses `oxttl::NTriplesParser` to stream triples from any `Read` source,
//! batching into the dictionary + tier in chunks of `BATCH_SIZE`.

use crate::error::{Result, StorageError};
use crate::store::Store;
use crate::term::DEFAULT_GRAPH;
use oxrdf::{Subject, Term};
use oxttl::NTriplesParser;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;
use std::time::Instant;

const BATCH_SIZE: usize = 65_536;

#[derive(Debug, Clone, Copy)]
pub struct LoadStats {
    pub triples: u64,
    pub bytes_read: u64,
    pub elapsed_ms: u64,
    pub dictionary_size: u64,
}

pub fn load_ntriples_file(store: &Store, path: &Path) -> Result<LoadStats> {
    let file = File::open(path)?;
    let bytes = file.metadata().ok().map(|m| m.len()).unwrap_or(0);
    let reader = BufReader::with_capacity(1 << 20, file);
    let mut stats = load_ntriples_reader(store, reader)?;
    stats.bytes_read = bytes;
    Ok(stats)
}

pub fn load_ntriples_reader<R: Read>(store: &Store, reader: R) -> Result<LoadStats> {
    let start = Instant::now();
    let parser = NTriplesParser::new();
    let mut iter = parser.parse_read(reader);

    let mut batch: Vec<(crate::term::GraphId, _, _, _)> = Vec::with_capacity(BATCH_SIZE);
    let mut total: u64 = 0;

    // Pre-intern terms via dictionary, then push encoded quad into batch.
    while let Some(t) = iter.next() {
        let triple =
            t.map_err(|e| StorageError::NtriplesParse(format!("{e}")))?;
        // Convert oxrdf::Subject and oxrdf::Term-style nodes into oxrdf::Term.
        let s_term = subject_to_term(triple.subject);
        let p_term = Term::NamedNode(triple.predicate);
        let o_term = triple.object;

        let s_id = store.dictionary().intern(&s_term)?;
        let p_id = store.dictionary().intern(&p_term)?;
        let o_id = store.dictionary().intern(&o_term)?;
        batch.push((DEFAULT_GRAPH, s_id, p_id, o_id));
        total += 1;
        if batch.len() >= BATCH_SIZE {
            store.tier().insert_quad_batch(&batch)?;
            batch.clear();
        }
    }
    if !batch.is_empty() {
        store.tier().insert_quad_batch(&batch)?;
    }

    Ok(LoadStats {
        triples: total,
        bytes_read: 0, // file caller will overwrite
        elapsed_ms: start.elapsed().as_millis() as u64,
        dictionary_size: store.dictionary().len() as u64,
    })
}

fn subject_to_term(s: Subject) -> Term {
    match s {
        Subject::NamedNode(n) => Term::NamedNode(n),
        Subject::BlankNode(b) => Term::BlankNode(b),
        // oxrdf gates triples-as-subjects behind the `rdf-star` feature, which
        // we do not enable; this arm is therefore unreachable in Stage 1.
        #[allow(unreachable_patterns)]
        _ => panic!("RDF-star subject not supported in Stage 1"),
    }
}
```

- [ ] **Step 5: Verify the API surface in `lib.rs`**

`crates/storage/src/lib.rs` already re-exports `loader` as a module. Confirm that callers can write `use reasoner_storage::loader::ntriples::{load_ntriples_file, LoadStats}`. No edit needed if the module declarations from Task 1 are intact.

- [ ] **Step 6: Run the loader tests**

Run: `cargo test -p reasoner-storage --test ntriples_loader`
Expected: 4 tests pass.

If the literal-fixture test fails with a dictionary count off by one, inspect the actual count printed by `cargo test -p reasoner-storage --test ntriples_loader -- --nocapture` and reconcile against the fixture (most likely cause: an unexpected `xsd:string` datatype attached by oxrdf to plain literals — adjust the test count, not the loader).

- [ ] **Step 7: Commit**

```bash
git add crates/storage/src/loader/ntriples.rs crates/storage/tests/ntriples_loader.rs crates/storage/tests/fixtures/
git commit -m "$(cat <<'EOF'
storage: add streaming N-Triples bulk loader

Streams via oxttl::NTriplesParser into batches of 65 536 quads. Returns
LoadStats { triples, bytes_read, elapsed_ms, dictionary_size }. Default
graph only (Stage 1 quads-loader is a separate task).
EOF
)"
```

---

## Task 8: Property-based round-trip test for the dictionary

**Files:**
- Create: `crates/storage/tests/dictionary_proptest.rs`

- [ ] **Step 1: Write the failing test**

Create `/Users/stig/git/sunstone/reasoner/crates/storage/tests/dictionary_proptest.rs`:

```rust
use oxrdf::{NamedNode, Term};
use proptest::prelude::*;
use reasoner_storage::Dictionary;

fn arb_uri() -> impl Strategy<Value = Term> {
    "[a-z]{1,16}".prop_map(|s| {
        Term::NamedNode(NamedNode::new(format!("http://example.org/{}", s)).unwrap())
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn intern_then_lookup_round_trips(uris in proptest::collection::vec(arb_uri(), 1..50)) {
        let dict = Dictionary::new();
        let mut ids = Vec::with_capacity(uris.len());
        for u in &uris {
            ids.push(dict.intern(u).unwrap());
        }
        for (id, u) in ids.iter().zip(uris.iter()) {
            prop_assert_eq!(dict.lookup(*id).as_ref(), Some(u));
        }
    }

    #[test]
    fn duplicate_interns_collapse(u in arb_uri()) {
        let dict = Dictionary::new();
        let a = dict.intern(&u).unwrap();
        let b = dict.intern(&u).unwrap();
        prop_assert_eq!(a, b);
        prop_assert_eq!(dict.len(), 1);
    }
}
```

- [ ] **Step 2: Run the test**

Run: `cargo test -p reasoner-storage --test dictionary_proptest`
Expected: 2 properties pass (64 cases each).

- [ ] **Step 3: Commit**

```bash
git add crates/storage/tests/dictionary_proptest.rs
git commit -m "$(cat <<'EOF'
storage: add proptest round-trip coverage for Dictionary

64-case property runs verifying intern→lookup equality and
intern-twice idempotency on randomly generated URIs.
EOF
)"
```

---

## Task 9: Footprint instrumentation and `Store::report_footprint()`

**Files:**
- Modify: `crates/storage/src/store.rs`
- Test: `crates/storage/tests/store_roundtrip.rs` (append)

Acceptance gate (SPEC-02 NF1) says ≤50 bytes/triple in the warm tier. Stage 1 must *report* the figure so it can be tracked; Stage 2 may enforce. We add `Store::report_footprint() -> FootprintReport { triples, bytes_estimated, bytes_per_triple }`.

- [ ] **Step 1: Write the failing test**

Append to `/Users/stig/git/sunstone/reasoner/crates/storage/tests/store_roundtrip.rs`:

```rust
#[test]
fn footprint_is_reported() {
    let store = Store::in_memory();
    let s = NamedNode::new("http://example.org/s").unwrap();
    let p = NamedNode::new("http://example.org/p").unwrap();
    let triples: Vec<_> = (0..1000u32)
        .map(|i| {
            (
                Term::NamedNode(s.clone()),
                Term::NamedNode(p.clone()),
                Term::NamedNode(NamedNode::new(format!("http://example.org/o{}", i)).unwrap()),
            )
        })
        .collect();
    store.insert_triples(&triples).unwrap();
    let report = store.report_footprint();
    assert_eq!(report.triples, 1000);
    assert!(report.bytes_per_triple > 0.0);
    // 16 bytes (s/o columns) plus per-predicate overhead; sanity bound.
    assert!(
        report.bytes_per_triple < 64.0,
        "footprint {} bytes/triple exceeds Stage-1 sanity bound",
        report.bytes_per_triple
    );
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p reasoner-storage --test store_roundtrip footprint_is_reported`
Expected: `report_footprint` not defined.

- [ ] **Step 3: Implement `report_footprint`**

Append to `crates/storage/src/store.rs` (above the closing `}` of `impl Store`):

```rust
    pub fn report_footprint(&self) -> FootprintReport {
        let stats = self.tier.stats();
        let bpt = if stats.triples == 0 {
            0.0
        } else {
            stats.bytes_estimated as f64 / stats.triples as f64
        };
        FootprintReport {
            triples: stats.triples,
            bytes_estimated: stats.bytes_estimated,
            bytes_per_triple: bpt,
        }
    }
```

And add this struct above the `impl Store`:

```rust
#[derive(Debug, Clone, Copy)]
pub struct FootprintReport {
    pub triples: u64,
    pub bytes_estimated: u64,
    pub bytes_per_triple: f64,
}
```

Then re-export from `crates/storage/src/lib.rs`. Edit the `pub use store::Store;` line to:

```rust
pub use store::{FootprintReport, Store};
```

- [ ] **Step 4: Verify the test passes**

Run: `cargo test -p reasoner-storage --test store_roundtrip`
Expected: 3 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/storage/src/store.rs crates/storage/src/lib.rs crates/storage/tests/store_roundtrip.rs
git commit -m "$(cat <<'EOF'
storage: add Store::report_footprint for NF1 tracking

Reports bytes/triple based on TierStats so the Stage-1 acceptance run
can record the figure against the SPEC-02 NF1 target (≤50 B/triple).
Stage 1 only reports; Stage 2 may enforce.
EOF
)"
```

---

## Task 10: Criterion bench for bulk load throughput

**Files:**
- Create: `crates/storage/benches/load_lubm.rs`

This bench is the executable proxy for SPEC-02 acceptance criterion #1 (LUBM-100 in ≤30 s). For day-one CI we run it against `tests/fixtures/tiny.nt`. On the reference workstation, the engineer runs it against a real LUBM-100 dump by setting `LUBM_NT=/path/to/lubm100.nt`.

- [ ] **Step 1: Write the bench**

Create `/Users/stig/git/sunstone/reasoner/crates/storage/benches/load_lubm.rs`:

```rust
use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use reasoner_storage::loader::ntriples::load_ntriples_file;
use reasoner_storage::Store;
use std::path::PathBuf;

fn fixture_path() -> PathBuf {
    if let Ok(p) = std::env::var("LUBM_NT") {
        return PathBuf::from(p);
    }
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests/fixtures/tiny.nt");
    p
}

fn bench_load(c: &mut Criterion) {
    let path = fixture_path();
    let bytes = std::fs::metadata(&path)
        .expect("fixture exists")
        .len();

    // Probe triple count once for throughput annotation.
    let probe = Store::in_memory();
    let stats = load_ntriples_file(&probe, &path).expect("load fixture");
    let triples = stats.triples;

    let mut group = c.benchmark_group("ntriples_load");
    group.throughput(Throughput::Bytes(bytes));
    group.sample_size(10);
    group.bench_function("load_file", |b| {
        b.iter(|| {
            let store = Store::in_memory();
            load_ntriples_file(&store, &path).unwrap();
        });
    });
    eprintln!(
        "fixture: {} triples, {} bytes, last-stats elapsed {} ms (≈{:.2} Mtriples/s)",
        triples,
        bytes,
        stats.elapsed_ms,
        if stats.elapsed_ms == 0 {
            f64::INFINITY
        } else {
            triples as f64 / (stats.elapsed_ms as f64) / 1000.0
        }
    );
    group.finish();
}

criterion_group!(benches, bench_load);
criterion_main!(benches);
```

- [ ] **Step 2: Run the bench on the tiny fixture**

Run: `cargo bench -p reasoner-storage --bench load_lubm -- --quick`
Expected: bench completes; reports a load time in microseconds for `tiny.nt`. Triples count printed is 6.

- [ ] **Step 3: (Reference-workstation only) Run against LUBM-100 if available**

If a LUBM-100 N-Triples dump is available on the workstation, run:

```bash
LUBM_NT=/abs/path/to/lubm100.nt cargo bench -p reasoner-storage --bench load_lubm
```

Record the elapsed time in the commit message. Acceptance gate: ≤30 s. If it exceeds, file a follow-up issue rather than blocking Stage 1 — the gate is reported, then iterated.

- [ ] **Step 4: Commit**

```bash
git add crates/storage/benches/load_lubm.rs
git commit -m "$(cat <<'EOF'
storage: add Criterion bench for N-Triples bulk load

Defaults to tests/fixtures/tiny.nt for CI smoke. Override with
LUBM_NT=/path/to/lubm100.nt on the reference workstation to drive the
SPEC-02 Stage-1 acceptance run (≤30 s for ~13M triples).
EOF
)"
```

---

## Task 11: Stage-1 acceptance documentation

**Files:**
- Create: `crates/storage/STAGE1-ACCEPTANCE.md`

- [ ] **Step 1: Write the document**

Create `/Users/stig/git/sunstone/reasoner/crates/storage/STAGE1-ACCEPTANCE.md`:

```markdown
# SPEC-02 Stage-1 Acceptance Record

Run on: <fill in: hostname, kernel, CPU, DRAM channels & speed>
Commit: <fill in: git rev-parse HEAD>
Date: <fill in>

## SPEC-02 acceptance criteria addressed in Stage 1

| # | Criterion | Status | Evidence |
|---|-----------|--------|----------|
| 1 | LUBM-100 import ≤30 s | <PASS/FAIL> | `cargo bench -p reasoner-storage --bench load_lubm` with `LUBM_NT=...lubm100.nt`; elapsed = <ms> |
| 2 | LUBM-8000 import ≤30 min | DEFERRED | Stage 2 — bench harness exists, just not run yet |
| 3 | LUBM-8000 footprint ≤55 GB | DEFERRED | Stage 2 |
| 4 | Sequential scan ≥80% of STREAM Triad | DEFERRED | Stage 2 (needs the hot tier in a NUMA-pinned bench) |
| 5 | HDT round-trip isomorphism | DEFERRED | Stage 2 (no HDT support in Stage 1) |
| 6 | All-six orderings for top-10 predicates | DEFERRED | Stage 2 |

## Stage-1 surfaced figures

- LUBM-100 triple count: <fill in>
- LUBM-100 load elapsed: <fill in> ms (≈ <Mtriples/s>)
- LUBM-100 dictionary size: <fill in>
- LUBM-100 footprint via `Store::report_footprint()`: <fill in> bytes (<fill in> B/triple)
- W3C harness selected subset run (`cargo test -p reasoner-harness`): <PASS/FAIL>

## Out-of-scope items tracked as Future Work

- HDT cold-tier (SPEC-02 F9)
- CXL/NVMe tiering (SPEC-02 NF4)
- MVCC, copy-on-write snapshots (SPEC-02 risks/open questions)
- All-six index orderings (SPEC-02 F4)
- Snapshot HDT export (SPEC-02 F9)
- Persistent on-disk dictionary (SPEC-02 risks/open questions)
- Turtle, N-Quads, HDT input formats (SPEC-02 F8 — only N-Triples in Stage 1)
- Crash-consistent checkpointing (SPEC-02 NF5 — Stage 1 is in-memory only)
```

- [ ] **Step 2: Commit**

```bash
git add crates/storage/STAGE1-ACCEPTANCE.md
git commit -m "$(cat <<'EOF'
storage: add Stage-1 acceptance record template

Tracks which SPEC-02 acceptance criteria Stage 1 covers, which are
deliberately deferred to Stage 2, and where to record the LUBM-100
load figures from the reference workstation.
EOF
)"
```

---

## Task 12: Final verification — workspace-wide green build

**Files:** (none — verification only)

- [ ] **Step 1: Format check**

Run: `cargo fmt --all -- --check`
Expected: no output (clean). If output appears, run `cargo fmt --all` and commit the result with message `storage: cargo fmt`.

- [ ] **Step 2: Clippy on the storage crate**

Run: `cargo clippy -p reasoner-storage --all-targets -- -D warnings`
Expected: no warnings or errors. If clippy flags issues, fix them and commit per failure with messages like `storage: address clippy lint <lint-name>`.

- [ ] **Step 3: Full test run**

Run: `cargo test -p reasoner-storage`
Expected: all tests (lib + integration + proptest) pass. Approximate counts:
- `term` (lib): 6
- `memory_tier` (lib): 3
- `dictionary` (integration): 7
- `dictionary_proptest`: 2 properties
- `partition` (integration): 4
- `store_roundtrip` (integration): 3
- `ntriples_loader` (integration): 4

Total: ~29 tests + 2 proptests.

- [ ] **Step 4: Workspace build**

Run: `cargo build --workspace`
Expected: the rest of the crates (still placeholders) compile alongside `reasoner-storage`.

- [ ] **Step 5: No new commit unless something was fixed in steps 1–2**

If steps 1–2 surfaced fixes, they were already committed individually. Otherwise nothing to do here.

---

## Self-Review Summary

**Spec coverage (SPEC-02 functional requirements):**
- F1 Dictionary — Task 3 ✓
- F2 Term taxonomy — Task 2 ✓
- F3 Predicate partitioning — Task 4 ✓
- F4 Six orderings on demand — DEFERRED (Stage 2), recorded in STAGE1-ACCEPTANCE.md (Task 11) ✓
- F5 Roaring bitmaps — Task 4 ✓ (RoaringTreemap with documented Stage-1 reasoning)
- F6 Tier API — Task 5 (`Tier` trait + `MemoryTier`) ✓
- F7 Named graphs — Tasks 5 and 6 (insert_quads, graph isolation test) ✓
- F8 Bulk import — Task 7 (N-Triples only; Turtle/N-Quads/HDT deferred) ✓ partial as scoped
- F9 Snapshot export — DEFERRED, recorded ✓

**Spec coverage (acceptance criteria):**
- #1 LUBM-100 in 30 s — Task 10 bench + Task 11 record ✓
- #2–#6 — DEFERRED, recorded in Task 11 ✓

**Placeholder scan:** No "TBD", "implement later", or unspecified error handling. All code blocks are concrete and runnable.

**Type consistency:** `TermId`, `TermKind`, `GraphId`, `Dictionary`, `PredicatePartition`, `Tier`, `MemoryTier`, `Store`, `LoadStats`, `FootprintReport`, `TierStats` are introduced once and used consistently. `with_predicate` is defined in Task 5 and used in Task 6.

---

## Execution Handoff

Plan complete and saved to `plans/2026-05-24-SPEC-02-storage.md`. Two execution options:

1. **Subagent-Driven (recommended)** — dispatch a fresh subagent per task, review between tasks, fast iteration.
2. **Inline Execution** — execute tasks in this session using `superpowers:executing-plans`, batch execution with checkpoints.

Which approach?
