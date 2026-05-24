//! Cancellation token. SPEC-03 F7: queries respond within 100 ms.
//!
//! We poll an atomic `bool` once per output row (every 2048 rows when called
//! from the batch boundary) and once per leapfrog seek-loop iteration at
//! the top variable depth. At ≥5 ns/tuple that's ≥200M checks/sec —
//! well within the 100 ms latency budget.

use std::sync::atomic::{AtomicBool, Ordering as MemOrdering};
use std::sync::Arc;

use crate::error::{Result, WcojError};

#[derive(Clone, Default)]
pub struct CancelToken {
    flag: Arc<AtomicBool>,
}

impl CancelToken {
    pub fn new() -> Self {
        Self {
            flag: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn cancel(&self) {
        self.flag.store(true, MemOrdering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.flag.load(MemOrdering::Acquire)
    }

    /// Returns `Err(WcojError::Cancelled)` if the token has been cancelled.
    pub fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            Err(WcojError::Cancelled)
        } else {
            Ok(())
        }
    }
}
