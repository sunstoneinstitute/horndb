# id-based slot rows for the SPARQL runtime — Slice 1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stop decoding `TermId → String` for every SPARQL result row before aggregation runs — run the runtime on dictionary ids, decode once at the result boundary — so the ~12× SPB aggregation gap (#128) starts to close.

**Architecture:** Introduce a slot row (`Batch{schema, rows}`, `Row(Vec<Slot>)`, `Slot{Id|Term|Unbound}`) as the type `Runtime::eval` produces. `HornBackend::scan_bgp_ids` reads the WCOJ `UInt64Array` columns straight into `Slot::Id` cells (no decode). Equality-based operators (Join/Group/Distinct/Project + Filter) run on ids; everything else rides a `from_bindings`/`to_bindings` decode-adapter that wraps today's string code unchanged. `Runtime::run` decodes the final `Batch` to `Vec<Bindings>` once, so the public API, serializers, and HTTP server are untouched.

**Tech Stack:** Rust 1.90 (pinned), `horndb-sparql` crate, `horndb-storage::TermId`, `arrow::array::UInt64Array`, `proptest` (workspace dep), `cargo nextest`.

**Source of truth:** `docs/specs/2026-06-28-id-based-slot-rows-design.md`. Read it before starting.

---

## Assumptions / deviations (decided during planning — flag if you disagree)

1. **The `Resolver` capability is realized as two `Executor` trait methods with defaults, not a separate `Resolver` supertrait.** There are three `Executor` impls (`HornBackend`, `MemStore`, and the test double `KindAwareStore` in `tests/exec_describe.rs:285`). A `Resolver` supertrait would force all three to implement it for no benefit. Instead: `Executor::scan_bgp_ids` (default adapts the existing string scan) and `Executor::decode_term` (default errors — never called for backends that produce no `Slot::Id`). Only `HornBackend` overrides them. `Runtime` already holds `&E: Executor`, so it decodes via `self.exec.decode_term(id)`. This matches the spec's intent (one decode seam) with minimal surface.
2. **The decode boundary is `Runtime::run`, so `api.rs` does not change.** `run` calls `eval` (→ `Batch`) then `batch.to_bindings(|id| self.exec.decode_term(id))` → `Vec<Bindings>`, preserving its current `IntoIter<Bindings>` return type. All four call sites (Select/Ask/Construct/Describe) keep working verbatim.
3. **Native `Join` stays a nested loop (slot parity with today's `Join`), not a new hash join.** Today's `Join` (`runtime.rs:29-41`) is already a nested loop; this slice swaps its row type to slots and makes its equality checks id-based. A hash join is a separate optimization (Slice 2 / follow-up) to keep this slice a faithful representation swap.
4. **Within-column homogeneity invariant:** within one `Batch`, a given slot index is uniformly `Id`, uniformly `Term`, or uniformly `Unbound` across rows (scan ⇒ all `Id`; `from_bindings` ⇒ all `Term`; native ops preserve per-column provenance). This lets `Distinct`/`Group`/`Join` keys hash on raw ids without an `Id`-vs-equal-`Term` collision. The slot **equality** rule (decode on mix) remains the correctness backstop. Documented in `batch.rs`.
5. **`Batch` schema order is free.** The final `to_bindings` builds a `Bindings` (`BTreeMap`), which re-sorts vars by name, so schema column order never affects serialized output. Only the *set* of `(var, value)` pairs must match today.

---

## File structure

- **Create** `crates/sparql/src/exec/batch.rs` — `Slot`, `Row`, `Batch`, `KeyPart`, slot equality + key helpers, `from_bindings`/`to_bindings`, unit tests. One responsibility: the slot row type and its conversions.
- **Modify** `crates/sparql/src/exec/mod.rs` — `pub mod batch;` + re-exports; add `scan_bgp_ids` + `decode_term` default methods to `Executor`.
- **Modify** `crates/sparql/src/exec/horn.rs` — `HornBackend` impls of `scan_bgp_ids` (id-returning scan) and `decode_term` (dict lookup).
- **Modify** `crates/sparql/src/exec/runtime.rs` — `eval` returns `Batch`; adapter wiring; native operator ports; `referenced_vars`; transient-Bindings decode; `cfg(test)` `eval_legacy` oracle; differential proptest.
- **Modify** `crates/sparql/Cargo.toml` — add `proptest` to `[dev-dependencies]`.
- **Modify** docs (final task): `TASKS.md`, `docs/architecture.md`, `BENCHMARKS.md`, GitHub issue #128.

**Dependency order (hard barrier marked):** Task 1 → Task 2 → **Task 3 (eval→Batch, all-adapter; BARRIER)** → Tasks 4–7 (native ports, each independent and parallelizable after Task 3) → Task 8 (differential test) → Task 9 (measure + docs).

| Task | Depends on | Parallel with |
|---|---|---|
| 1 Core slot types | — | — |
| 2 Executor seam | 1 | — |
| 3 `eval`→`Batch`, all-adapter | 1, 2 | — (barrier) |
| 4 Native BgpScan + Slice | 3 | 5, 6, 7 |
| 5 Native Project + Distinct | 3 | 4, 6, 7 |
| 6 Native Group | 3 | 4, 5, 7 |
| 7 Native Filter + Join | 3 | 4, 5, 6 |
| 8 Differential proptest | 4–7 | — |
| 9 Measure + docs-sync | 8 | — |

> Tasks 4–7 each touch only their own arm(s) of the `eval` match plus private helpers. They can be dispatched in parallel after Task 3, but if executed by one worker, do them in order 4→5→6→7 and commit between each.

---

## Task 1: Core slot types (`batch.rs`)

**Files:**
- Create: `crates/sparql/src/exec/batch.rs`
- Modify: `crates/sparql/src/exec/mod.rs` (add `pub mod batch;` and re-export)

- [ ] **Step 1: Write the failing unit test**

Create `crates/sparql/src/exec/batch.rs` with only the test module first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::algebra::Term;
    use horndb_storage::TermId;

    // Fake resolver: decode id N to Iri "t{N}".
    fn decode(id: TermId) -> crate::error::Result<Term> {
        Ok(Term::Iri(format!("t{}", id.0)))
    }

    #[test]
    fn id_equals_id_by_id_no_decode() {
        // Same id → equal; different id → not equal. Decode must NOT be called
        // (pass a panicking resolver to prove it).
        let panic = |_: TermId| -> crate::error::Result<Term> { panic!("decoded") };
        assert!(Slot::eq(&Slot::Id(TermId(5)), &Slot::Id(TermId(5)), panic).unwrap());
        assert!(!Slot::eq(&Slot::Id(TermId(5)), &Slot::Id(TermId(6)), panic).unwrap());
    }

    #[test]
    fn id_vs_term_compares_by_decoded_term() {
        // Id(5) decodes to Iri("t5"); equal to Term(Iri("t5")), unequal to Term(Iri("x")).
        assert!(Slot::eq(&Slot::Id(TermId(5)), &Slot::Term(Term::Iri("t5".into())), decode).unwrap());
        assert!(!Slot::eq(&Slot::Id(TermId(5)), &Slot::Term(Term::Iri("x".into())), decode).unwrap());
    }

    #[test]
    fn term_vs_term_and_unbound() {
        assert!(Slot::eq(&Slot::Term(Term::Iri("a".into())), &Slot::Term(Term::Iri("a".into())), decode).unwrap());
        assert!(!Slot::eq(&Slot::Term(Term::Iri("a".into())), &Slot::Term(Term::Iri("b".into())), decode).unwrap());
        // Unbound equals only Unbound.
        assert!(Slot::eq(&Slot::Unbound, &Slot::Unbound, decode).unwrap());
        assert!(!Slot::eq(&Slot::Unbound, &Slot::Id(TermId(5)), decode).unwrap());
    }

    #[test]
    fn from_then_to_bindings_roundtrips() {
        use crate::exec::Bindings;
        let mut b = Bindings::new();
        b.set("s", Term::Iri("http://x".into()));
        b.set("o", Term::Literal("\"1\"".into()));
        let batch = Batch::from_bindings(vec![b.clone()]);
        let back = batch.to_bindings(decode).unwrap();
        assert_eq!(back, vec![b]);
    }
}
```

- [ ] **Step 2: Run it to verify it fails (does not compile)**

Run: `cargo test -p horndb-sparql --lib exec::batch 2>&1 | head -20`
Expected: FAIL — `Slot`, `Batch` not found.

- [ ] **Step 3: Implement the types above the test module**

Prepend to `crates/sparql/src/exec/batch.rs`:

```rust
//! Slot rows — the id-carrying runtime row that replaces the
//! string-decoded `Bindings` above the executor seam (#128).
//!
//! Within-column homogeneity invariant: in a single `Batch`, a given slot
//! index is uniformly `Id`, uniformly `Term`, or uniformly `Unbound` across
//! rows (scan ⇒ all `Id`; `from_bindings` ⇒ all `Term`; native operators
//! preserve per-column provenance). Equality keys may therefore hash on raw
//! ids without an `Id`-vs-equal-`Term` collision; the `Slot::eq` decode path
//! is the correctness backstop for genuinely mixed comparisons.

