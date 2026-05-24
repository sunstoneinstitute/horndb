//! Query executors. `WcojExecutor` and `BinaryHashExecutor` both produce
//! a stream of Arrow `RecordBatch`es.

pub mod binary_hash;
pub mod wcoj;

use arrow::record_batch::RecordBatch;

use crate::error::Result;

/// Common output type — a fallible iterator over batches.
pub type BatchStream<'a> = Box<dyn Iterator<Item = Result<RecordBatch>> + 'a>;
