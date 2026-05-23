//! Storage error taxonomy.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("dictionary capacity exceeded ({0} terms)")]
    DictionaryFull(u64),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("n-triples parse error: {0}")]
    NtriplesParse(String),
    #[error("invalid term for storage: {0}")]
    InvalidTerm(String),
}

pub type Result<T> = std::result::Result<T, StorageError>;
