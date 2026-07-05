//! 错误类型。

use thiserror::Error;

#[derive(Debug, Error)]
pub enum WikiError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("storage: {0}")]
    Storage(String),
    #[error("permission denied: {0}")]
    PermissionDenied(String),
    #[error("version conflict: {0}")]
    VersionConflict(String),
    #[error("serialization: {0}")]
    Serialization(String),
    #[error("llm: {0}")]
    Llm(String),
    #[error("invalid: {0}")]
    Invalid(String),
}

pub type Result<T> = std::result::Result<T, WikiError>;
