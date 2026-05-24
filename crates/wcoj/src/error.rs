use thiserror::Error;

#[derive(Debug, Error)]
pub enum WcojError {
    #[error("query was cancelled")]
    Cancelled,
    #[error("requested ordering {0:?} not supported by source")]
    OrderingUnavailable(crate::ids::Ordering),
    #[error("internal: {0}")]
    Internal(String),
    #[error(transparent)]
    Arrow(#[from] arrow::error::ArrowError),
}

pub type Result<T> = std::result::Result<T, WcojError>;