use crate::algebra::{Term, Var};
use crate::error::Result;
use crate::exec::Bindings;
use horndb_storage::TermId;

/// One cell of a solution row.
#[derive(Debug, Clone, PartialEq)]
pub enum Slot {
    /// Dictionary id straight from the scan — the hot, no-string case.
    Id(TermId),
    /// A materialized term: BIND/aggregate output, VALUES literal,
    /// path-synthesized, or any cell produced by the decode-adapter.
    Term(Term),
    /// No binding (OPTIONAL right-side with no match).
    Unbound,
}

impl Slot {
    /// SPARQL term-identity equality. `Id == Id` compares ids directly (no
    /// decode); any other mix decodes both sides and compares by term.
    /// `Unbound` equals only `Unbound`.
    pub fn eq(
        a: &Slot,
        b: &Slot,
        decode: impl Fn(TermId) -> Result<Term>,
    ) -> Result<bool> {
        Ok(match (a, b) {
            (Slot::Id(x), Slot::Id(y)) => x == y,
            (Slot::Unbound, Slot::Unbound) => true,
            (Slot::Unbound, _) | (_, Slot::Unbound) => false,
            _ => {
                let ta = a.to_term(&decode)?;
                let tb = b.to_term(&decode)?;
                ta == tb
            }
        })
    }

    /// Decode this slot to a `Term`. `Unbound` is an error (callers must
    /// check boundness first); `Id` goes through `decode`, `Term` clones.
    fn to_term(&self, decode: &impl Fn(TermId) -> Result<Term>) -> Result<Term> {
        match self {
            Slot::Id(id) => decode(*id),
            Slot::Term(t) => Ok(t.clone()),
            Slot::Unbound => Err(crate::error::SparqlError::Executor(
                "to_term on Unbound slot".into(),
            )),
        }
    }

    /// A hash/Ord key for equality grouping (Distinct / Group / Join key).
    /// Relies on within-column homogeneity (see module docs): `Id` keys on
    /// the raw id, `Term` on its lexical form, so two homogeneous columns
    /// never produce a false `Id`-vs-`Term` collision.
    pub fn key_part(&self) -> KeyPart {
        match self {
            Slot::Id(id) => KeyPart::Id(id.0),
            Slot::Term(t) => KeyPart::Lex(crate::exec::runtime::lex(t)),
            Slot::Unbound => KeyPart::Unbound,
        }
    }
}

/// A grouping/equality key fragment for one slot. `Ord` so group output can
/// be made deterministic; `Lex` carries the term's lexical form.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum KeyPart {
    Id(u64),
    Lex(String),
    Unbound,
}

/// One solution row: `slots[i]` is the value of `schema[i]`.
#[derive(Debug, Clone, PartialEq)]
pub struct Row(pub Vec<Slot>);

/// A block of rows sharing one schema.
#[derive(Debug, Clone, PartialEq)]
pub struct Batch {
    pub schema: Vec<Var>,
    pub rows: Vec<Row>,
}

impl Batch {
    /// Zero rows over the empty schema.
    pub fn empty() -> Self {
        Batch { schema: Vec::new(), rows: Vec::new() }
    }

    /// One empty row over the empty schema — the BGP/ASK unit ("one empty
    /// solution mapping").
    pub fn unit() -> Self {
        Batch { schema: Vec::new(), rows: vec![Row(Vec::new())] }
    }

    /// Slot index of `var`, if present in the schema.
    pub fn col(&self, var: &str) -> Option<usize> {
        self.schema.iter().position(|v| v.name() == var)
    }

    /// Wrap decoded `Bindings` rows as a `Batch` of `Slot::Term` cells. The
    /// schema is the sorted union of all bound variable names; a row that
    /// does not bind a schema var gets `Slot::Unbound` there. This is the
    /// decode-adapter's re-encode half (the inverse of `to_bindings`).
    pub fn from_bindings(rows: Vec<Bindings>) -> Self {
        use std::collections::BTreeSet;
        let mut names: BTreeSet<String> = BTreeSet::new();
        for b in &rows {
            for k in b.keys() {
                names.insert(k.to_owned());
            }
        }
        let schema: Vec<Var> = names.iter().map(|n| Var::new(n.as_str())).collect();
        let out_rows = rows
            .iter()
            .map(|b| {
                Row(schema
                    .iter()
                    .map(|v| match b.get(v.name()) {
                        Some(t) => Slot::Term(t.clone()),
                        None => Slot::Unbound,
                    })
                    .collect())
            })
            .collect();
        Batch { schema, rows: out_rows }
    }

