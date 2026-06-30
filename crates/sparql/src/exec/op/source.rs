//! Source operators: leaves with no child input.

use super::{ChunkedBatch, Op};
use crate::algebra::{Term, TriplePattern, Var};
use crate::error::Result;
use crate::exec::{Batch, Executor, Row, Slot};

/// Scans a BGP once via the executor, then hands the rows out in chunks.
/// The scan seam is unchanged (`scan_bgp_ids` returns a whole `Batch`); this
/// op only re-chunks it so parents pull incrementally.
pub struct ScanOp {
    inner: ChunkedBatch,
}

impl ScanOp {
    pub fn new(batch: Batch) -> Self {
        Self {
            inner: ChunkedBatch::new(batch),
        }
    }
}

impl Op for ScanOp {
    fn schema(&self) -> &[Var] {
        self.inner.schema()
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

    fn next(&mut self) -> Result<Option<Batch>> {
        Ok(self.batch.take())
    }
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
