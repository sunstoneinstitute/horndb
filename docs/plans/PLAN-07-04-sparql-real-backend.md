---
status: executed
date: 2026-06-11
scope: "SPARQL Real-Backend Wiring (storage + WCOJ + closure)"
---

# SPARQL Real-Backend Wiring (storage + WCOJ + closure) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement `HornBackend` — a `horndb-storage`-backed, `horndb-wcoj`-executed implementation of the SPARQL crate's `Executor`/`Store` seam — wire the HTTP server and `serve` binary onto it, and let the `horndb-owlrl` materialized closure load into the same store (issue #67).

**Architecture:** The SPARQL crate already has the right seam: `exec::Executor::scan_bgp` + `exec::Store` (`crates/sparql/src/exec/mod.rs:65-85`). We add a new implementation `exec::horn::HornBackend` that owns a `horndb_storage::Store` (kind-tagged dictionary + columnar tier — fixes the term-typing erasure), a tombstone set (storage is insertion-only; `DELETE DATA` is an overlay), and a lazily-rebuilt `horndb_wcoj::source::vec_source::VecTripleSource` snapshot that the Leapfrog Triejoin executor runs over. The owlrl `Engine`'s `materialized_triples()` loads straight into the same backend (no flat-file round trip). The axum `AppState` becomes generic over the backend so both `MemStore` (existing tests) and `HornBackend` (the `serve` binary) work.

**Tech Stack:** Rust 1.90, oxrdf 0.3, arrow (via horndb-wcoj), horndb-storage / horndb-wcoj / horndb-owlrl (RuleFiring backend — no GraphBLAS needed).

