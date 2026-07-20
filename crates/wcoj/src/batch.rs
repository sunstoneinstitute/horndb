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
        let flushed = if !self.cols.is_empty() && self.cols[0].len() == STANDARD_VECTOR_SIZE {
            self.finish_internal()
        } else {
            None
        };
        for (col, &v) in self.cols.iter_mut().zip(row.iter()) {
            col.push(v);
        }
        flushed
    }

    /// Bulk-append a run of rows that share a fixed `prefix` (one value per
    /// non-leaf column, in column order) while the last column takes successive
    /// `leaf_values`. Used by the WCOJ leaf hot path: when the final variable's
    /// bindings are a precomputed intersection buffer, this blits them straight
    /// into the column instead of pushing one row at a time through the executor
    /// state machine (SPEC-03 NF1).
    ///
    /// Appends only up to the next flush boundary and returns
    /// `(rows_appended, Some(batch))` when that append filled and flushed a
    /// batch. The caller loops, advancing through `leaf_values` by
    /// `rows_appended`, until all are consumed. Row order and column contents
    /// are identical to calling `push_row([..prefix, value])` for each value.
    pub fn push_run_chunk(
        &mut self,
        prefix: &[u64],
        leaf_values: &[u64],
    ) -> (usize, Option<RecordBatch>) {
        debug_assert_eq!(prefix.len() + 1, self.vars.len());
        if leaf_values.is_empty() {
            return (0, None);
        }
        let cur = self.cols[prefix.len()].len();
        let n = leaf_values.len().min(STANDARD_VECTOR_SIZE - cur);
        // Non-leaf columns: extend with the fixed prefix value repeated `n`×.
        for (col, &v) in self.cols[..prefix.len()].iter_mut().zip(prefix.iter()) {
            col.resize(cur + n, v);
        }
        // Leaf column: copy the run of distinct bindings.
        self.cols[prefix.len()].extend_from_slice(&leaf_values[..n]);
        let flushed = if self.cols[prefix.len()].len() == STANDARD_VECTOR_SIZE {
            self.finish_internal()
        } else {
            None
        };
        (n, flushed)
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
        if self.cols.is_empty() || self.cols[0].is_empty() {
            return None;
        }
        let arrays: Vec<ArrayRef> = self
            .cols
            .iter_mut()
            .map(|c| {
                // Swap in a fresh buffer so the builder is ready for reuse.
                let take = std::mem::replace(c, Vec::with_capacity(STANDARD_VECTOR_SIZE));
                Arc::new(UInt64Array::from(take)) as ArrayRef
            })
            .collect();
        RecordBatch::try_new(self.schema.clone(), arrays).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::array::UInt64Array;

    /// Collect every row a builder produced across a sequence of flushes plus a
    /// final drain, as `Vec<Vec<u64>>` (row-major).
    fn rows(batches: &[RecordBatch]) -> Vec<Vec<u64>> {
        let mut out = Vec::new();
        for b in batches {
            let cols: Vec<&UInt64Array> = (0..b.num_columns())
                .map(|c| b.column(c).as_any().downcast_ref::<UInt64Array>().unwrap())
                .collect();
            for r in 0..b.num_rows() {
                out.push(cols.iter().map(|c| c.value(r)).collect());
            }
        }
        out
    }

    #[test]
    fn push_run_chunk_matches_row_by_row_across_flushes() {
        // A run longer than STANDARD_VECTOR_SIZE so several flush boundaries are
        // crossed; two columns so prefix replication is exercised.
        let vars = vec![Var(0), Var(1)];
        let prefix = [42u64];
        let leaf: Vec<u64> = (0..(STANDARD_VECTOR_SIZE as u64 * 2 + 5)).collect();

        // Reference: one row at a time.
        let mut ref_b = BindingBatchBuilder::new(vars.clone());
        let mut ref_batches = Vec::new();
        for &v in &leaf {
            if let Some(b) = ref_b.push_row(&[prefix[0], v]) {
                ref_batches.push(b);
            }
        }
        if let Some(b) = ref_b.finish() {
            ref_batches.push(b);
        }

        // Under test: chunked bulk append.
        let mut bulk_b = BindingBatchBuilder::new(vars);
        let mut bulk_batches = Vec::new();
        let mut pos = 0usize;
        while pos < leaf.len() {
            let (n, flushed) = bulk_b.push_run_chunk(&prefix, &leaf[pos..]);
            assert!(n > 0);
            pos += n;
            if let Some(b) = flushed {
                bulk_batches.push(b);
            }
        }
        if let Some(b) = bulk_b.finish() {
            bulk_batches.push(b);
        }

        assert_eq!(rows(&ref_batches), rows(&bulk_batches));
    }
}