    /// Decode every row to a `Bindings`, the result-boundary step. `Unbound`
    /// slots contribute no key (matching today's "var simply absent"). `Id`
    /// slots go through `decode`; `Term` slots clone.
    pub fn to_bindings(
        &self,
        decode: impl Fn(TermId) -> Result<Term>,
    ) -> Result<Vec<Bindings>> {
        let mut out = Vec::with_capacity(self.rows.len());
        for row in &self.rows {
            let mut b = Bindings::new();
            for (i, slot) in row.0.iter().enumerate() {
                match slot {
                    Slot::Id(id) => b.set(self.schema[i].name().to_owned(), decode(*id)?),
                    Slot::Term(t) => b.set(self.schema[i].name().to_owned(), t.clone()),
                    Slot::Unbound => {}
                }
            }
            out.push(b);
        }
        Ok(out)
    }
}
```

Then in `crates/sparql/src/exec/mod.rs`, after the existing `pub mod runtime;` (line ~9) add:

```rust
pub mod batch;
pub use batch::{Batch, KeyPart, Row, Slot};
```

> `key_part` calls `crate::exec::runtime::lex`. `lex` is currently a private free fn in `runtime.rs:869`. Change its declaration from `fn lex(` to `pub(crate) fn lex(` in this task.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p horndb-sparql --lib exec::batch 2>&1 | tail -20`
Expected: PASS (4 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/sparql/src/exec/batch.rs crates/sparql/src/exec/mod.rs crates/sparql/src/exec/runtime.rs
git commit -F - <<'EOF'
feat(sparql): slot row types (Slot/Row/Batch) for id-based runtime

First step of #128 id-based bindings: the slot row that replaces the
string-decoded Bindings above the executor seam. Equality decode rule +
from/to_bindings adapter, with the within-column homogeneity invariant
documented. No runtime behavior change yet.
EOF
```

---

## Task 2: Executor seam — `scan_bgp_ids` + `decode_term`

**Files:**
- Modify: `crates/sparql/src/exec/mod.rs` (add two defaulted trait methods)
- Modify: `crates/sparql/src/exec/horn.rs` (override both for `HornBackend`)
- Test: `crates/sparql/tests/exec_horn.rs` (parity test)

- [ ] **Step 1: Write the failing parity test**

Append to `crates/sparql/tests/exec_horn.rs`:

```rust
#[test]
fn scan_bgp_ids_decodes_to_same_rows_as_scan_bgp() {
    use horndb_sparql::algebra::{Term, TriplePattern, Var};
    use horndb_sparql::exec::Executor;

    // Build a tiny graph in a HornBackend.
    let mut be = horndb_sparql::exec::horn::HornBackend::new();
    be.insert_triple(
        Term::Iri("http://ex/a".into()),
        Term::Iri("http://ex/p".into()),
        Term::Iri("http://ex/b".into()),
    );
    be.insert_triple(
        Term::Iri("http://ex/c".into()),
        Term::Iri("http://ex/p".into()),
        Term::Iri("http://ex/d".into()),
    );

    let pat = vec![TriplePattern {
        subject: Term::Var(Var::new("s")),
        predicate: Term::Iri("http://ex/p".into()),
        object: Term::Var(Var::new("o")),
    }];

    // Legacy string scan.
    let mut legacy: Vec<_> = be.scan_bgp(&pat).unwrap().collect();
    // Id scan, decoded at the boundary.
    let batch = be.scan_bgp_ids(&pat).unwrap();
    let mut ids: Vec<_> = batch.to_bindings(|id| be.decode_term(id)).unwrap();

    // Compare as sets (row order is executor-defined).
    let key = |b: &horndb_sparql::exec::Bindings| {
        let mut v: Vec<String> = b.vars().map(|(k, t)| format!("{k}={t:?}")).collect();
        v.sort();
        v.join(",")
    };
    legacy.sort_by_key(key);
    ids.sort_by_key(key);
    assert_eq!(ids, legacy);
}
```

> Check the existing top of `tests/exec_horn.rs` for how it constructs a `HornBackend` and inserts triples; reuse that helper if one exists instead of `insert_triple` directly. `HornBackend`, `Bindings`, `Executor`, and `algebra` must be re-exported from the crate root — they already are (the file's other tests use them); match those import paths.

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p horndb-sparql --test exec_horn scan_bgp_ids_decodes 2>&1 | head -20`
Expected: FAIL — `scan_bgp_ids`/`decode_term` not found.

- [ ] **Step 3a: Add the defaulted trait methods**

In `crates/sparql/src/exec/mod.rs`, inside `pub trait Executor` (after `scan_bgp`, before `cardinality_estimate`), add:

```rust
    /// Scan a BGP returning id-carrying slot rows (no `TermId → String`
    /// decode). The default adapts the string [`scan_bgp`] for backends
    /// without a dictionary (e.g. `MemStore`, test doubles): the rows come
    /// back as `Slot::Term`. `HornBackend` overrides this to read the WCOJ
    /// id columns directly.
    fn scan_bgp_ids(&self, patterns: &[TriplePattern]) -> Result<crate::exec::Batch> {
        let rows: Vec<Bindings> = self.scan_bgp(patterns)?.collect();
        Ok(crate::exec::Batch::from_bindings(rows))
    }

    /// Decode a dictionary id to its term. Only meaningful for backends that
    /// produce `Slot::Id` (i.e. `HornBackend`); the default errors and is
    /// never reached for backends whose `scan_bgp_ids` yields only
    /// `Slot::Term`.
    fn decode_term(&self, id: horndb_storage::TermId) -> Result<Term> {
        Err(crate::error::SparqlError::Executor(format!(
            "backend has no dictionary to decode {id:?}"
        )))
    }
```

> Add `use horndb_storage::TermId;` is NOT needed here (the method spells the full path). Confirm `horndb_storage` is a normal dep of the crate (it is — `Cargo.toml:43`). `SparqlError` is already imported via `crate::error`.

- [ ] **Step 3b: Override both for `HornBackend`**

In `crates/sparql/src/exec/horn.rs`, inside `impl Executor for HornBackend` (after `scan_bgp`, alongside `cardinality_estimate`), add `decode_term`:

```rust
    fn decode_term(&self, id: TermId) -> Result<Term> {
        let ox = self
            .store
            .dictionary()
            .lookup(id)
            .ok_or_else(|| SparqlError::Executor(format!("dangling TermId {id:?}")))?;
        Ok(oxrdf_to_algebra(&ox))
    }
```

Then add `scan_bgp_ids` as a near-clone of `scan_bgp` (`horn.rs:480-634`) that builds a `Batch` instead of `Vec<Bindings>`. The structure is identical through the WCOJ loop; only the row materialization and the diagonal-filter/strip steps change. Implement it as:

```rust
    fn scan_bgp_ids(&self, patterns: &[TriplePattern]) -> Result<crate::exec::Batch> {
        use crate::algebra::Var;
        use crate::exec::{Batch, Row, Slot};

        // Empty BGP: one empty solution (join unit).
        if patterns.is_empty() {
            return Ok(Batch::unit());
        }

        let snapshot = self.wcoj_snapshot();
        let dict = self.store.dictionary();

        // Identical pattern-compilation pass to scan_bgp (var_index,
        // diagonal_filters, ground, wpatterns). COPY it verbatim from
        // scan_bgp lines 493-569, up to and including the two early returns
        // for ground-miss (empty Batch) and all-ground (unit Batch):
        let mut var_index: std::collections::HashMap<String, u8> = std::collections::HashMap::new();
        let mut diagonal_filters: Vec<(String, String)> = Vec::new();
        let mut wpatterns: Vec<WPattern> = Vec::new();
        let mut ground: Vec<WTriple> = Vec::new();
        for pattern in patterns {
            // ... EXACT copy of the per-pattern loop body from scan_bgp
            //     (horn.rs:504-558), unchanged ...
        }
        if ground.iter().any(|t| !snapshot.contains(t)) {
            return Ok(Batch::empty());
        }
        if wpatterns.is_empty() {
            return Ok(Batch::unit());
        }

        // Build the output schema: var_index keys in ascending WVar order,
        // minus the diagonal aliases (which are stripped from output, as in
        // scan_bgp). Column j of every Row corresponds to schema[j].
        let aliases: std::collections::HashSet<&str> =
            diagonal_filters.iter().map(|(_, a)| a.as_str()).collect();
        let mut ordered: Vec<(String, u8)> = var_index
            .iter()
            .filter(|(name, _)| !aliases.contains(name.as_str()))
            .map(|(n, i)| (n.clone(), *i))
            .collect();
        ordered.sort_by_key(|(_, i)| *i);
        let schema: Vec<Var> = ordered.iter().map(|(n, _)| Var::new(n.as_str())).collect();

        // For the diagonal-filter check we still need the alias columns'
        // ids per row, so collect ALL var columns (incl. aliases) keyed by
        // name into a per-row map, then project to schema + apply diagonals.
        let bgp = WBgp::new(wpatterns);
        let mut rows: Vec<Row> = Vec::new();
        for batch in WcojExecutor::for_bgp(
            snapshot.as_ref(),
            &bgp,
            &Planner::default(),
            CancelToken::new(),
        ) {
            let batch = batch.map_err(|e| SparqlError::Executor(format!("wcoj: {e}")))?;
            let arrow_schema = batch.schema();
            // name -> &UInt64Array, for every var incl. aliases (mirrors
            // scan_bgp's `cols`, but we keep ids and we need aliases too).
            let mut cols: Vec<(&str, &UInt64Array)> = Vec::with_capacity(var_index.len());
            for (name, idx) in &var_index {
                let Some((col_idx, _)) = arrow_schema.column_with_name(&format!("v{idx}")) else {
                    continue;
                };
                let arr = batch
                    .column(col_idx)
                    .as_any()
                    .downcast_ref::<UInt64Array>()
                    .ok_or_else(|| {
                        SparqlError::Executor(format!("wcoj batch column v{idx} is not UInt64"))
                    })?;
                cols.push((name.as_str(), arr));
            }
            // Quick name->position lookup into `cols`.
            let pos = |want: &str| cols.iter().position(|(n, _)| *n == want);
            for r in 0..batch.num_rows() {
                // Diagonal filter: drop the row unless each alias id equals
                // its original id (id equality == term identity).
                let keep = diagonal_filters.iter().all(|(orig, alias)| {
                    match (pos(orig), pos(alias)) {
                        (Some(io), Some(ia)) => cols[io].1.value(r) == cols[ia].1.value(r),
                        // Missing column ⇒ unbound both sides ⇒ trivially equal.
                        _ => true,
                    }
                });
                if !keep {
                    continue;
                }
                // Project to schema order, as Slot::Id (Unbound if no column).
                let slots = schema
                    .iter()
                    .map(|v| match pos(v.name()) {
                        Some(i) => Slot::Id(TermId(cols[i].1.value(r))),
                        None => Slot::Unbound,
                    })
                    .collect();
                rows.push(Row(slots));
            }
        }
        Ok(Batch { schema, rows })
    }
```

> The per-pattern loop body (the `for pattern in patterns { ... }`) is a byte-for-byte copy of `scan_bgp`'s `horn.rs:504-558`. Do not paraphrase it — copy it so the two stay in lockstep (a `// keep in sync with scan_bgp` comment on both is worth adding). The only behavioral differences from `scan_bgp` are: rows are `Slot::Id` not decoded `Bindings`, and the diagonal filter/strip is applied inline by id rather than post-hoc on decoded terms.

- [ ] **Step 4: Run the parity test**

Run: `cargo test -p horndb-sparql --test exec_horn scan_bgp_ids_decodes 2>&1 | tail -20`
Expected: PASS.

Also run the existing horn tests to confirm no break: `cargo nextest run -p horndb-sparql --test exec_horn`
Expected: all green.

- [ ] **Step 5: Commit**

```bash
git add crates/sparql/src/exec/mod.rs crates/sparql/src/exec/horn.rs crates/sparql/tests/exec_horn.rs
git commit -F - <<'EOF'
feat(sparql): Executor::scan_bgp_ids + decode_term (id scan seam, #128)

HornBackend reads the WCOJ UInt64Array columns straight into Slot::Id rows,
dropping the per-cell dict.lookup decode from the scan hot path; decode_term
backs the boundary decode. Defaults adapt the string scan for MemStore/test
doubles. Parity test: scan_bgp_ids decoded == scan_bgp.
EOF
```

---

## Task 3: Switch `Runtime::eval` to `Batch`, all operators on the adapter (BARRIER)

This is the all-or-nothing return-type switch. Every operator is routed through the `from_bindings`/`to_bindings` adapter wrapping today's unchanged code, so the suite stays green before any native port. **No behavior change** — `BgpScan` still uses the string `scan_bgp` here (native scan lands in Task 4).

**Files:**
- Modify: `crates/sparql/src/exec/runtime.rs`

- [ ] **Step 1: Add the `cfg(test)` legacy oracle first**

At the bottom of `runtime.rs`, copy today's `eval` body verbatim into a test-only method, so the differential test (Task 8) has a reference. Rename the copy's recursion to call itself:

```rust
#[cfg(test)]
impl<'a, E: Executor + ?Sized> Runtime<'a, E> {
    /// The pre-#128 string runtime, retained as a differential oracle for
    /// the slot port. Deleted when Slice 2 lands. Mirrors the `eval` that
    /// returned `Vec<Bindings>` (copy of git HEAD~ `eval`).
    pub(crate) fn eval_legacy(&self, plan: &PhysicalPlan) -> Result<Vec<Bindings>> {
        // EXACT copy of the eval body as it exists at the start of Task 3
        // (the Vec<Bindings> version), recursing via self.eval_legacy.
        // ... paste here ...
    }
}
```

> Paste the body from the current `eval` (`runtime.rs:26-141` at the start of this task) and replace each `self.eval(` with `self.eval_legacy(`. This compiles against the current free fns (`hash_left_join`, `eval_group`, `project`, etc.), which Task 3 leaves untouched.

- [ ] **Step 2: Change `run` to decode at the boundary**

Replace `run` (`runtime.rs:21-24`):

```rust
    pub fn run(&self, plan: &PhysicalPlan) -> Result<std::vec::IntoIter<Bindings>> {
        let batch = self.eval(plan)?;
        let rows = batch.to_bindings(|id| self.exec.decode_term(id))?;
        Ok(rows.into_iter())
    }
```

- [ ] **Step 3: Change `eval` to return `Batch`, every arm on the adapter**

Replace `eval`'s signature and body so each arm decodes its children, runs today's free fn, and re-encodes. Two small helpers first (add as private methods on `Runtime`):

```rust
    /// Decode a child plan to today's `Vec<Bindings>` (adapter input half).
    fn eval_rows(&self, plan: &PhysicalPlan) -> Result<Vec<Bindings>> {
        self.eval(plan)?.to_bindings(|id| self.exec.decode_term(id))
    }
```

```rust
    fn eval(&self, plan: &PhysicalPlan) -> Result<Batch> {
        match plan {
            // Native scan lands in Task 4; here, adapt the string scan.
            PhysicalPlan::BgpScan { patterns } => {
                Ok(Batch::from_bindings(self.exec.scan_bgp(patterns)?.collect()))
            }
            PhysicalPlan::Join { left, right } => {
                let l = self.eval_rows(left)?;
                let r = self.eval_rows(right)?;
                let mut out = Vec::new();
                for a in &l {
                    for b in &r {
                        if let Some(m) = a.extend_compat(b) {
                            out.push(m);
                        }
                    }
                }
                Ok(Batch::from_bindings(out))
            }
            PhysicalPlan::LeftJoin { left, right, expr } => {
                let l = self.eval_rows(left)?;
                let r = self.eval_rows(right)?;
                Ok(Batch::from_bindings(hash_left_join(l, r, expr.as_ref())?))
            }
            PhysicalPlan::Filter { expr, inner } => {
                let v = self.eval_rows(inner)?;
                let kept = v
                    .into_iter()
                    .map(|b| eval_expr(expr, &b).map(|keep| (b, keep)))
                    .collect::<Result<Vec<_>>>()?
                    .into_iter()
                    .filter(|(_, k)| *k)
                    .map(|(b, _)| b)
                    .collect();
                Ok(Batch::from_bindings(kept))
            }
            PhysicalPlan::Union { left, right } => {
                let mut a = self.eval_rows(left)?;
                a.extend(self.eval_rows(right)?);
                Ok(Batch::from_bindings(a))
            }
            PhysicalPlan::Project { vars, inner } => {
                let v = self.eval_rows(inner)?;
                Ok(Batch::from_bindings(v.into_iter().map(|b| project(&b, vars)).collect()))
            }
            PhysicalPlan::Distinct { inner } => {
                let v = self.eval_rows(inner)?;
                let mut seen: std::collections::HashSet<Bindings> = std::collections::HashSet::with_capacity(v.len());
                let mut out: Vec<Bindings> = Vec::with_capacity(v.len());
                for b in v {
                    if seen.insert(b.clone()) {
                        out.push(b);
                    }
                }
                Ok(Batch::from_bindings(out))
            }
            PhysicalPlan::Slice { inner, start, length } => {
                let v = self.eval_rows(inner)?;
                let s = *start;
                let take = length.unwrap_or(v.len().saturating_sub(s));
                Ok(Batch::from_bindings(v.into_iter().skip(s).take(take).collect()))
            }
            PhysicalPlan::OrderBy { inner, keys } => {
                let mut v = self.eval_rows(inner)?;
                v.sort_by(|a, b| compare_by_keys(a, b, keys));
                Ok(Batch::from_bindings(v))
            }
            PhysicalPlan::Extend { inner, var, expr } => {
                let v = self.eval_rows(inner)?;
                let mut out = Vec::with_capacity(v.len());
                for mut b in v {
                    if let Some(t) = eval_expr_to_term(expr, &b)? {
                        b.set(var.name().to_owned(), t);
                    }
                    out.push(b);
                }
                Ok(Batch::from_bindings(out))
            }
            PhysicalPlan::Values { vars, rows } => {
                let mut out = Vec::with_capacity(rows.len());
                for row in rows {
                    let mut b = Bindings::new();
                    for (var, cell) in vars.iter().zip(row.iter()) {
                        if let Some(term) = cell {
                            b.set(var.name().to_owned(), term.clone());
                        }
                    }
                    out.push(b);
                }
                Ok(Batch::from_bindings(out))
            }
            PhysicalPlan::Group { inner, keys, aggregates } => {
                let v = self.eval_rows(inner)?;
                Ok(Batch::from_bindings(eval_group(v, keys, aggregates)?))
            }
            PhysicalPlan::PathClosure { subject, object, edge, reflexive } => {
                let edge_rows = self.eval_rows(edge)?;
                Ok(Batch::from_bindings(eval_path_closure(subject, object, &edge_rows, *reflexive)?))
            }
        }
    }
```

> Add `use crate::exec::{Batch, Row, Slot, KeyPart};` to the imports at the top of `runtime.rs` (Row/Slot/KeyPart are used by Tasks 4–7; add them now to avoid churn). Keep all existing free fns (`hash_left_join`, `eval_group`, `project`, `compare_by_keys`, `eval_path_closure`, `eval_expr`, `eval_expr_to_term`, etc.) exactly as they are — the adapter calls them unchanged.

- [ ] **Step 4: Run the whole SPARQL suite — it must stay byte-identical green**

Run: `cargo nextest run -p horndb-sparql`
Then: `cargo nextest run -p horndb-sparql --features server`
Expected: all green (no test edits needed — this is a pure representation wrap).

Also confirm the workspace builds and clippy is clean (pre-push gate):
Run: `cargo clippy -p horndb-sparql --all-targets -- -D warnings`
Expected: no warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/sparql/src/exec/runtime.rs
git commit -F - <<'EOF'
refactor(sparql): Runtime::eval returns Batch (all-adapter, no behavior change)

The hard barrier for #128: eval now produces slot Batches, every operator
routed through the from_bindings/to_bindings decode-adapter wrapping today's
unchanged string code. run() decodes once at the boundary, so api.rs,
serializers, and the HTTP server are untouched. eval_legacy retained under
cfg(test) as the differential oracle. Suite stays byte-identical green.
EOF
```

---

## Task 4: Native `BgpScan` + native `Slice`

After this, scans no longer decode, and `LIMIT`/`OFFSET` is slot-native.

**Files:**
- Modify: `crates/sparql/src/exec/runtime.rs` (the `BgpScan` and `Slice` arms)

- [ ] **Step 1: Replace the `BgpScan` arm**

```rust
            PhysicalPlan::BgpScan { patterns } => self.exec.scan_bgp_ids(patterns),
```

- [ ] **Step 2: Replace the `Slice` arm (operate on the Batch directly)**

```rust
            PhysicalPlan::Slice { inner, start, length } => {
                let mut b = self.eval(inner)?;
                let s = (*start).min(b.rows.len());
                let take = length.unwrap_or(b.rows.len() - s);
                b.rows = b.rows.into_iter().skip(s).take(take).collect();
                Ok(b)
            }
```

- [ ] **Step 3: Run the suite**

Run: `cargo nextest run -p horndb-sparql && cargo nextest run -p horndb-sparql --features server`
Expected: green. (`exec_select`, `exec_horn`, slice/`LIMIT` tests exercise both arms.)

- [ ] **Step 4: Commit**

```bash
git add crates/sparql/src/exec/runtime.rs
git commit -m 'perf(sparql): native id BgpScan + Slice (no decode on scan/limit) (#128)'
```

---

## Task 5: Native `Project` + `Distinct`

**Files:**
- Modify: `crates/sparql/src/exec/runtime.rs`

- [ ] **Step 1: Replace the `Project` arm**

```rust
            PhysicalPlan::Project { vars, inner } => {
                let b = self.eval(inner)?;
                if vars.is_empty() {
                    // SELECT * / ASK: keep everything (parity with project()).
                    return Ok(b);
                }
                // New schema = projected vars that exist in the input, in
                // projection order; remap each row's slots by index.
                let idx: Vec<Option<usize>> =
                    vars.iter().map(|v| b.col(v.name())).collect();
                let schema: Vec<Var> = vars
                    .iter()
                    .zip(&idx)
                    .filter(|(_, i)| i.is_some())
                    .map(|(v, _)| v.clone())
                    .collect();
                let rows = b
                    .rows
                    .iter()
                    .map(|r| {
                        Row(idx
                            .iter()
                            .filter_map(|i| i.map(|i| r.0[i].clone()))
                            .collect())
                    })
                    .collect();
                Ok(Batch { schema, rows })
            }
```

- [ ] **Step 2: Replace the `Distinct` arm (hash on `Vec<KeyPart>`)**

```rust
            PhysicalPlan::Distinct { inner } => {
                let b = self.eval(inner)?;
                let mut seen: std::collections::HashSet<Vec<KeyPart>> =
                    std::collections::HashSet::with_capacity(b.rows.len());
                let mut rows = Vec::with_capacity(b.rows.len());
                for r in b.rows {
                    let key: Vec<KeyPart> = r.0.iter().map(|s| s.key_part()).collect();
                    if seen.insert(key) {
                        rows.push(r);
                    }
                }
                Ok(Batch { schema: b.schema, rows })
            }
```

> `key_part` preserves the within-column-homogeneity contract: for a scanned Batch every column is `Id`, so the dedup key is a `Vec` of raw ids — no string decode. Order-preserving (first-seen) just like `runtime.rs:70-83`.

- [ ] **Step 3: Run the suite**

Run: `cargo nextest run -p horndb-sparql && cargo nextest run -p horndb-sparql --features server`
Expected: green (`exec_select` DISTINCT cases, projection tests, `results_json`/snapshot tests byte-identical).

- [ ] **Step 4: Commit**

```bash
git add crates/sparql/src/exec/runtime.rs
git commit -m 'perf(sparql): native id Project + Distinct (no decode) (#128)'
```

---

## Task 6: Native `Group` (the `COUNT(*)` win)

Group by id keys (no per-row decode); `COUNT(*) = members.len()`; value-aggregates decode only their referenced columns per member; decode each group key once for output; sort output by decoded key to reproduce today's `BTreeMap`-lexical order.

**Files:**
- Modify: `crates/sparql/src/exec/runtime.rs`

- [ ] **Step 1: Add a `referenced_vars` helper (used here and in Task 7)**

Add near the other free fns in `runtime.rs`:

```rust
/// Collect the variable names an expression reads, so a slot operator can
/// decode only those columns into a transient `Bindings`.
fn referenced_vars(e: &Expr, out: &mut std::collections::HashSet<String>) {
    match e {
        Expr::Term(Term::Var(v)) => { out.insert(v.name().to_owned()); }
        Expr::Term(_) => {}
        Expr::Bound(v) => { out.insert(v.name().to_owned()); }
        Expr::Eq(a, b) | Expr::Ne(a, b) | Expr::Lt(a, b) | Expr::Gt(a, b)
        | Expr::Le(a, b) | Expr::Ge(a, b) | Expr::And(a, b) | Expr::Or(a, b)
        | Expr::Add(a, b) | Expr::Sub(a, b) | Expr::Mul(a, b) | Expr::Div(a, b) => {
            referenced_vars(a, out);
            referenced_vars(b, out);
        }
        Expr::Not(a) | Expr::Neg(a) => referenced_vars(a, out),
        Expr::If(a, b, c) => {
            referenced_vars(a, out);
            referenced_vars(b, out);
            referenced_vars(c, out);
        }
        Expr::In(a, list) => {
            referenced_vars(a, out);
            for x in list { referenced_vars(x, out); }
        }
        Expr::Coalesce(args) | Expr::Func(_, args) => {
            for x in args { referenced_vars(x, out); }
        }
    }
}
```

> Cross-check every `Expr` variant against `crate::algebra::Expr` (the arms used by `eval_expr_to_term`, `runtime.rs:990-1043`). If a variant is missing here the compiler will force a match arm — add it. This must be exhaustive (no `_ =>`) so a future `Expr` variant can't silently skip a referenced var.

- [ ] **Step 2: Add a method to decode a row subset into a transient `Bindings`**

Add as a private method on `Runtime`:

```rust
    /// Decode just the named columns of a slot row into a `Bindings`, for
    /// reusing the string expression/aggregate evaluator verbatim.
    fn decode_subset(
        &self,
        row: &Row,
        schema: &[Var],
        want: &std::collections::HashSet<String>,
    ) -> Result<Bindings> {
        let mut b = Bindings::new();
        for (i, v) in schema.iter().enumerate() {
            if !want.contains(v.name()) {
                continue;
            }
            match &row.0[i] {
                Slot::Id(id) => b.set(v.name().to_owned(), self.exec.decode_term(*id)?),
                Slot::Term(t) => b.set(v.name().to_owned(), t.clone()),
                Slot::Unbound => {}
            }
        }
        Ok(b)
    }
```

- [ ] **Step 3: Replace the `Group` arm with a native grouper**

```rust
            PhysicalPlan::Group { inner, keys, aggregates } => {
                let b = self.eval(inner)?;
                self.eval_group_native(b, keys, aggregates)
            }
```

Add the method:

```rust
    fn eval_group_native(
        &self,
        b: Batch,
        keys: &[Var],
        aggregates: &[Aggregate],
    ) -> Result<Batch> {
        use std::collections::HashMap;

        // Key-column indices into the input schema.
        let key_idx: Vec<Option<usize>> = keys.iter().map(|k| b.col(k.name())).collect();

        // Group by Vec<KeyPart> (no per-row decode). Keep the first row's key
        // slots as the representative, plus all member rows for aggregates.
        struct Grp {
            key_slots: Vec<Slot>,   // aligned with `keys`
            members: Vec<Row>,
        }
        let mut groups: HashMap<Vec<KeyPart>, Grp> = HashMap::new();
        for r in b.rows {
            let gkey: Vec<KeyPart> = key_idx
                .iter()
                .map(|i| i.map(|i| r.0[i].key_part()).unwrap_or(KeyPart::Unbound))
                .collect();
            let entry = groups.entry(gkey).or_insert_with(|| Grp {
                key_slots: key_idx
                    .iter()
                    .map(|i| i.map(|i| r.0[i].clone()).unwrap_or(Slot::Unbound))
                    .collect(),
                members: Vec::new(),
            });
            entry.members.push(r);
        }

        // Implicit grouping with no input rows still yields one empty group
        // (SPARQL §11.2: COUNT(*) of nothing is one row with 0).
        if keys.is_empty() && groups.is_empty() {
            groups.insert(Vec::new(), Grp { key_slots: Vec::new(), members: Vec::new() });
        }

        // Output schema = keys ++ aggregate output vars.
        let mut schema: Vec<Var> = keys.to_vec();
        for agg in aggregates {
            schema.push(agg.out.clone());
        }

        // Which input columns each aggregate's inner expression references —
        // decode only those per member. Built once.
        let agg_vars: Vec<std::collections::HashSet<String>> = aggregates
            .iter()
            .map(|agg| {
                let mut s = std::collections::HashSet::new();
                for e in agg_inner_exprs(agg) {
                    referenced_vars(e, &mut s);
                }
                s
            })
            .collect();

        // Produce one output Row per group; also keep a sort key (decoded
        // lexical of the group key) to match today's BTreeMap order.
        let mut out: Vec<(Vec<Option<String>>, Row)> = Vec::with_capacity(groups.len());
        for grp in groups.into_values() {
            let mut slots: Vec<Slot> = grp.key_slots.clone();

            // Decode member subset once per (aggregate ∪ vars) is overkill;
            // decode per aggregate using its own referenced columns.
            for (agg, want) in aggregates.iter().zip(&agg_vars) {
                let members_decoded: Vec<Bindings> = if matches!(agg.func, AggFunc::CountStar) && !agg.distinct {
                    Vec::new() // COUNT(*) needs no values
                } else {
                    grp.members
                        .iter()
                        .map(|r| self.decode_subset(r, &b.schema, want))
                        .collect::<Result<Vec<_>>>()?
                };
                // For COUNT(*) the count is members.len(), not decoded len.
                let value = if matches!(agg.func, AggFunc::CountStar) && !agg.distinct {
                    Some(integer_literal(grp.members.len() as i64))
                } else {
                    eval_aggregate(agg, &members_decoded)?
                };
                if let Some(t) = value {
                    slots.push(Slot::Term(t));
                } else {
                    slots.push(Slot::Unbound);
                }
            }

            // Sort key: decoded lexical of each group key slot (matches the
            // pre-#128 BTreeMap<Vec<Option<String>>> ordering exactly).
            let sort_key: Vec<Option<String>> = grp
                .key_slots
                .iter()
                .map(|s| match s {
                    Slot::Unbound => Ok(None),
                    Slot::Id(id) => self.exec.decode_term(*id).map(|t| Some(lex(&t))),
                    Slot::Term(t) => Ok(Some(lex(t))),
                })
                .collect::<Result<Vec<_>>>()?;
            out.push((sort_key, Row(slots)));
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));

        Ok(Batch { schema, rows: out.into_iter().map(|(_, r)| r).collect() })
    }
```

Add the small helper that lists an aggregate's inner expressions (so `referenced_vars` can scan them). `COUNT(*)` has none:

```rust
/// The inner expression(s) an aggregate evaluates over its members.
fn agg_inner_exprs(agg: &Aggregate) -> Vec<&Expr> {
    match &agg.func {
        AggFunc::CountStar => Vec::new(),
        AggFunc::Count(e) | AggFunc::Sum(e) | AggFunc::Avg(e)
        | AggFunc::Min(e) | AggFunc::Max(e) | AggFunc::Sample(e) => vec![e],
        AggFunc::GroupConcat { expr, .. } => vec![expr],
    }
}
```

> Cross-check `AggFunc`'s variants against `crate::algebra::AggFunc` (used in `eval_aggregate`, `runtime.rs:725-781`). Keep `eval_aggregate`, `integer_literal`, `lex`, `eval_expr_to_term` unchanged — `eval_aggregate` already takes `&[Bindings]`, which is exactly what `members_decoded` is.

- [ ] **Step 4: Run the aggregate + snapshot suite**

Run: `cargo nextest run -p horndb-sparql exec_aggregate && cargo nextest run -p horndb-sparql --features server`
Expected: green, **byte-identical** (group output order preserved by the sort).

- [ ] **Step 5: Sanity-check the win with `agg_profile` (laptop smoke, not recorded)**

Run: `cargo run -p horndb-sparql --release --example agg_profile 100000 2>&1 | grep -E 'Q1|Q2'`
Expected: `Q1 COUNT(*)` and `Q2 GROUP BY` per/q drop sharply vs the pre-change ~133 ms / ~55 ms (the real numbers get recorded on hornbench in Task 9; this is just a directional check).

- [ ] **Step 6: Commit**

```bash
git add crates/sparql/src/exec/runtime.rs
git commit -F - <<'EOF'
perf(sparql): native id Group — COUNT(*) and GROUP BY without per-row decode (#128)

Group keys on raw-id KeyParts (no string decode per row); COUNT(*) is
members.len(); value-aggregates decode only their referenced columns per
member via the existing eval_aggregate. Group key decoded once per group and
output sorted to preserve the pre-#128 lexical row order (byte-identical).
EOF
```

---

## Task 7: Native `Filter` + `Join`

**Files:**
- Modify: `crates/sparql/src/exec/runtime.rs`

- [ ] **Step 1: Replace the `Filter` arm (decode only referenced columns)**

```rust
            PhysicalPlan::Filter { expr, inner } => {
                let b = self.eval(inner)?;
                let mut want = std::collections::HashSet::new();
                referenced_vars(expr, &mut want);
                let mut rows = Vec::with_capacity(b.rows.len());
                for r in b.rows {
                    let env = self.decode_subset(&r, &b.schema, &want)?;
                    if eval_expr(expr, &env)? {
                        rows.push(r);
                    }
                }
                Ok(Batch { schema: b.schema, rows })
            }
```

- [ ] **Step 2: Add a slot-row merge helper for `Join`**

Native `Join` mirrors today's nested loop (`runtime.rs:29-41`) but on slots. Add a method:

```rust
    /// Merge two slot rows if compatible (shared vars equal by the slot
    /// rule), producing the union row over `out_schema`. Returns None if any
    /// shared var disagrees. Mirrors `Bindings::extend_compat` on slots.
    fn merge_rows(
        &self,
        ls: &[Var],
        l: &Row,
        rs: &[Var],
        r: &Row,
        out_schema: &[Var],
    ) -> Result<Option<Row>> {
        let decode = |id| self.exec.decode_term(id);
        // Build name -> slot for quick lookup on both sides.
        let lget = |name: &str| ls.iter().position(|v| v.name() == name).map(|i| &l.0[i]);
        let rget = |name: &str| rs.iter().position(|v| v.name() == name).map(|i| &r.0[i]);
        let mut slots = Vec::with_capacity(out_schema.len());
        for v in out_schema {
            let chosen = match (lget(v.name()), rget(v.name())) {
                (Some(a), Some(b)) => {
                    // Shared var: must agree (Unbound on either side is a
                    // wildcard — take the bound one, matching extend_compat
                    // where a missing key never conflicts).
                    match (a, b) {
                        (Slot::Unbound, x) | (x, Slot::Unbound) => x.clone(),
                        _ => {
                            if Slot::eq(a, b, &decode)? {
                                a.clone()
                            } else {
                                return Ok(None);
                            }
                        }
                    }
                }
                (Some(a), None) => a.clone(),
                (None, Some(b)) => b.clone(),
                (None, None) => Slot::Unbound,
            };
            slots.push(chosen);
        }
        Ok(Some(Row(slots)))
    }
```

- [ ] **Step 3: Replace the `Join` arm**

```rust
            PhysicalPlan::Join { left, right } => {
                let l = self.eval(left)?;
                let r = self.eval(right)?;
                // Output schema = left schema ++ right-only vars.
                let mut out_schema = l.schema.clone();
                for v in &r.schema {
                    if !out_schema.iter().any(|x| x.name() == v.name()) {
                        out_schema.push(v.clone());
                    }
                }
                let mut rows = Vec::new();
                for a in &l.rows {
                    for b in &r.rows {
                        if let Some(m) =
                            self.merge_rows(&l.schema, a, &r.schema, b, &out_schema)?
                        {
                            rows.push(m);
                        }
                    }
                }
                Ok(Batch { schema: out_schema, rows })
            }
```

> This is the same O(|l|·|r|) nested loop as today's `Join` (Assumption 3) — a faithful slot port, not a new hash join. The id-based `Slot::eq` shortcut means the all-`Id` common case does the shared-var checks with no decode.

- [ ] **Step 4: Run the suite (filters, joins, full SPARQL)**

Run: `cargo nextest run -p horndb-sparql && cargo nextest run -p horndb-sparql --features server`
Expected: green (`exec_filter_ops`, `exec_horn`, `exec_select`, `exec_expressions`).

- [ ] **Step 5: Commit**

```bash
git add crates/sparql/src/exec/runtime.rs
git commit -m 'perf(sparql): native id Filter + Join (decode only referenced cols) (#128)'
```

---

## Task 8: Differential property test (slot path ≡ legacy oracle)

**Files:**
- Modify: `crates/sparql/Cargo.toml` (add `proptest` dev-dep)
- Modify: `crates/sparql/src/exec/runtime.rs` (add a `#[cfg(test)]` proptest module)

> The test lives **in-crate** (not in `tests/`) because it calls the `cfg(test)`-only `eval_legacy`, which integration tests cannot see.

- [ ] **Step 1: Add the dev-dependency**

In `crates/sparql/Cargo.toml`, under `[dev-dependencies]`, add:

```toml
proptest = { workspace = true }
```

> The workspace already pins `proptest = "1"` (`Cargo.toml:47`). Other crates reference it as `proptest = { workspace = true }` — match that. If `[dev-dependencies]` lacks a `workspace`-style entry yet, this is the first; place it alphabetically.

- [ ] **Step 2: Write the differential test**

Add to the bottom of `runtime.rs`:

```rust
#[cfg(test)]
mod slot_differential {
    use super::*;
    use crate::api::execute_query;          // adjust path if needed
    use crate::exec::mem::MemStore;
    use proptest::prelude::*;

    // Build a MemStore from generated (s,p,o) integer triples, run a fixed
    // family of aggregation queries through both the slot runtime (execute_
    // query, which uses eval) and the legacy oracle, and assert equal result
    // sets. MemStore exercises the Slot::Term path; an analogous HornBackend
    // run (below) exercises the Slot::Id path.
    fn rows_via_legacy(store: &MemStore, plan: &PhysicalPlan) -> Vec<Bindings> {
        Runtime::new(store).eval_legacy(plan).unwrap()
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]
        #[test]
        fn group_count_matches_legacy(
            // up to 40 triples over a small vocab so groups actually collide
            triples in proptest::collection::vec(
                (0u32..6, 0u32..3, 0u32..6), 0..40)
        ) {
            let mut store = MemStore::new();
            for (s, p, o) in &triples {
                store.insert_triple(
                    crate::algebra::Term::Iri(format!("http://ex/s{s}")),
                    crate::algebra::Term::Iri(format!("http://ex/p{p}")),
                    crate::algebra::Term::Iri(format!("http://ex/o{o}")),
                );
            }
            // A representative set: COUNT(*), GROUP BY + COUNT, DISTINCT.
            for q in [
                "SELECT (COUNT(*) AS ?c) WHERE { ?s ?p ?o }",
                "SELECT ?p (COUNT(?s) AS ?c) WHERE { ?s ?p ?o } GROUP BY ?p",
                "SELECT DISTINCT ?o WHERE { ?s ?p ?o }",
            ] {
                let plan = crate::plan::planner::plan(
                    &crate::algebra::translate::translate_query_with(
                        &match crate::parser::parse_query(q).unwrap() {
                            crate::parser::ParsedQuery::Select { inner } => inner,
                            _ => unreachable!(),
                        },
                        &crate::SparqlConfig::default(),
                    ).unwrap()
                ).unwrap();

                let mut got = Runtime::new(&store).run(&plan).unwrap().collect::<Vec<_>>();
                let mut want = rows_via_legacy(&store, &plan);
                let sort = |v: &mut Vec<Bindings>| v.sort_by_key(|b| {
                    let mut s: Vec<String> = b.vars().map(|(k, t)| format!("{k}={t:?}")).collect();
                    s.sort(); s.join("|")
                });
                sort(&mut got); sort(&mut want);
                prop_assert_eq!(got, want, "query: {}", q);
            }
        }
    }
}
```

> The exact import paths (`api::execute_query`, `parser::ParsedQuery`, `plan::planner::plan`, `algebra::translate::translate_query_with`) must match what `api.rs` uses — copy them from `api.rs:6-13` and `api.rs:48-52`. If wiring the plan inline is awkward, factor a small `pub(crate) fn plan_select(q: &str) -> PhysicalPlan` test helper next to the module. The MemStore path proves schema/slot bookkeeping; for the **id** path, add a second proptest body identical except it builds a `HornBackend` (its `eval_legacy` uses the string `scan_bgp`, the slot path uses `scan_bgp_ids`) — this is the differential that actually covers `Slot::Id`.

- [ ] **Step 3: Run it**

Run: `cargo nextest run -p horndb-sparql slot_differential`
Expected: PASS (128 cases across the two backends).

- [ ] **Step 4: Commit**

```bash
git add crates/sparql/Cargo.toml crates/sparql/src/exec/runtime.rs
git commit -F - <<'EOF'
test(sparql): differential proptest — slot runtime == legacy oracle (#128)

Random small graphs through COUNT(*) / GROUP BY+COUNT / DISTINCT, asserting
the slot runtime's decoded output equals the retained cfg(test) eval_legacy
string runtime, over both MemStore (Slot::Term) and HornBackend (Slot::Id).
EOF
```

---

## Task 9: Measure + docs-sync

**Files:**
- Modify: `BENCHMARKS.md`, `TASKS.md`, `docs/architecture.md`
- GitHub issue #128 (mirror)

- [ ] **Step 1: Full-workspace green + clippy (the acceptance gate)**

Run: `cargo nextest run -p horndb-sparql && cargo nextest run -p horndb-sparql --features server`
Run: `cargo clippy --workspace --all-targets -- -D warnings`
Run: `cargo build --workspace`
Expected: all green, no warnings. (Criterion 1.)

- [ ] **Step 2: Record `agg_profile` before/after**

Per CLAUDE.md, recorded numbers come from **hornbench**, but `agg_profile` is explicitly *not* a recorded bench, so a laptop before/after delta is acceptable for the plan's evidence; note the env. If a hornbench run is convenient, prefer it. Capture Q1–Q5 per/q + qps at `agg_profile 100000` against the pre-change baseline (Q1 ~133 ms, Q2 ~55 ms, Q3 ~81 ms, Q4 ~111 ms, Q5 ~37 ms).

- [ ] **Step 3: Update `BENCHMARKS.md`**

Add/refresh the SPB aggregation row with the new `agg_profile` deltas and a note that the SPB **nightly** `aggregation-qps` will be recorded on the next nightly (or trigger `gh workflow run nightly.yml`). State explicitly that the 12× gap is **not** fully closed here — streaming/pushdown (deferred under #128) own the remainder. (Criteria 4–5.)

- [ ] **Step 4: Flip `docs/architecture.md` §9 rows**

- The aggregation row (`architecture.md:279`, "implemented (correct, slow)"): update the **Perf** note to say `eval_group` now keys on id `KeyPart`s and `COUNT(*)`/`GROUP BY` no longer decode per row (Slice 1 of #128 landed); the remaining gap is streaming/pushdown.
- The planner+runtime row (`architecture.md:283`): update the **Perf limitation** note — `scan_bgp_ids` now feeds the runtime id-carrying slot rows and the boundary decodes once; the string `scan_bgp` remains only for DESCRIBE; the six adapter-backed operators and streaming/pushdown remain (Slice 2 / #128).

- [ ] **Step 5: Check off the #128 sub-scope in `TASKS.md` + mirror the issue**

`TASKS.md:48` is the #128 task. Do **not** close it (Slice 2 + streaming/pushdown remain). Add a sub-bullet noting Slice 1 (id-based slot bindings) landed with the commit/PR ref, and re-scope the task body to the remaining work (native ports of the six adapter operators; streaming; planner pushdown). Mirror the same note to GitHub issue #128 per the `TASKS.md` header procedure (do not auto-close).

- [ ] **Step 6: Commit the docs-sync**

```bash
git add BENCHMARKS.md TASKS.md docs/architecture.md
git commit -F - <<'EOF'
docs: record id-based slot rows Slice 1 (#128)

Slice 1 (id-carrying slot rows: native scan_bgp_ids + slot Join/Group/
Distinct/Project/Filter, boundary decode) landed. BENCHMARKS agg_profile
deltas; architecture §9 aggregation + runtime rows reflect id-based bindings;
#128 re-scoped to the remaining adapter-operator ports, streaming, and
planner pushdown (not closed).
EOF
```

---

## Self-review notes (author)

- **Spec coverage:** types (T1), Resolver/decode seam (T2), `scan_bgp_ids` (T2), `eval`→`Batch` boundary (T3), native Join/Group/Distinct/Project/Filter + Slice (T4–T7), adapter for the other six (T3 leaves them adapter-backed — correct), decode at boundary (T3 `run`), slot-comparison unit test (T1), differential proptest with `eval_legacy` oracle (T8), `agg_profile` + nightly + `BENCHMARKS.md` + docs-sync (T9). All acceptance criteria mapped.
- **No placeholders:** every code step shows complete code or an exact copy-anchor (`horn.rs:504-558`, the `eval` body) with the precise transformation.
- **Type consistency:** `Slot`/`Row`/`Batch`/`KeyPart` names, `scan_bgp_ids`/`decode_term`/`to_bindings`/`from_bindings`/`key_part`/`referenced_vars`/`decode_subset`/`merge_rows`/`eval_group_native`/`agg_inner_exprs`/`eval_legacy` are used identically across tasks. `lex` made `pub(crate)` in T1.
- **Open risk to watch during execution:** the two cross-check notes (exhaustive `Expr` match in `referenced_vars`, `AggFunc` arms in `agg_inner_exprs`) — the compiler enforces both, so a missed variant fails the build, not silently.
