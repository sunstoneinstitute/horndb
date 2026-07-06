//! Streaming operators: one child, per-chunk transform, no buffering.

use super::Op;
use crate::algebra::{Expr, Var};
use crate::error::Result;
use crate::exec::runtime::Runtime;
use crate::exec::{Batch, Executor, KeyPart};
use std::collections::HashSet;

/// Streams its child, evaluating `expr` and binding the result to `var` (BIND).
/// Never drops rows — an unbound expr result leaves the slot as `Slot::Unbound`.
pub struct ExtendOp<'r, E: Executor + ?Sized> {
    rt: &'r Runtime<'r, E>,
    child: Box<dyn Op + 'r>,
    var: Var,
    expr: Expr,
    schema: Vec<Var>,
}

impl<'r, E: Executor + ?Sized> ExtendOp<'r, E> {
    pub fn new(rt: &'r Runtime<'r, E>, child: Box<dyn Op + 'r>, var: Var, expr: Expr) -> Self {
        // Mirror apply_extend's output schema rule exactly: append var iff
        // absent from child schema (re-BIND overwrites in place, no schema change).
        let mut schema = child.schema().to_vec();
        if !schema.iter().any(|v| v.name() == var.name()) {
            schema.push(var.clone());
        }
        Self {
            rt,
            child,
            var,
            expr,
            schema,
        }
    }
}

impl<'r, E: Executor + ?Sized> Op for ExtendOp<'r, E> {
    fn schema(&self) -> &[Var] {
        &self.schema
    }
    fn may_emit_term(&self) -> Vec<bool> {
        // Child columns keep their provenance; the BIND output column is a
        // computed Slot::Term (or Unbound). Covers both the appended-column
        // case (index past the child schema) and the re-BIND overwrite case.
        let child = self.child.may_emit_term();
        self.schema
            .iter()
            .enumerate()
            .map(|(i, v)| v.name() == self.var.name() || child.get(i).copied().unwrap_or(true))
            .collect()
    }
    fn next(&mut self) -> Result<Option<Batch>> {
        match self.child.next()? {
            Some(chunk) => Ok(Some(self.rt.apply_extend(chunk, &self.var, &self.expr)?)),
            None => Ok(None),
        }
    }
}

/// Streams its child, projecting each chunk to `vars`.
pub struct ProjectOp<'r, E: Executor + ?Sized> {
    rt: &'r Runtime<'r, E>,
    child: Box<dyn Op + 'r>,
    vars: Vec<Var>,
    schema: Vec<Var>,
}

impl<'r, E: Executor + ?Sized> ProjectOp<'r, E> {
    pub fn new(rt: &'r Runtime<'r, E>, child: Box<dyn Op + 'r>, vars: Vec<Var>) -> Self {
        // Mirror `apply_project`'s output schema exactly: empty `vars` is a
        // passthrough; otherwise keep only projected vars present in the child
        // schema (absent vars are dropped from both schema and rows). This
        // keeps `schema()` consistent with every Batch `next` yields, honoring
        // the Op contract even for an ill-scoped Project.
        let schema = if vars.is_empty() {
            child.schema().to_vec()
        } else {
            let child_schema = child.schema();
            vars.iter()
                .filter(|v| child_schema.iter().any(|cv| cv.name() == v.name()))
                .cloned()
                .collect()
        };
        Self {
            rt,
            child,
            vars,
            schema,
        }
    }
}

impl<'r, E: Executor + ?Sized> Op for ProjectOp<'r, E> {
    fn schema(&self) -> &[Var] {
        &self.schema
    }
    fn may_emit_term(&self) -> Vec<bool> {
        // Remap the child's claims into projection order.
        let child_terms = self.child.may_emit_term();
        let child_schema = self.child.schema();
        self.schema
            .iter()
            .map(|v| {
                child_schema
                    .iter()
                    .position(|c| c.name() == v.name())
                    .map(|i| child_terms[i])
                    .unwrap_or(false)
            })
            .collect()
    }
    fn next(&mut self) -> Result<Option<Batch>> {
        match self.child.next()? {
            Some(chunk) => Ok(Some(self.rt.apply_project(chunk, &self.vars)?)),
            None => Ok(None),
        }
    }
}

