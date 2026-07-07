//! Source operators: leaves with no child input.

use super::{ChunkedBatch, Op};
use crate::algebra::{Term, TriplePattern, Var};
use crate::error::Result;
use crate::exec::runtime::{integer_literal, lex};
use crate::exec::{Batch, Executor, GroupCount, KeyPart, Row, Slot};
use std::collections::HashMap;

/// Scans a BGP once via the executor, then hands the rows out in chunks.
/// The scan seam is unchanged (`scan_bgp_ids` returns a whole `Batch`); this
/// op only re-chunks it so parents pull incrementally.
pub struct ScanOp {
    inner: ChunkedBatch,
    /// Per-column `may_emit_term` claim, computed from the materialized scan
    /// batch before it is wrapped for chunked iteration.
    term_columns: Vec<bool>,
}

impl ScanOp {
    pub fn new(batch: Batch) -> Self {
        // Compute the per-column provenance claim from the actual rows:
        // column c may emit Term iff some row holds a Slot::Term there.
        let mut term_columns = vec![false; batch.schema.len()];
        for row in &batch.rows {
            for (c, slot) in row.0.iter().enumerate() {
                if matches!(slot, Slot::Term(_)) {
                    term_columns[c] = true;
                }
            }
        }
        Self {
            inner: ChunkedBatch::new(batch),
            term_columns,
        }
    }
}

impl Op for ScanOp {
    fn schema(&self) -> &[Var] {
        self.inner.schema()
    }

    fn may_emit_term(&self) -> Vec<bool> {
        // Computed from the materialized scan batch: exact for any Executor —
        // dictionary-backed scans are all Slot::Id (all-false), adapter
        // backends (default `scan_bgp_ids` via `Batch::from_bindings`) are
        // all Slot::Term.
        self.term_columns.clone()
    }

    fn next(&mut self) -> Result<Option<Batch>> {
        Ok(self.inner.next_chunk())
    }
}

/// Pushed-down `COUNT(*)` / `COUNT(?v)` over a BGP (#144). Computes the
/// solution count via the executor's `count_bgp` fast path (falling back to
/// `scan_bgp_ids().rows.len()` when the backend has no fast count) and emits a
/// single row binding `out_var` to that count as an `xsd:integer` literal —
/// byte-identical to what the streaming `Group` would produce.
pub struct CountScanOp {
    schema: Vec<Var>,
    batch: Option<Batch>,
}

impl CountScanOp {
    pub fn new<E: Executor + ?Sized>(
        exec: &E,
        patterns: &[TriplePattern],
        out_var: &Var,
    ) -> Result<Self> {
        let n = match exec.count_bgp(patterns)? {
            Some(n) => n,
            // Correctness fallback: count the id-rows the scan would yield.
            None => exec.scan_bgp_ids(patterns)?.rows.len(),
        };
        let lit = crate::exec::runtime::integer_literal(i64::try_from(n).unwrap_or(i64::MAX));
        let batch = Batch {
            schema: vec![out_var.clone()],
            rows: vec![Row(vec![Slot::Term(lit)])],
        };
        Ok(Self {
            schema: vec![out_var.clone()],
            batch: Some(batch),
        })
    }
}

impl Op for CountScanOp {
    fn schema(&self) -> &[Var] {
        &self.schema
    }

    fn may_emit_term(&self) -> Vec<bool> {
        // The count is a computed xsd:integer literal (Slot::Term).
        vec![true; self.schema.len()]
    }

    fn next(&mut self) -> Result<Option<Batch>> {
        Ok(self.batch.take())
    }
}

/// Pushed-down grouped / multi-output `COUNT` over a BGP (#128). One row per
/// group: the key slots, then one `xsd:integer` count per output var (every
/// replaced aggregate is a plain non-DISTINCT count, so all outputs carry the
/// group size). Rows are sorted by the decoded lexical form of the key slots
/// — the same deterministic order `eval_group_native` produces, which is
/// observable under a parent `Slice` (LIMIT).
pub struct GroupCountScanOp {
    schema: Vec<Var>,
    inner: ChunkedBatch,
}

impl GroupCountScanOp {
    pub fn new<E: Executor + ?Sized>(
        exec: &E,
        patterns: &[TriplePattern],
        keys: &[Var],
        out_vars: &[Var],
    ) -> Result<Self> {
        let mut schema: Vec<Var> = keys.to_vec();
        schema.extend(out_vars.iter().cloned());

        // Implicit grouping (no keys): exactly one row, even over zero
        // solutions (SPARQL §11.2 — COUNT of nothing is 0), answered by the
        // existing count_bgp seam (with its scan+len correctness fallback).
        if keys.is_empty() {
            let n = match exec.count_bgp(patterns)? {
                Some(n) => n,
                None => exec.scan_bgp_ids(patterns)?.rows.len(),
            };
            let lit = integer_literal(i64::try_from(n).unwrap_or(i64::MAX));
            let rows = vec![Row(out_vars
                .iter()
                .map(|_| Slot::Term(lit.clone()))
                .collect())];
            let batch = Batch {
                schema: schema.clone(),
                rows,
            };
            return Ok(Self {
                schema,
                inner: ChunkedBatch::new(batch),
            });
        }

        // Per-key counts: fast seam when the backend has one, else scan the
        // id-rows once and hash-count on the key columns only.
        let groups = match exec.count_bgp_grouped(patterns, keys)? {
            Some(groups) => groups,
            None => fallback_group_counts(exec, patterns, keys)?,
        };

        // Sort by decoded-lexical key — byte-identical ordering to
        // eval_group_native's sort_key (None sorts before Some, matching the
        // Unbound-first convention there).
        let mut tagged: Vec<(Vec<Option<String>>, Row)> = Vec::with_capacity(groups.len());
        for (key_slots, n) in groups {
            let sort_key: Vec<Option<String>> = key_slots
                .iter()
                .map(|s| match s {
                    Slot::Unbound => Ok(None),
                    Slot::Id(id) => exec.decode_term(*id).map(|t| Some(lex(&t))),
                    Slot::Term(t) => Ok(Some(lex(t))),
                })
                .collect::<Result<Vec<_>>>()?;
            let lit = integer_literal(i64::try_from(n).unwrap_or(i64::MAX));
            let mut slots = key_slots;
            slots.extend(out_vars.iter().map(|_| Slot::Term(lit.clone())));
            tagged.push((sort_key, Row(slots)));
        }
        tagged.sort_by(|a, b| a.0.cmp(&b.0));
        let batch = Batch {
            schema: schema.clone(),
            rows: tagged.into_iter().map(|(_, r)| r).collect(),
        };
        Ok(Self {
            schema,
            inner: ChunkedBatch::new(batch),
        })
    }
}

