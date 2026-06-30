//! Source operators: leaves with no child input.

use super::{Op, BATCH_ROWS};
use crate::algebra::Var;
use crate::error::Result;
use crate::exec::{Batch, Row};

/// Scans a BGP once via the executor, then hands the rows out in chunks.
/// The scan seam is unchanged (`scan_bgp_ids` returns a whole `Batch`); this
/// op only re-chunks it so parents pull incrementally.
pub struct ScanOp {
    schema: Vec<Var>,
    rows: std::vec::IntoIter<Row>,
}

impl ScanOp {
    pub fn new(batch: Batch) -> Self {
        Self {
            schema: batch.schema,
            rows: batch.rows.into_iter(),
        }
    }
}

impl Op for ScanOp {
    fn schema(&self) -> &[Var] {
        &self.schema
    }

    fn next(&mut self) -> Result<Option<Batch>> {
        let chunk: Vec<Row> = self.rows.by_ref().take(BATCH_ROWS).collect();
        if chunk.is_empty() {
            Ok(None)
        } else {
            Ok(Some(Batch {
                schema: self.schema.clone(),
                rows: chunk,
            }))
        }
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