/// OFFSET/LIMIT. `to_skip` rows are dropped first, then up to `remaining` are
/// emitted; state persists across chunks so a window can span chunk boundaries.
pub struct SliceOp<'r> {
    child: Box<dyn Op + 'r>,
    to_skip: usize,
    remaining: Option<usize>, // None = unbounded (no LIMIT)
    schema: Vec<Var>,
}

impl<'r> SliceOp<'r> {
    pub fn new(child: Box<dyn Op + 'r>, start: usize, length: Option<usize>) -> Self {
        let schema = child.schema().to_vec();
        Self {
            child,
            to_skip: start,
            remaining: length,
            schema,
        }
    }
}

impl<'r> Op for SliceOp<'r> {
    fn schema(&self) -> &[Var] {
        &self.schema
    }
    fn may_emit_term(&self) -> Vec<bool> {
        self.child.may_emit_term()
    }
    fn next(&mut self) -> Result<Option<Batch>> {
        if self.remaining == Some(0) {
            return Ok(None);
        }
        while let Some(mut chunk) = self.child.next()? {
            // Drop offset rows still owed.
            if self.to_skip > 0 {
                let drop = self.to_skip.min(chunk.rows.len());
                chunk.rows.drain(0..drop);
                self.to_skip -= drop;
            }
            // Cap to remaining LIMIT.
            if let Some(rem) = self.remaining {
                if chunk.rows.len() > rem {
                    chunk.rows.truncate(rem);
                }
                self.remaining = Some(rem - chunk.rows.len());
            }
            // If the limit was just hit, the chunk is non-empty (it carried the
            // last `rem` rows), so we return here and the top-level guard ends
            // the stream on the next call. A chunk empty at this point was
            // wholly consumed by the offset drain (limit untouched), so we fall
            // through and pull the next chunk.
            if !chunk.rows.is_empty() {
                return Ok(Some(chunk));
            }
        }
        Ok(None)
    }
}

/// Streams its child, keeping rows that satisfy `expr`. Loops internally so it
/// never yields an empty chunk (a chunk fully filtered out pulls the next one).
pub struct FilterOp<'r, E: Executor + ?Sized> {
    rt: &'r Runtime<'r, E>,
    child: Box<dyn Op + 'r>,
    expr: Expr,
    schema: Vec<Var>,
}

impl<'r, E: Executor + ?Sized> FilterOp<'r, E> {
    pub fn new(rt: &'r Runtime<'r, E>, child: Box<dyn Op + 'r>, expr: Expr) -> Self {
        let schema = child.schema().to_vec();
        Self {
            rt,
            child,
            expr,
            schema,
        }
    }
}

impl<'r, E: Executor + ?Sized> Op for FilterOp<'r, E> {
    fn schema(&self) -> &[Var] {
        &self.schema
    }
    fn may_emit_term(&self) -> Vec<bool> {
        self.child.may_emit_term()
    }
    fn next(&mut self) -> Result<Option<Batch>> {
        while let Some(chunk) = self.child.next()? {
            let kept = self.rt.apply_filter(chunk, &self.expr)?;
            if !kept.rows.is_empty() {
                return Ok(Some(kept));
            }
        }
        Ok(None)
    }
}

/// Deduplicates rows by their `KeyPart` vector. The seen-set persists across
/// chunks, so only first-seen rows are emitted; loops internally to skip a
/// chunk that turns out to be all duplicates (never yields an empty chunk).
pub struct DistinctOp<'r> {
    child: Box<dyn Op + 'r>,
    seen: HashSet<Vec<KeyPart>>,
    schema: Vec<Var>,
}

impl<'r> DistinctOp<'r> {
    pub fn new(child: Box<dyn Op + 'r>) -> Self {
        let schema = child.schema().to_vec();
        Self {
            child,
            seen: HashSet::new(),
            schema,
        }
    }
}