impl Op for GroupCountScanOp {
    fn schema(&self) -> &[Var] {
        &self.schema
    }

    fn may_emit_term(&self) -> Vec<bool> {
        // Count columns are computed xsd:integer literals (Slot::Term); a
        // group-key column may bind a literal or come back as Slot::Term from
        // a backend/fallback, so the whole row is a Term over-approximation.
        vec![true; self.schema.len()]
    }

    fn next(&mut self) -> Result<Option<Batch>> {
        Ok(self.inner.next_chunk())
    }
}

/// Correctness fallback when the backend has no fast grouped count: scan the
/// id-rows once and hash-count on the key columns, never decoding non-key
/// columns. Grouping semantics are identical to `eval_group_native`:
/// `KeyPart` per key slot, `Unbound` for a key column the scan does not
/// produce, first-seen key slots kept per group.
fn fallback_group_counts<E: Executor + ?Sized>(
    exec: &E,
    patterns: &[TriplePattern],
    keys: &[Var],
) -> Result<Vec<GroupCount>> {
    let batch = exec.scan_bgp_ids(patterns)?;
    let key_idx: Vec<Option<usize>> = keys.iter().map(|k| batch.col(k.name())).collect();
    let mut groups: HashMap<Vec<KeyPart>, (Vec<Slot>, usize)> = HashMap::new();
    for r in &batch.rows {
        let gkey: Vec<KeyPart> = key_idx
            .iter()
            .map(|i| i.map(|i| r.0[i].key_part()).unwrap_or(KeyPart::Unbound))
            .collect();
        let entry = groups.entry(gkey).or_insert_with(|| {
            (
                key_idx
                    .iter()
                    .map(|i| i.map(|i| r.0[i].clone()).unwrap_or(Slot::Unbound))
                    .collect(),
                0,
            )
        });
        entry.1 += 1;
    }
    Ok(groups.into_values().collect())
}

/// Materialize VALUES rows into a `Batch` (`Slot::Term`/`Slot::Unbound` cells).
/// Used by `ValuesOp`.
pub(super) fn build_values_batch(vars: &[Var], rows: &[Vec<Option<Term>>]) -> Batch {
    // Rows are guaranteed full-width by the spargebra parser (it rejects
    // `VALUES` clauses where any row length != vars.len()), so `zip` stops
    // correctly and no trailing-Unbound padding is needed.
    let schema: Vec<Var> = vars.to_vec();
    let out_rows = rows
        .iter()
        .map(|row| {
            Row(vars
                .iter()
                .zip(row.iter())
                .map(|(_, cell)| match cell {
                    Some(t) => Slot::Term(t.clone()),
                    None => Slot::Unbound,
                })
                .collect())
        })
        .collect();
    Batch {
        schema,
        rows: out_rows,
    }
}

/// VALUES row source: materializes the literal rows once, then chunks them.
pub struct ValuesOp {
    inner: ChunkedBatch,
}

impl ValuesOp {
    pub fn new(vars: &[Var], rows: &[Vec<Option<Term>>]) -> Self {
        Self {
            inner: ChunkedBatch::new(build_values_batch(vars, rows)),
        }
    }
}

impl Op for ValuesOp {
    fn schema(&self) -> &[Var] {
        self.inner.schema()
    }

    fn may_emit_term(&self) -> Vec<bool> {
        // VALUES cells are Slot::Term (or Slot::Unbound for UNDEF).
        vec![true; self.inner.schema().len()]
    }

    fn next(&mut self) -> Result<Option<Batch>> {
        Ok(self.inner.next_chunk())
    }
}

#[cfg(test)]
mod tests {
    use crate::algebra::{Term, TriplePattern, Var};
    use crate::exec::horn::HornBackend;
    use crate::exec::runtime::Runtime;
    use crate::exec::Store;
    use crate::plan::PhysicalPlan;

    #[test]
    fn scan_emits_all_rows_across_chunks() {
        let mut horn = HornBackend::new();
        let iri = |s: &str| Term::Iri(format!("http://ex/{s}"));
        for i in 0..10 {
            horn.insert_triple(iri(&format!("e{i}")), iri("p"), iri("o"));
        }
        let plan = PhysicalPlan::BgpScan {
            patterns: vec![TriplePattern {
                subject: Term::Var(Var::new("s")),
                predicate: iri("p"),
                object: Term::Var(Var::new("o")),
            }],
        };
        let rt = Runtime::new(&horn);
        let mut op = rt.build(&plan).unwrap();
        let mut total = 0;
        while let Some(b) = op.next().unwrap() {
            assert!(!b.rows.is_empty(), "no empty chunks mid-stream");
            total += b.rows.len();
        }
        assert_eq!(total, 10);
    }
}