**Scope guard (YAGNI):** No streaming, no MVCC, no named-graph scoping, no property paths, no `INSERT … WHERE` (that is #51), no changes to wcoj's planner. The snapshot rebuild on mutation is O(n log n) and documented as a Stage-1 cost.

**Conventions you must preserve** (established by `crates/sparql/src/algebra/translate.rs:238-240` and `exec/mem.rs`):
- `algebra::Term::Iri` carries the bare IRI string (no `<>`).
- `algebra::Term::BlankNode` carries the **bare** label (no `_:` prefix).
- `algebra::Term::Literal` carries the full N-Triples form (`oxrdf::Literal::to_string()`), e.g. `"hi"@en`, `"42"^^<http://www.w3.org/2001/XMLSchema#integer>`.
- `Engine::materialized_triples()` (owlrl) returns IRIs bare, blank nodes **with** `_:` prefix, literals N-Triples form (`crates/owlrl/src/integration.rs:256-291`).

---

### Task 1: `Dictionary::get` — non-interning lookup (horndb-storage)

Query constants must resolve to `TermId`s without polluting the dictionary. A constant absent from the dictionary means "no triple can match".

**Files:**
- Modify: `crates/storage/src/dictionary.rs`
- Test: same file, `#[cfg(test)]` module (follow the existing test style in `crates/storage/src/term.rs`)

- [ ] **Step 1: Write the failing test**

Append to `crates/storage/src/dictionary.rs` (create the test module if absent; check the file end first):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use oxrdf::NamedNode;

    #[test]
    fn get_returns_id_without_interning() {
        let d = Dictionary::new();
        let t = Term::NamedNode(NamedNode::new("http://ex/a").unwrap());
        assert_eq!(d.get(&t), None);
        assert_eq!(d.len(), 0, "get must not intern");
        let id = d.intern(&t).unwrap();
        assert_eq!(d.get(&t), Some(id));
    }

    #[test]
    fn get_resolves_inline_int_without_interning() {
        let d = Dictionary::new();
        let t = Term::Literal(Literal::new_typed_literal(
            "42",
            NamedNodeRef::new(XSD_INTEGER).unwrap(),
        ));
        let id = d.get(&t).expect("inline ints always resolve");
        assert_eq!(id, TermId::inline_int(42));
        assert_eq!(d.len(), 0);
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p horndb-storage get_returns_id_without_interning`
Expected: compile FAIL — `no method named `get` found`.

- [ ] **Step 3: Implement `get`**

Add to `impl Dictionary` in `crates/storage/src/dictionary.rs` (after `intern`):

```rust
    /// Resolve a term to its `TermId` **without** interning it. Returns
    /// `None` if the term has never been interned (inline-int literals
    /// always resolve — they are value-encoded, not dictionary-allocated).
    /// Used by query frontends to look up constants: an absent constant
    /// means no stored triple can match it.
    pub fn get(&self, term: &Term) -> Option<TermId> {
        if let Some(id) = try_inline_int(term) {
            return Some(id);
        }
        self.forward.get(term).map(|e| *e)
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p horndb-storage dictionary`
Expected: PASS (both new tests).

- [ ] **Step 5: Commit**

```bash
git add crates/storage/src/dictionary.rs
git commit -m "feat(storage): non-interning Dictionary::get for query-constant lookup (#67)"
```

---

### Task 2: `Store::scan_all_term_ids` — full default-graph dump (horndb-storage)

The wcoj snapshot needs every `(s, p, o)` as raw `TermId`s.

**Files:**
- Modify: `crates/storage/src/store.rs`

- [ ] **Step 1: Write the failing test**

Append to `crates/storage/src/store.rs` (create a `#[cfg(test)] mod tests` at the end if the file has none — check first):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use oxrdf::NamedNode;

    fn iri(s: &str) -> Term {
        Term::NamedNode(NamedNode::new(s).unwrap())
    }

    #[test]
    fn scan_all_term_ids_returns_every_default_graph_triple() {
        let store = Store::in_memory();
        store
            .insert_triples(&[
                (iri("http://ex/a"), iri("http://ex/p"), iri("http://ex/b")),
                (iri("http://ex/a"), iri("http://ex/q"), iri("http://ex/c")),
            ])
            .unwrap();
        let all = store.scan_all_term_ids();
        assert_eq!(all.len(), 2);
        let p = store.dictionary().get(&iri("http://ex/p")).unwrap();
        let q = store.dictionary().get(&iri("http://ex/q")).unwrap();
        let preds: Vec<TermId> = all.iter().map(|t| t.1).collect();
        assert!(preds.contains(&p) && preds.contains(&q));
    }
}
```

Also add `use crate::term::TermId;` to the imports at the top of `store.rs` if not already present (it is not — the file currently imports `GraphId, DEFAULT_GRAPH` from `crate::term`; extend that import).

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p horndb-storage scan_all_term_ids`
Expected: compile FAIL — no method `scan_all_term_ids`.

- [ ] **Step 3: Implement**

Add to `impl Store` (after `scan_predicate_ordered`):

```rust
    /// Dump every default-graph triple as raw `TermId`s, in arbitrary
    /// order. O(triples) and materialized — intended for snapshot
    /// builders (e.g. the SPARQL frontend's WCOJ source), not hot paths.
    pub fn scan_all_term_ids(&self) -> Vec<(TermId, TermId, TermId)> {
        let mt = self
            .tier
            .as_any()
            .downcast_ref::<MemoryTier>()
            .expect("Stage-1 store always wraps MemoryTier");
        let mut out = Vec::with_capacity(self.tier.triple_count() as usize);
        for p_id in self.tier.predicates(DEFAULT_GRAPH) {
            mt.with_predicate(DEFAULT_GRAPH, p_id, |part| {
                out.extend(part.scan().map(|(s, o)| (s, p_id, o)));
            });
        }
        out
    }
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p horndb-storage`
Expected: PASS, no regressions.

- [ ] **Step 5: Commit**

```bash
git add crates/storage/src/store.rs
git commit -m "feat(storage): Store::scan_all_term_ids default-graph dump (#67)"
```

---

### Task 3: `VecTripleSource::contains` (horndb-wcoj)

Fully-ground BGP patterns (e.g. `ASK { <s> <p> <o> }`) are answered by membership, not by the join executor (a zero-variable BGP would produce a zero-column Arrow batch, which `RecordBatch` cannot represent).

**Files:**
- Modify: `crates/wcoj/src/source/vec_source.rs`

- [ ] **Step 1: Write the failing test**

Append to `crates/wcoj/src/source/vec_source.rs` (it has no test module today — add one):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contains_finds_present_and_rejects_absent() {
        let src = VecTripleSource::from_triples(vec![
            Triple::new(1, 2, 3),
            Triple::new(1, 2, 4),
            Triple::new(5, 6, 7),
        ]);
        assert!(src.contains(&Triple::new(1, 2, 4)));
        assert!(!src.contains(&Triple::new(1, 2, 5)));
        assert!(!src.contains(&Triple::new(9, 9, 9)));
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p horndb-wcoj contains_finds_present`
Expected: compile FAIL — no method `contains`.

- [ ] **Step 3: Implement**

Add to `impl VecTripleSource` (after `from_triples`):

```rust
    /// O(log n) membership test against the SPO-sorted ordering.
    pub fn contains(&self, t: &Triple) -> bool {
        let spo = &self.sorted[&Ordering::Spo];
        spo.binary_search(&(t.s, t.p, t.o)).is_ok()
    }
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p horndb-wcoj contains_finds_present`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/wcoj/src/source/vec_source.rs
git commit -m "feat(wcoj): VecTripleSource::contains membership probe (#67)"
```

---

### Task 4: Term conversion helpers + crate plumbing (horndb-sparql)

Create `crates/sparql/src/exec/horn.rs` with the conversion layer between `algebra::Term` (lexical strings), `oxrdf::Term` (what the storage dictionary keys on), and the owlrl lexical dump format. The `HornBackend` itself comes in Task 5.

**Files:**
- Modify: `crates/sparql/Cargo.toml`, root `Cargo.toml` (workspace deps), `crates/sparql/src/exec/mod.rs`, `crates/sparql/src/exec/runtime.rs`
- Create: `crates/sparql/src/exec/horn.rs`

- [ ] **Step 1: Wire dependencies**

In the **root** `Cargo.toml` `[workspace.dependencies]`, ensure entries exist for the internal crates (check first — they may not be listed since intra-crate deps use `path`). If absent, skip the workspace table (internal path deps are referenced directly per existing convention — confirm by looking at `crates/owlrl/Cargo.toml`'s `horndb-closure = { path = "../closure", optional = true }`). In `crates/sparql/Cargo.toml` `[dependencies]` add:

```toml
horndb-storage = { path = "../storage" }
horndb-wcoj = { path = "../wcoj" }
arrow = { workspace = true }
```

(`arrow` must already be a workspace dep because horndb-wcoj uses it — check the root `Cargo.toml`; if it is declared per-crate in wcoj, mirror that exact version requirement instead of `workspace = true`.)

- [ ] **Step 2: Expose the runtime's literal parsing helpers**

In `crates/sparql/src/exec/runtime.rs`, change the visibility of two existing helpers (no behavior change):

```rust
pub(crate) fn unescape_ntriples(s: &str) -> String {
pub(crate) fn literal_parts(raw: &str) -> (String, Option<String>, Option<String>) {
```

(They are currently private `fn`; `literal_parts` returns the **escaped** value — `unescape_ntriples` must be applied by the consumer.)

- [ ] **Step 3: Write the failing tests**

Create `crates/sparql/src/exec/horn.rs` containing only the conversion functions and their tests for now:

```rust
//! `HornBackend` — the storage/WCOJ-backed implementation of the
//! [`Executor`](crate::exec::Executor) + [`Store`](crate::exec::Store)
//! seam (SPEC-07 wiring increment, issue #67).
//!
//! Term identity lives in `horndb_storage::Dictionary` (kind-tagged
//! `TermId`s — fixes the Stage-1 lexical type erasure). BGPs execute on
//! the SPEC-03 Leapfrog Triejoin over a lazily-rebuilt sorted snapshot.

use crate::algebra::{Term, TriplePattern};
use crate::error::{Result, SparqlError};
use crate::exec::runtime::{literal_parts, unescape_ntriples};
use oxrdf::{BlankNode, Literal, NamedNode, Term as OxTerm};

/// algebra::Term constant -> oxrdf::Term (dictionary key form).
/// Errors on variables and RDF 1.2 triple terms.
pub(crate) fn algebra_to_oxrdf(t: &Term) -> Result<OxTerm> {
    match t {
        Term::Iri(s) => Ok(OxTerm::NamedNode(NamedNode::new_unchecked(s.clone()))),
        Term::BlankNode(s) => Ok(OxTerm::BlankNode(BlankNode::new_unchecked(s.clone()))),
        Term::Literal(raw) => Ok(OxTerm::Literal(parse_literal(raw))),
        Term::Var(v) => Err(SparqlError::Executor(format!(
            "algebra_to_oxrdf called on variable ?{}",
            v.name()
        ))),
        Term::Triple(_) => Err(SparqlError::Executor(
            "RDF 1.2 triple terms are not supported by the storage backend yet".into(),
        )),
    }
}

/// N-Triples literal lexical form -> oxrdf::Literal.
/// `literal_parts` keeps the value escaped; unescape before building.
fn parse_literal(raw: &str) -> Literal {
    let (escaped, lang, dt) = literal_parts(raw);
    let value = unescape_ntriples(&escaped);
    match (lang, dt) {
        (Some(lang), _) => Literal::new_language_tagged_literal(value, lang)
            .unwrap_or_else(|_| Literal::new_simple_literal(raw)),
        (None, Some(dt)) => Literal::new_typed_literal(value, NamedNode::new_unchecked(dt)),
        (None, None) => Literal::new_simple_literal(value),
    }
}

/// oxrdf::Term -> algebra::Term, preserving kind (the point of #67).
pub(crate) fn oxrdf_to_algebra(t: &OxTerm) -> Term {
    match t {
        OxTerm::NamedNode(n) => Term::Iri(n.as_str().to_owned()),
        OxTerm::BlankNode(b) => Term::BlankNode(b.as_str().to_owned()),
        OxTerm::Literal(l) => Term::Literal(l.to_string()),
        // Triple terms never enter the backend (rejected on insert/lookup),
        // so this arm is unreachable in practice; degrade gracefully.
        #[allow(unreachable_patterns)]
        other => Term::Iri(other.to_string()),
    }
}

/// One lexical term in the `Engine::materialized_triples()` convention:
/// leading `"` = literal (N-Triples form), leading `_:` = blank node
/// (prefix stripped), anything else = bare IRI.
pub(crate) fn lexical_to_oxrdf(s: &str) -> OxTerm {
    if s.starts_with('"') {
        OxTerm::Literal(parse_literal(s))
    } else if let Some(label) = s.strip_prefix("_:") {
        OxTerm::BlankNode(BlankNode::new_unchecked(label))
    } else {
        OxTerm::NamedNode(NamedNode::new_unchecked(s))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::algebra::Var;

    #[test]
    fn literal_round_trips_through_oxrdf() {
        for raw in [
            "\"hello\"",
            "\"hej\"@sv",
            "\"42\"^^<http://www.w3.org/2001/XMLSchema#integer>",
            "\"a \\\"quoted\\\" word\"",
        ] {
            let ox = algebra_to_oxrdf(&Term::Literal(raw.to_owned())).unwrap();
            // xsd:string normalisation: oxrdf may render plain literals
            // identically; the invariant is algebra->oxrdf->algebra fixpoint.
            let back = oxrdf_to_algebra(&ox);
            assert_eq!(back, Term::Literal(raw.to_owned()), "round trip of {raw}");
        }
    }

    #[test]
    fn iri_and_bnode_conventions_match_translate() {
        let iri = algebra_to_oxrdf(&Term::Iri("http://ex/a".into())).unwrap();
        assert_eq!(oxrdf_to_algebra(&iri), Term::Iri("http://ex/a".into()));
        let b = algebra_to_oxrdf(&Term::BlankNode("b0".into())).unwrap();
        assert_eq!(oxrdf_to_algebra(&b), Term::BlankNode("b0".into()));
    }

    #[test]
    fn lexical_convention_covers_owlrl_dump_forms() {
        assert!(matches!(lexical_to_oxrdf("http://ex/a"), OxTerm::NamedNode(_)));
        match lexical_to_oxrdf("_:b0") {
            OxTerm::BlankNode(b) => assert_eq!(b.as_str(), "b0"),
            other => panic!("expected bnode, got {other:?}"),
        }
        assert!(matches!(lexical_to_oxrdf("\"x\"@en"), OxTerm::Literal(_)));
    }

    #[test]
    fn variables_are_rejected() {
        assert!(algebra_to_oxrdf(&Term::Var(Var::new("x"))).is_err());
    }
}
```

Register the module in `crates/sparql/src/exec/mod.rs` (after `pub mod mem;`):

```rust
pub mod horn;
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p horndb-sparql horn`
Expected: PASS. If `literal_round_trips_through_oxrdf` fails on the plain-literal case because `Literal::to_string()` renders `"hello"` differently than the input, inspect the actual output and align the test input with oxrdf's canonical rendering (the translator uses `Literal::to_string()` on the way in, so canonical-in = canonical-out is the real invariant).

- [ ] **Step 5: Commit**

```bash
git add crates/sparql/Cargo.toml Cargo.toml crates/sparql/src/exec/mod.rs crates/sparql/src/exec/horn.rs crates/sparql/src/exec/runtime.rs
git commit -m "feat(sparql): term conversion layer for the storage backend (#67)"
```

---

### Task 5: `HornBackend` — struct, `Store` impl, snapshot lifecycle

**Files:**
- Modify: `crates/sparql/src/exec/horn.rs`

- [ ] **Step 1: Write the failing tests**

Append to the test module in `horn.rs`:

```rust
    #[test]
    fn insert_and_delete_round_trip() {
        let mut b = HornBackend::new();
        b.insert_triple(
            Term::Iri("http://ex/s".into()),
            Term::Iri("http://ex/p".into()),
            Term::Literal("\"v\"".into()),
        );
        assert_eq!(b.len(), 1);
        b.delete_triple(
            &Term::Iri("http://ex/s".into()),
            &Term::Iri("http://ex/p".into()),
            &Term::Literal("\"v\"".into()),
        );
        assert_eq!(b.len(), 0);
        // Deleting an unknown triple is a no-op, not a panic.
        b.delete_triple(
            &Term::Iri("http://ex/nope".into()),
            &Term::Iri("http://ex/p".into()),
            &Term::Iri("http://ex/o".into()),
        );
        // Re-insert after delete resurrects the triple (tombstone cleared).
        b.insert_triple(
            Term::Iri("http://ex/s".into()),
            Term::Iri("http://ex/p".into()),
            Term::Literal("\"v\"".into()),
        );
        assert_eq!(b.len(), 1);
    }

    #[test]
    fn load_lexical_triples_accepts_owlrl_dump() {
        let mut b = HornBackend::new();
        b.load_lexical_triples(
            [
                (
                    "http://ex/s".to_owned(),
                    "http://ex/p".to_owned(),
                    "_:b0".to_owned(),
                ),
                (
                    "http://ex/s".to_owned(),
                    "http://ex/q".to_owned(),
                    "\"10\"^^<http://www.w3.org/2001/XMLSchema#integer>".to_owned(),
                ),
            ]
            .into_iter(),
        )
        .unwrap();
        assert_eq!(b.len(), 2);
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p horndb-sparql horn`
Expected: compile FAIL — `HornBackend` not found.

- [ ] **Step 3: Implement the struct and `Store` impl**

Add to `horn.rs` (above the test module):

```rust
use crate::exec::{Bindings, Executor, Store};
use horndb_storage::Store as ColumnStore;
use horndb_wcoj::ids::Triple as WTriple;
use horndb_wcoj::source::vec_source::VecTripleSource;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};

/// Storage + WCOJ backed SPARQL backend (issue #67).
///
/// * Term identity: `horndb_storage::Dictionary` (kind-tagged TermIds).
/// * Reads: Leapfrog Triejoin over a lazily-built [`VecTripleSource`]
///   snapshot (all six orderings, rebuilt after any mutation — a
///   documented Stage-1 cost; see INTEGRATION-NOTES.md).
/// * Writes: storage is insertion-only at Stage 1, so `DELETE DATA`
///   maintains a tombstone overlay applied at snapshot-build time.
///
/// Two literals with the same *value* but different lexical forms of
/// `xsd:integer` (e.g. `"042"` vs `"42"`) share an inline-int TermId;
/// matching is value-based for small integers and bound values decode
/// to the canonical lexical form. This is closer to SPARQL value
/// semantics than the Stage-1 MemStore's pure lexical matching.
pub struct HornBackend {
    store: ColumnStore,
    /// Raw `(s, p, o)` TermId payloads currently deleted.
    tombstones: HashSet<(u64, u64, u64)>,
    /// Live triple count (storage's count minus active tombstones).
    live: u64,
    /// Lazily-built WCOJ source. `None` after any mutation.
    snapshot: Mutex<Option<Arc<VecTripleSource>>>,
}

impl Default for HornBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl HornBackend {
    pub fn new() -> Self {
        Self {
            store: ColumnStore::in_memory(),
            tombstones: HashSet::new(),
            live: 0,
            snapshot: Mutex::new(None),
        }
    }

    /// Live triple count.
    pub fn len(&self) -> u64 {
        self.live
    }

    pub fn is_empty(&self) -> bool {
        self.live == 0
    }

    fn invalidate(&mut self) {
        *self.snapshot.get_mut().expect("snapshot lock poisoned") = None;
    }

    /// Insert one oxrdf triple. Returns true if it was new.
    pub fn insert_oxrdf(
        &mut self,
        s: &oxrdf::Term,
        p: &oxrdf::Term,
        o: &oxrdf::Term,
    ) -> Result<bool> {
        let key = self.intern_key(s, p, o)?;
        let existed = self.contains_key(key);
        let was_tombstoned = self.tombstones.remove(&key);
        if !existed {
            self.store
                .insert_triples(&[(s.clone(), p.clone(), o.clone())])
                .map_err(|e| SparqlError::Executor(format!("storage insert: {e}")))?;
        }
        let newly_live = !existed || was_tombstoned;
        if newly_live {
            self.live += 1;
            self.invalidate();
        }
        Ok(newly_live)
    }

    /// Bulk-load lexical triples in the `Engine::materialized_triples()`
    /// convention (IRIs bare, bnodes `_:`-prefixed, literals N-Triples).
    pub fn load_lexical_triples(
        &mut self,
        triples: impl Iterator<Item = (String, String, String)>,
    ) -> Result<u64> {
        let mut n = 0;
        for (s, p, o) in triples {
            if self.insert_oxrdf(
                &lexical_to_oxrdf(&s),
                &lexical_to_oxrdf(&p),
                &lexical_to_oxrdf(&o),
            )? {
                n += 1;
            }
        }
        Ok(n)
    }

    fn intern_key(
        &self,
        s: &oxrdf::Term,
        p: &oxrdf::Term,
        o: &oxrdf::Term,
    ) -> Result<(u64, u64, u64)> {
        let d = self.store.dictionary();
        let err = |e: horndb_storage::StorageError| SparqlError::Executor(format!("intern: {e}"));
        Ok((
            d.intern(s).map_err(err)?.0,
            d.intern(p).map_err(err)?.0,
            d.intern(o).map_err(err)?.0,
        ))
    }

    /// Membership against the *storage* layer (ignores tombstones).
    fn contains_key(&self, key: (u64, u64, u64)) -> bool {
        // Cheap path: consult the snapshot if it is current; otherwise
        // scan the (small) predicate partition.
        if let Some(snap) = self.snapshot.lock().expect("snapshot lock poisoned").as_ref() {
            return snap.contains(&WTriple::new(key.0, key.1, key.2))
                || self.tombstones.contains(&key);
        }
        self.store
            .scan_all_term_ids()
            .iter()
            .any(|t| (t.0 .0, t.1 .0, t.2 .0) == key)
    }

    /// Get-or-build the WCOJ snapshot.
    fn snapshot(&self) -> Arc<VecTripleSource> {
        let mut guard = self.snapshot.lock().expect("snapshot lock poisoned");
        if let Some(s) = guard.as_ref() {
            return Arc::clone(s);
        }
        let triples: Vec<WTriple> = self
            .store
            .scan_all_term_ids()
            .into_iter()
            .map(|(s, p, o)| (s.0, p.0, o.0))
            .filter(|k| !self.tombstones.contains(k))
            .map(|(s, p, o)| WTriple::new(s, p, o))
            .collect();
        let built = Arc::new(VecTripleSource::from_triples(triples));
        *guard = Some(Arc::clone(&built));
        built
    }
}

impl Store for HornBackend {
    fn insert_triple(&mut self, subject: Term, predicate: Term, object: Term) {
        let (Ok(s), Ok(p), Ok(o)) = (
            algebra_to_oxrdf(&subject),
            algebra_to_oxrdf(&predicate),
            algebra_to_oxrdf(&object),
        ) else {
            // Variables / triple terms cannot reach INSERT DATA (the
            // parser only produces ground quads); ignore defensively.
            return;
        };
        let _ = self.insert_oxrdf(&s, &p, &o);
    }

    fn delete_triple(&mut self, subject: &Term, predicate: &Term, object: &Term) {
        let (Ok(s), Ok(p), Ok(o)) = (
            algebra_to_oxrdf(subject),
            algebra_to_oxrdf(predicate),
            algebra_to_oxrdf(object),
        ) else {
            return;
        };
        let d = self.store.dictionary();
        // Non-interning lookups: a term the dictionary has never seen
        // cannot participate in any stored triple.
        let (Some(s), Some(p), Some(o)) = (d.get(&s), d.get(&p), d.get(&o)) else {
            return;
        };
        let key = (s.0, p.0, o.0);
        if self.contains_key(key) && self.tombstones.insert(key) {
            self.live -= 1;
            self.invalidate();
        }
    }
}
```

Note on `contains_key`'s snapshot path: a tombstoned triple is absent from the snapshot but still in storage — the `|| self.tombstones.contains(&key)` arm restores "present in storage" semantics so re-insert and double-delete bookkeeping stay correct.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p horndb-sparql horn`
Expected: PASS (Task 4 + Task 5 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/sparql/src/exec/horn.rs
git commit -m "feat(sparql): HornBackend storage-backed Store impl with tombstone deletes (#67)"
```

---

### Task 6: `Executor` impl — BGPs on the Leapfrog Triejoin

The heart of #67. Translate `algebra::TriplePattern`s into a `horndb_wcoj::pattern::Bgp`, run `horndb_wcoj::executor::Executor::for_bgp`, decode Arrow `RecordBatch` columns (`UInt64` TermIds, fields named `v{idx}`) back into `Bindings` with kind-correct terms.

**Files:**
- Modify: `crates/sparql/src/exec/horn.rs`
- Test: `crates/sparql/tests/exec_horn.rs` (create)

Key wcoj facts (verified against the code):
- `horndb_wcoj::pattern::{Bgp, TriplePattern, Term as WTerm, Var as WVar}` — `WVar(pub u8)`, `WTerm::Bound(u64) | WTerm::Var(WVar)`.
- `horndb_wcoj::executor::Executor::for_bgp(&source, &bgp, &Planner::default(), CancelToken::new())` yields `Result<RecordBatch>` items; columns are `UInt64`, field names `format!("v{}", var.0)` (`crates/wcoj/src/batch.rs:27`).
- ≤3 patterns route to binary-hash, ≥4 to WCOJ (`Planner::default()` cutover 4) — both must be covered by tests.
- The executor requires ≥1 variable in the BGP; fully-ground patterns are pre-filtered via `VecTripleSource::contains` (Task 3).
- A variable repeated *within one pattern* (`?x :p ?x`) is not safe to hand to the trie executor — rewrite the second occurrence to a fresh alias variable and post-filter rows where alias ≠ original.

- [ ] **Step 1: Write the failing tests**

Create `crates/sparql/tests/exec_horn.rs`:

```rust
//! HornBackend executor tests — mirrors the MemStore scenarios in
//! `mem.rs` plus the #67-specific behaviors (term typing, WCOJ routing,
//! ground patterns, repeated variables).

use horndb_sparql::algebra::{Term, TriplePattern, Var};
use horndb_sparql::api::{execute_query, QueryAnswer};
use horndb_sparql::exec::horn::HornBackend;
use horndb_sparql::exec::{Executor, Store};

fn iri(s: &str) -> Term {
    Term::Iri(format!("http://ex/{s}"))
}
fn lit(s: &str) -> Term {
    Term::Literal(format!("\"{s}\""))
}
fn var(s: &str) -> Term {
    Term::Var(Var::new(s))
}
fn pat(s: Term, p: Term, o: Term) -> TriplePattern {
    TriplePattern { subject: s, predicate: p, object: o }
}

fn store() -> HornBackend {
    let mut st = HornBackend::new();
    for (s, p, o) in [
        ("cw1", "a", "BlogPost"),
        ("cw2", "a", "BlogPost"),
        ("cw3", "a", "NewsItem"),
    ] {
        st.insert_triple(iri(s), iri(p), iri(o));
    }
    st.insert_triple(iri("cw1"), iri("title"), lit("First"));
    st.insert_triple(iri("cw1"), iri("body"), lit("Hello"));
    st.insert_triple(iri("cw2"), iri("title"), lit("Second"));
    st.insert_triple(iri("cw3"), iri("title"), lit("Third"));
    st
}

#[test]
fn two_pattern_join_binds_kind_correct_terms() {
    let st = store();
    let patterns = vec![
        pat(var("cw"), iri("a"), iri("BlogPost")),
        pat(var("cw"), iri("title"), var("t")),
    ];
    let mut rows: Vec<(Term, Term)> = st
        .scan_bgp(&patterns)
        .unwrap()
        .map(|b| (b.get("cw").unwrap().clone(), b.get("t").unwrap().clone()))
        .collect();
    rows.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
    assert_eq!(
        rows,
        vec![
            (iri("cw1"), lit("First")),
            (iri("cw2"), lit("Second")),
        ],
        "literals must come back as Term::Literal, not Term::Iri"
    );
}

#[test]
fn four_pattern_bgp_takes_wcoj_path() {
    // >= 4 patterns crosses Planner::default()'s WCOJ cutover.
    let st = store();
    let patterns = vec![
        pat(var("cw"), iri("a"), iri("BlogPost")),
        pat(var("cw"), iri("title"), var("t")),
        pat(var("cw"), iri("body"), var("b")),
        pat(var("cw2"), iri("a"), iri("BlogPost")),
    ];
    let rows: Vec<_> = st.scan_bgp(&patterns).unwrap().collect();
    // cw1 x {cw1, cw2}: only cw1 has a body.
    assert_eq!(rows.len(), 2);
}

#[test]
fn ground_pattern_filters_without_executor() {
    let st = store();
    // Present ground triple + one var pattern.
    let patterns = vec![
        pat(iri("cw1"), iri("a"), iri("BlogPost")),
        pat(var("x"), iri("a"), iri("NewsItem")),
    ];
    let rows: Vec<_> = st.scan_bgp(&patterns).unwrap().collect();
    assert_eq!(rows.len(), 1);
    // Absent ground triple zeroes the result.
    let patterns = vec![
        pat(iri("cw1"), iri("a"), iri("NewsItem")),
        pat(var("x"), iri("a"), iri("BlogPost")),
    ];
    assert_eq!(st.scan_bgp(&patterns).unwrap().count(), 0);
    // All-ground, all-present: exactly one empty row (ASK semantics).
    let patterns = vec![pat(iri("cw1"), iri("a"), iri("BlogPost"))];
    let rows: Vec<_> = st.scan_bgp(&patterns).unwrap().collect();
    assert_eq!(rows.len(), 1);
    assert!(rows[0].is_empty());
}

#[test]
fn repeated_variable_within_pattern_filters_to_diagonal() {
    let mut st = HornBackend::new();
    st.insert_triple(iri("a"), iri("likes"), iri("a"));
    st.insert_triple(iri("a"), iri("likes"), iri("b"));
    let patterns = vec![pat(var("x"), iri("likes"), var("x"))];
    let rows: Vec<_> = st.scan_bgp(&patterns).unwrap().collect();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get("x"), Some(&iri("a")));
}

#[test]
fn unknown_constant_yields_empty_not_error() {
    let st = store();
    let patterns = vec![pat(var("x"), iri("never-seen"), var("y"))];
    assert_eq!(st.scan_bgp(&patterns).unwrap().count(), 0);
}

#[test]
fn order_by_literal_object_uses_value_semantics() {
    // The #67 consequence-3 regression: numeric literals must ORDER BY
    // numerically (MemStore's IRI coercion broke this only for typing,
    // but the end-to-end path proves kinds survive the dictionary).
    let mut st = HornBackend::new();
    for (s, n) in [("x1", "10"), ("x2", "2"), ("x3", "30")] {
        st.insert_triple(
            iri(s),
            iri("count"),
            Term::Literal(format!(
                "\"{n}\"^^<http://www.w3.org/2001/XMLSchema#integer>"
            )),
        );
    }
    let q = "SELECT ?s WHERE { ?s <http://ex/count> ?n } ORDER BY ?n";
    match execute_query(q, &st).unwrap() {
        QueryAnswer::Solutions { rows, .. } => {
            let order: Vec<_> = rows
                .iter()
                .map(|r| r.get("s").unwrap().clone())
                .collect();
            assert_eq!(order, vec![iri("x2"), iri("x1"), iri("x3")]);
        }
        other => panic!("expected solutions, got {other:?}"),
    }
}

#[test]
fn empty_pattern_list_yields_single_empty_row() {
    let st = HornBackend::new();
    let rows: Vec<_> = st.scan_bgp(&[]).unwrap().collect();
    assert_eq!(rows.len(), 1);
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p horndb-sparql --test exec_horn`
Expected: compile FAIL — `Executor` not implemented for `HornBackend`.

- [ ] **Step 3: Implement `Executor`**

Add to `horn.rs`:

```rust
use horndb_wcoj::cancel::CancelToken;
use horndb_wcoj::executor::Executor as WcojExecutor;
use horndb_wcoj::pattern::{
    Bgp as WBgp, Term as WTerm, TriplePattern as WPattern, Var as WVar,
};
use horndb_wcoj::planner::Planner;
use std::collections::HashMap;

impl Executor for HornBackend {
    fn scan_bgp(
        &self,
        patterns: &[TriplePattern],
    ) -> Result<Box<dyn Iterator<Item = Bindings> + '_>> {
        // Stage-1 contract parity with MemStore: an empty BGP yields the
        // unit solution.
        if patterns.is_empty() {
            return Ok(Box::new(std::iter::once(Bindings::new())));
        }
        let snapshot = self.snapshot();
        let d = self.store.dictionary();

        // Variable table: name -> WVar index, in first-appearance order.
        let mut var_ids: HashMap<String, u8> = HashMap::new();
        let mut var_names: Vec<String> = Vec::new();
        // (original, alias) pairs from within-pattern repeated variables.
        let mut diagonal_filters: Vec<(String, String)> = Vec::new();
        let mut alloc_var = |name: &str,
                             var_ids: &mut HashMap<String, u8>,
                             var_names: &mut Vec<String>|
         -> Result<u8> {
            if let Some(&i) = var_ids.get(name) {
                return Ok(i);
            }
            let i = u8::try_from(var_names.len()).map_err(|_| {
                SparqlError::Executor("BGP exceeds 256 distinct variables".into())
            })?;
            var_ids.insert(name.to_owned(), i);
            var_names.push(name.to_owned());
            Ok(i)
        };

        let mut wpatterns: Vec<WPattern> = Vec::new();
        let mut ground: Vec<(u64, u64, u64)> = Vec::new();
        for p in patterns {
            // Resolve each slot. A constant absent from the dictionary
            // means the whole BGP has no solutions.
            let mut slots: [Option<WTerm>; 3] = [None, None, None];
            let mut seen_in_pattern: HashMap<String, u8> = HashMap::new();
            for (i, term) in [&p.subject, &p.predicate, &p.object].into_iter().enumerate() {
                slots[i] = Some(match term {
                    Term::Var(v) => {
                        let name = v.name();
                        if seen_in_pattern.contains_key(name) {
                            // ?x :p ?x — alias the repeat, filter later.
                            let alias = format!("__horndb_dup_{}_{}", name, i);
                            diagonal_filters.push((name.to_owned(), alias.clone()));
                            WTerm::Var(WVar(alloc_var(&alias, &mut var_ids, &mut var_names)?))
                        } else {
                            seen_in_pattern.insert(name.to_owned(), i as u8);
                            WTerm::Var(WVar(alloc_var(name, &mut var_ids, &mut var_names)?))
                        }
                    }
                    constant => {
                        let ox = algebra_to_oxrdf(constant)?;
                        match d.get(&ox) {
                            Some(id) => WTerm::Bound(id.0),
                            None => return Ok(Box::new(std::iter::empty())),
                        }
                    }
                });
            }
            let (s, p, o) = (
                slots[0].take().unwrap(),
                slots[1].take().unwrap(),
                slots[2].take().unwrap(),
            );
            match (s, p, o) {
                (WTerm::Bound(s), WTerm::Bound(p), WTerm::Bound(o)) => ground.push((s, p, o)),
                (s, p, o) => wpatterns.push(WPattern::new(s, p, o)),
            }
        }

        // Ground patterns: every one must be present, or no solutions.
        for (s, p, o) in &ground {
            if !snapshot.contains(&WTriple::new(*s, *p, *o)) {
                return Ok(Box::new(std::iter::empty()));
            }
        }
        if wpatterns.is_empty() {
            return Ok(Box::new(std::iter::once(Bindings::new())));
        }

        // Run the join and decode TermIds back to kind-correct terms.
        let bgp = WBgp::new(wpatterns);
        let planner = Planner::default();
        let exec = WcojExecutor::for_bgp(snapshot.as_ref(), &bgp, &planner, CancelToken::new());
        let mut out: Vec<Bindings> = Vec::new();
        for batch in exec {
            let batch = batch.map_err(|e| SparqlError::Executor(format!("wcoj: {e}")))?;
            let schema = batch.schema();
            // Column index per variable, resolved once per batch.
            let cols: Vec<(usize, &arrow::array::UInt64Array)> = Vec::new();
            let mut decoded_cols: Vec<(String, &arrow::array::UInt64Array)> = Vec::new();
            let _ = cols;
            for (idx, name) in var_names.iter().enumerate() {
                let field = format!("v{idx}");
                let Some((col_idx, _)) = schema.column_with_name(&field) else {
                    continue; // variable eliminated (shouldn't happen Stage-1)
                };
                let arr = batch
                    .column(col_idx)
                    .as_any()
                    .downcast_ref::<arrow::array::UInt64Array>()
                    .ok_or_else(|| {
                        SparqlError::Executor(format!("column {field} is not UInt64"))
                    })?;
                decoded_cols.push((name.clone(), arr));
            }
            for row in 0..batch.num_rows() {
                let mut b = Bindings::new();
                for (name, arr) in &decoded_cols {
                    let id = horndb_storage::TermId(arr.value(row));
                    let term = self
                        .store
                        .dictionary()
                        .lookup(id)
                        .map(|t| oxrdf_to_algebra(&t))
                        .ok_or_else(|| {
                            SparqlError::Executor(format!("dangling TermId {id:?}"))
                        })?;
                    b.set(name.clone(), term);
                }
                out.push(b);
            }
        }

        // Apply diagonal filters (?x :p ?x aliases) and strip the aliases.
        if !diagonal_filters.is_empty() {
            out.retain(|b| {
                diagonal_filters
                    .iter()
                    .all(|(orig, alias)| b.get(orig) == b.get(alias))
            });
            let alias_names: Vec<&str> =
                diagonal_filters.iter().map(|(_, a)| a.as_str()).collect();
            out = out
                .into_iter()
                .map(|b| {
                    let mut nb = Bindings::new();
                    for (k, v) in b.vars() {
                        if !alias_names.contains(&k) {
                            nb.set(k.to_owned(), v.clone());
                        }
                    }
                    nb
                })
                .collect();
        }
        Ok(Box::new(out.into_iter()))
    }
}
```

Check `crates/sparql/src/exec/horn.rs` imports compile: `horndb_storage::TermId` must be re-exported from the storage crate root (`crates/storage/src/lib.rs`) — verify with `grep "pub use" crates/storage/src/lib.rs`; if `TermId` / `StorageError` are not re-exported, import from their modules (`horndb_storage::term::TermId`) or add the re-export to storage's `lib.rs`. Same check for `horndb_wcoj` module paths (`ids`, `pattern`, `planner`, `cancel`, `executor`, `source::vec_source`) — confirm against `crates/wcoj/src/lib.rs` and adjust paths to whatever it actually exports. Remove the dead `cols` placeholder lines (artifact guard — write the final code without them).

- [ ] **Step 4: Run the tests**

Run: `cargo test -p horndb-sparql --test exec_horn`
Expected: PASS — all 7 tests. Debug specific failures against the wcoj fact list above before changing test expectations.

- [ ] **Step 5: Run the full sparql + wcoj + storage suites**

Run: `cargo test -p horndb-sparql -p horndb-wcoj -p horndb-storage`
Expected: PASS, no regressions.

- [ ] **Step 6: Commit**

```bash
git add crates/sparql/src/exec/horn.rs crates/sparql/tests/exec_horn.rs
git commit -m "feat(sparql): execute BGPs on storage-backed Leapfrog Triejoin (#67)"
```

---

### Task 7: W3C suite runs against both backends (harness-first rule)

The SPEC-07 harness subset (`harness/selected.toml` `[sparql_query]`, driven by `crates/sparql/tests/w3c_suite.rs`) must be green on the new backend, not just MemStore.

**Files:**
- Modify: `crates/sparql/tests/w3c_suite.rs`

- [ ] **Step 1: Genericize the loader and runner**

In `w3c_suite.rs`: `load_ntriples` currently returns `MemStore` and the test functions call it. Change `load_ntriples` to be generic:

```rust
fn load_ntriples<S: Store + Default>(path: &Path) -> S {
    let mut s = S::default();
    /* body unchanged — it only calls s.insert_triple(...) */
    s
}
```

Find the function(s) that iterate the `[sparql_query]` test list and execute each case (read the rest of the file first). Refactor the per-case execution into a generic helper `fn run_case<B: horndb_sparql::exec::FullBackend + Default>(case: &str)` and add a second test entry point:

```rust
#[test]
fn w3c_sparql_query_subset_memstore() {
    run_all::<MemStore>();
}

#[test]
fn w3c_sparql_query_subset_hornbackend() {
    run_all::<horndb_sparql::exec::horn::HornBackend>();
}
```

(Adapt names to the existing structure — keep the existing test name working if CI or `selected.toml` references it; check with `grep -rn "w3c_suite\|sparql_query" .github/ harness/ crates/harness/src/`.)

- [ ] **Step 2: Run the suite on both backends**

Run: `cargo test -p horndb-sparql --test w3c_suite`
Expected: PASS for both backends. Failures on the HornBackend leg are real #67 bugs — fix them in `horn.rs` (likely suspects: literal canonicalization mismatches, bnode conventions), do not relax expectations.

- [ ] **Step 3: Commit**

```bash
git add crates/sparql/tests/w3c_suite.rs
git commit -m "test(sparql): run the W3C sparql_query subset against HornBackend too (#67)"
```

---

### Task 8: Generic server state + `serve` on the real backend

**Files:**
- Modify: `crates/sparql/src/server/mod.rs`, `crates/sparql/src/server/query.rs`, `crates/sparql/src/server/update.rs`, `crates/sparql/src/bin/serve.rs`
- Test: `crates/sparql/tests/server_http.rs` (existing tests must keep passing; add one HornBackend round trip)

- [ ] **Step 1: Genericize `AppState`**

`crates/sparql/src/server/mod.rs`:

```rust
use crate::exec::FullBackend;
use crate::exec::mem::MemStore;

/// Shared state, generic over the storage backend. Defaults to the
/// Stage-1 `MemStore` so existing constructors keep compiling; the
/// `serve` binary instantiates `AppState<HornBackend>`.
pub struct AppState<B: FullBackend + Send + Sync + 'static = MemStore> {
    pub store: Arc<RwLock<B>>,
}

impl<B: FullBackend + Send + Sync + 'static> Clone for AppState<B> {
    fn clone(&self) -> Self {
        Self { store: Arc::clone(&self.store) }
    }
}

pub fn build_router<B: FullBackend + Send + Sync + 'static>(state: AppState<B>) -> Router {
    Router::new()
        .route(
            "/query",
            get(query::handle_query_get::<B>).post(query::handle_query_post::<B>),
        )
        .route("/update", post(update::handle_update::<B>))
        .with_state(state)
}
```

(`#[derive(Clone)]` on a generic struct bounds `B: Clone`, which is wrong — hence the manual impl.)

- [ ] **Step 2: Genericize the handlers**

In `server/query.rs` and `server/update.rs`, add `<B: FullBackend + Send + Sync + 'static>` to `handle_query_get`, `handle_query_post`, the private `run` helper, and `handle_update`; replace `State<AppState>` with `State<AppState<B>>`. The bodies are unchanged (`execute_query` is already generic over `E: Executor + ?Sized`, `execute_update` over `S: Store`).

- [ ] **Step 3: Verify existing server tests still pass**

Run: `cargo test -p horndb-sparql --features server --test server_http`
Expected: PASS unchanged (default type param keeps `AppState { store: Arc::new(RwLock::new(MemStore...)) }` compiling).

- [ ] **Step 4: Switch `serve` to `HornBackend`**

In `crates/sparql/src/bin/serve.rs`:
- Replace `MemStore` with `horndb_sparql::exec::horn::HornBackend`.
- Replace `load_file`'s `store.insert(lex_triple(...))` with `store.insert_oxrdf(&subject_term, &predicate_term, &object_term)` where the oxrdf parser's `t.subject` / `t.predicate` / `t.object` convert via `NamedOrBlankNode -> oxrdf::Term` (`NamedNode(n) => Term::NamedNode(n)`, `BlankNode(b) => Term::BlankNode(b)`) and `t.predicate` via `Term::NamedNode(t.predicate)`. Delete the now-unused `lex_triple` / `subject_lex` / `object_lex` helpers.
- Keep the per-file count logging; `store.len()` still works (Task 5 added it).
- Update the module doc comment: the binary now serves the dictionary-encoded storage + WCOJ backend.

- [ ] **Step 5: Add one HornBackend HTTP round trip test**

In `crates/sparql/tests/server_http.rs`, copy the simplest existing query round-trip test, rename it with a `_hornbackend` suffix, and construct `AppState::<HornBackend> { store: Arc::new(RwLock::new(backend)) }` seeded via `insert_triple`. Assert the same response body.

- [ ] **Step 6: Run**

Run: `cargo test -p horndb-sparql --features server`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/sparql/src/server/ crates/sparql/src/bin/serve.rs crates/sparql/tests/server_http.rs
git commit -m "feat(sparql): generic server state; serve binary runs on HornBackend (#67)"
```

---

### Task 9: owlrl closure into the same store (`reasoner` feature)

**Files:**
- Modify: `crates/sparql/Cargo.toml`, `crates/sparql/src/exec/horn.rs`, `crates/sparql/src/bin/serve.rs`
- Test: `crates/sparql/tests/exec_horn.rs`

- [ ] **Step 1: Feature + dependency**

`crates/sparql/Cargo.toml`:

```toml
[dependencies]
horndb-owlrl = { path = "../owlrl", optional = true }

[features]
default = ["server", "reasoner"]
reasoner = ["dep:horndb-owlrl"]
```

(No GraphBLAS: owlrl's default `RuleFiringBackend` is dependency-free; the `graphblas-backend` feature stays off.)

- [ ] **Step 2: Write the failing test**

Append to `crates/sparql/tests/exec_horn.rs`:

```rust
#[cfg(feature = "reasoner")]
#[test]
fn materialized_closure_is_queryable() {
    use oxrdf::{Dataset, NamedNode, Quad, GraphName};
    let nn = |s: &str| NamedNode::new(s).unwrap();
    let mut dataset = Dataset::default();
    // :Penguin rdfs:subClassOf :Bird . :pingu a :Penguin .
    dataset.insert(&Quad::new(
        nn("http://ex/Penguin"),
        nn("http://www.w3.org/2000/01/rdf-schema#subClassOf"),
        nn("http://ex/Bird"),
        GraphName::DefaultGraph,
    ));
    dataset.insert(&Quad::new(
        nn("http://ex/pingu"),
        nn("http://www.w3.org/1999/02/22-rdf-syntax-ns#type"),
        nn("http://ex/Penguin"),
        GraphName::DefaultGraph,
    ));
    let mut backend = HornBackend::new();
    let stats = horndb_sparql::exec::horn::load_with_reasoning(&mut backend, &dataset).unwrap();
    assert!(stats.loaded >= 2);
    // cax-sco: pingu must now be a Bird, visible through SPARQL.
    let q = "ASK { <http://ex/pingu> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://ex/Bird> }";
    match execute_query(q, &backend).unwrap() {
        QueryAnswer::Boolean(b) => assert!(b, "inferred triple must be queryable"),
        other => panic!("expected boolean, got {other:?}"),
    }
}
```

Run: `cargo test -p horndb-sparql --test exec_horn materialized_closure` → compile FAIL (no `load_with_reasoning`).

- [ ] **Step 3: Implement `load_with_reasoning`**

Append to `horn.rs`:

```rust
/// Statistics from a reasoning load.
#[cfg(feature = "reasoner")]
#[derive(Debug, Clone, Copy)]
pub struct ReasonStats {
    /// Triples loaded into the backend (asserted base + inferred).
    pub loaded: u64,
    /// Asserted triples in the input dataset's default graph.
    pub asserted: usize,
}

/// Run the OWL 2 RL `horndb_owlrl::Engine` (RuleFiring backend) over
/// `dataset`'s default graph and load the full materialized closure —
/// asserted base plus everything inferred — into `backend`. This is the
/// #67 replacement for the dump-to-flat-file round trip: the served
/// store and the reasoner see the same triples.
#[cfg(feature = "reasoner")]
pub fn load_with_reasoning(
    backend: &mut HornBackend,
    dataset: &oxrdf::Dataset,
) -> Result<ReasonStats> {
    let mut engine = horndb_owlrl::integration::Engine::new();
    engine
        .load(dataset)
        .map_err(|e| SparqlError::Executor(format!("owlrl load: {e}")))?;
    let asserted = engine.asserted_len().unwrap_or(0);
    let triples = engine
        .materialized_triples()
        .ok_or_else(|| SparqlError::Executor("owlrl produced no state".into()))?;
    let loaded = backend.load_lexical_triples(triples.into_iter())?;
    Ok(ReasonStats { loaded, asserted })
}
```

Verify the `Engine` path: `grep -n "pub mod integration\|pub use" crates/owlrl/src/lib.rs` and adjust the import (`horndb_owlrl::Engine` if re-exported).

- [ ] **Step 4: Run the test**

Run: `cargo test -p horndb-sparql --test exec_horn`
Expected: PASS (the owlrl build step compiles `rules.toml` codegen — the first build is slow; that is normal).

- [ ] **Step 5: `serve --materialize`**

In `serve.rs` add to `Cli`:

```rust
    /// Run OWL 2 RL materialization over the loaded data and serve the
    /// closure (requires the `reasoner` feature, on by default).
    #[arg(long = "materialize", default_value_t = false)]
    materialize: bool,
```

In `main`, collect parsed triples into both the backend path and (when `--materialize`) an `oxrdf::Dataset`; structure:

```rust
    if cli.materialize {
        #[cfg(feature = "reasoner")]
        {
            let stats = horndb_sparql::exec::horn::load_with_reasoning(&mut store, &dataset)
                .context("materializing OWL 2 RL closure")?;
            eprintln!(
                "serve: materialized closure — {} asserted, {} total loaded",
                stats.asserted, stats.loaded
            );
        }
        #[cfg(not(feature = "reasoner"))]
        anyhow::bail!("--materialize requires the `reasoner` feature");
    } else {
        /* insert parsed triples directly, as in Task 8 */
    }
```

Implementation detail: parse each file once into `Vec<(oxrdf::Term, oxrdf::Term, oxrdf::Term)>`; build the `Dataset` only when `--materialize` (wrap each triple in `Quad::new(s, p, o, GraphName::DefaultGraph)` — subjects need `Term -> NamedOrBlankNode` conversion; keep the original `NamedOrBlankNode`/`NamedNode` types from the parser instead of converting through `Term` to avoid lossy round trips).

- [ ] **Step 6: Smoke the binary**

```bash
printf '<http://ex/Penguin> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <http://ex/Bird> .\n<http://ex/pingu> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://ex/Penguin> .\n' > /tmp/t67.nt
cargo run -p horndb-sparql --features server --bin serve -- --data /tmp/t67.nt --materialize --bind 127.0.0.1:7979 &
sleep 2
curl -s 'http://127.0.0.1:7979/query?query=ASK%20%7B%20%3Chttp%3A%2F%2Fex%2Fpingu%3E%20%3Chttp%3A%2F%2Fwww.w3.org%2F1999%2F02%2F22-rdf-syntax-ns%23type%3E%20%3Chttp%3A%2F%2Fex%2FBird%3E%20%7D'
kill %1
```

Expected: a SPARQL JSON/XML body with boolean `true`.

- [ ] **Step 7: Commit**

```bash
git add crates/sparql/Cargo.toml crates/sparql/src/exec/horn.rs crates/sparql/src/bin/serve.rs crates/sparql/tests/exec_horn.rs
git commit -m "feat(sparql): load the owlrl materialized closure into the served store (#67)"
```

---

### Task 10: Scale smoke test

Issue #67's consequence 2: the MemStore nested loop timed out (>20 s) at ~500 K triples. Prove the new path handles a six-figure store in test-grade time.

**Files:**
- Modify: `crates/sparql/tests/exec_horn.rs`

- [ ] **Step 1: Add the smoke test**

```rust
/// #67 consequence-2 regression: a multi-pattern query over a
/// six-figure store must complete in test-grade time (the Stage-1
/// MemStore nested loop needed >20 s at this scale). Debug-build
/// timings are noisy, so the bound is generous; the point is
/// "seconds, not minutes".
#[test]
fn six_figure_store_multi_pattern_smoke() {
    let mut st = HornBackend::new();
    let n: usize = 100_000;
    for i in 0..n {
        let s = iri(&format!("e{i}"));
        st.insert_triple(s.clone(), iri("a"), iri(&format!("T{}", i % 50)));
        st.insert_triple(
            s.clone(),
            iri("score"),
            Term::Literal(format!(
                "\"{}\"^^<http://www.w3.org/2001/XMLSchema#integer>",
                i % 1000
            )),
        );
        st.insert_triple(s, iri("next"), iri(&format!("e{}", (i + 1) % n)));
    }
    let started = std::time::Instant::now();
    let q = "SELECT ?x ?y WHERE { \
        ?x <http://ex/a> <http://ex/T7> . \
        ?x <http://ex/next> ?y . \
        ?y <http://ex/a> <http://ex/T8> . \
        ?x <http://ex/score> ?s . }";
    match execute_query(q, &st).unwrap() {
        QueryAnswer::Solutions { rows, .. } => assert_eq!(rows.len(), 2000),
        other => panic!("expected solutions, got {other:?}"),
    }
    assert!(
        started.elapsed() < std::time::Duration::from_secs(30),
        "query took {:?}",
        started.elapsed()
    );
}
```

Note the insert loop builds 300 K triples one at a time — each `insert_triple` invalidates the snapshot but does **not** rebuild it (lazy), so loading is O(n) dictionary work; only the first query pays the sort. If the *load* itself is the slow part in debug builds (>60 s), reduce `n` to 50_000 and halve the expected row count.

Sanity-check the expected row count before trusting it: `?x ∈ T7` ⇔ `i % 50 == 7`, `?y = e(i+1) ∈ T8` ⇔ `(i+1) % 50 == 8` — both hold for every `i ≡ 7 (mod 50)`, so `n/50 = 2000` rows.

- [ ] **Step 2: Run it (release-ish)**

Run: `cargo test -p horndb-sparql --test exec_horn six_figure -- --nocapture`
Expected: PASS well inside the bound. Record the elapsed time for the PR description.

- [ ] **Step 3: Commit**

```bash
git add crates/sparql/tests/exec_horn.rs
git commit -m "test(sparql): six-figure-store smoke for the WCOJ-backed executor (#67)"
```

---

### Task 11: Docs bookkeeping (Phase-7 of /next-task — no TASKS.md here)

**Files:**
- Modify: `docs/architecture.md`, `crates/sparql/INTEGRATION-NOTES.md`

- [ ] **Step 1: architecture.md**

Read the SPEC-07 section of `docs/architecture.md` and update the rows that describe the executor/store wiring (the issue quotes them as having been corrected to "MemStore executor"): the BGP execution row flips to **implemented** — "BGPs route to the storage-backed WCOJ executor (`exec::horn::HornBackend`); `MemStore` retained as the test double". Mention term-kind preservation and the closure-into-store load path (`load_with_reasoning`). Do **not** touch `TASKS.md` (the `/next-task` flow flips it on `main` after merge).

- [ ] **Step 2: INTEGRATION-NOTES.md (sparql)**

Append a dated section documenting:
- `HornBackend` design: dictionary-owned term identity, tombstone deletes over insertion-only storage, lazily-rebuilt `VecTripleSource` snapshot (all six orderings eagerly sorted — ~144 bytes/triple transient cost; replacing it with a direct `TripleSource` over the columnar partitions is the named follow-up).
- Inline-int value semantics (`"042"` ≡ `"42"` for i32 `xsd:integer`).
- The `reasoner` feature and `load_with_reasoning`.
- GRAPH patterns remain unscoped (unchanged from Stage 1; named-graph scoping is still future work under #7).

- [ ] **Step 3: Commit**

```bash
git add docs/architecture.md crates/sparql/INTEGRATION-NOTES.md
git commit -m "docs: SPEC-07 storage/WCOJ/closure wiring status + integration notes (#67)"
```

---

### Task 12: Full verification gate

- [ ] **Step 1: Run the Phase-6 gate**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test -p horndb-sparql --features server
```

Expected: all green. Fix anything red before proceeding (clippy on the new generics often flags `type_complexity` — add a type alias rather than `#[allow]` where reasonable).

- [ ] **Step 2: Commit any fixups**

One commit per logical fix; no "fix clippy" mega-commits.

## Self-review checklist (done during planning)

- Spec coverage: issue #67 consequences — (1) decoupled data → Tasks 8+9 (same store, no flat-file path); (2) naive executor at scale → Tasks 6+10; (3) term-type erasure → Tasks 4+6 (dictionary kinds) with regression test; single-RwLock sharp edge → pre-existing RwLock retained, documented; full MVCC is #19/#46, out of scope.
- Harness-first: Task 7 runs the SPEC-07 `[sparql_query]` subset on the new backend.
- Placeholder scan: all code blocks complete; the two "verify the import path" notes are deliberate (re-export layout varies) with exact grep commands.
- Type consistency: `HornBackend::len() -> u64` used by serve; `insert_oxrdf(&Term, &Term, &Term) -> Result<bool>`; `load_lexical_triples(impl Iterator<Item=(String,String,String)>) -> Result<u64>`; `load_with_reasoning(&mut HornBackend, &Dataset) -> Result<ReasonStats>` — consistent across Tasks 5/8/9.