impl<'r> Op for DistinctOp<'r> {
    fn schema(&self) -> &[Var] {
        &self.schema
    }
    fn may_emit_term(&self) -> Vec<bool> {
        self.child.may_emit_term()
    }
    fn next(&mut self) -> Result<Option<Batch>> {
        while let Some(chunk) = self.child.next()? {
            let mut kept = Vec::new();
            for row in chunk.rows {
                let key: Vec<KeyPart> = row.0.iter().map(|s| s.key_part()).collect();
                if self.seen.insert(key) {
                    kept.push(row);
                }
            }
            if !kept.is_empty() {
                return Ok(Some(Batch {
                    schema: self.schema.clone(),
                    rows: kept,
                }));
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use crate::algebra::{Term, TriplePattern, Var};
    use crate::exec::horn::HornBackend;
    use crate::exec::runtime::Runtime;
    use crate::exec::Store;
    use crate::plan::PhysicalPlan;

    /// Build a 10-row store, plan `Slice` over a BGP scan, and return the total
    /// rows emitted across all chunks (asserting no empty chunk mid-stream).
    /// Row identities are not checked — scan order is non-deterministic.
    fn slice_count(start: usize, length: Option<usize>) -> usize {
        let mut horn = HornBackend::new();
        let iri = |s: &str| Term::Iri(format!("http://ex/{s}"));
        for i in 0..10 {
            horn.insert_triple(iri(&format!("e{i}")), iri("p"), iri("o"));
        }
        let scan = PhysicalPlan::BgpScan {
            patterns: vec![TriplePattern {
                subject: Term::Var(Var::new("s")),
                predicate: iri("p"),
                object: Term::Var(Var::new("o")),
            }],
        };
        let plan = PhysicalPlan::Slice {
            inner: Box::new(scan),
            start,
            length,
        };
        let rt = Runtime::new(&horn);
        let mut op = rt.build(&plan).unwrap();
        let mut total = 0;
        while let Some(b) = op.next().unwrap() {
            assert!(!b.rows.is_empty(), "no empty chunks mid-stream");
            total += b.rows.len();
        }
        total
    }

    #[test]
    fn slice_offset_and_limit() {
        // OFFSET 2 LIMIT 5 over 10 rows -> 5.
        assert_eq!(slice_count(2, Some(5)), 5);
    }

    #[test]
    fn slice_offset_only_unbounded_limit() {
        // OFFSET 3, no LIMIT -> 7 remaining.
        assert_eq!(slice_count(3, None), 7);
    }

    #[test]
    fn slice_limit_only() {
        // LIMIT 4, no OFFSET -> 4.
        assert_eq!(slice_count(0, Some(4)), 4);
    }

    #[test]
    fn slice_zero_length_emits_nothing() {
        // LIMIT 0 -> 0 rows.
        assert_eq!(slice_count(0, Some(0)), 0);
    }

    #[test]
    fn slice_offset_past_end_emits_nothing() {
        // OFFSET 25 > 10 rows -> 0 rows.
        assert_eq!(slice_count(25, Some(5)), 0);
        assert_eq!(slice_count(25, None), 0);
    }

    /// Insert (e0,p,X),(e1,p,X),(e2,p,Y).  Distinct over Project(?o, BgpScan)
    /// must emit exactly 2 rows ({X, Y}) and no empty chunk mid-stream. This
    /// pins the dedup *count*; the cross-chunk seen-set (dedup spanning chunk
    /// boundaries) is exercised by the Task 13 chunk-boundary suite, which
    /// shrinks `TEST_BATCH_ROWS` to force multiple chunks.
    #[test]
    fn distinct_deduplicates_rows() {
        let mut horn = HornBackend::new();
        let iri = |s: &str| Term::Iri(format!("http://ex/{s}"));
        horn.insert_triple(iri("e0"), iri("p"), iri("X"));
        horn.insert_triple(iri("e1"), iri("p"), iri("X"));
        horn.insert_triple(iri("e2"), iri("p"), iri("Y"));

        let scan = PhysicalPlan::BgpScan {
            patterns: vec![TriplePattern {
                subject: Term::Var(Var::new("s")),
                predicate: iri("p"),
                object: Term::Var(Var::new("o")),
            }],
        };
        let proj = PhysicalPlan::Project {
            vars: vec![Var::new("o")],
            inner: Box::new(scan),
        };
        let plan = PhysicalPlan::Distinct {
            inner: Box::new(proj),
        };

        let rt = Runtime::new(&horn);
        let mut op = rt.build(&plan).unwrap();
        let mut total = 0;
        while let Some(b) = op.next().unwrap() {
            assert!(!b.rows.is_empty(), "no empty chunks mid-stream");
            total += b.rows.len();
        }
        assert_eq!(total, 2, "distinct ?o should be {{X, Y}}");
    }
}
