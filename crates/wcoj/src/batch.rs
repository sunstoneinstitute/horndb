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
    #[allow(dead_code)]
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
