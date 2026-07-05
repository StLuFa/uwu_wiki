//! 错误类型。

use thiserror::Error;

/// 错误码 —— 调用方可据此做程序化处理，无需匹配字符串。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorCode {
    /// W001: 目标不存在。
    NotFound = 1,
    /// W002: 存储操作失败。
    Storage = 2,
    /// W003: 权限不足。
    PermissionDenied = 3,
    /// W004: 版本冲突。
    VersionConflict = 4,
    /// W005: 序列化/反序列化失败。
    Serialization = 5,
    /// W006: LLM 调用失败。
    Llm = 6,
    /// W007: 输入不合法。
    Invalid = 7,
}

#[derive(Debug, Error)]
pub enum WikiError {
    #[error("W001: not found: {0}")]
    NotFound(String),
    #[error("W002: storage: {0}")]
    Storage(String),
    #[error("W003: permission denied: {0}")]
    PermissionDenied(String),
    #[error("W004: version conflict: {0}")]
    VersionConflict(String),
    #[error("W005: serialization: {0}")]
    Serialization(String),
    #[error("W006: llm: {0}")]
    Llm(String),
    #[error("W007: invalid: {0}")]
    Invalid(String),
}

impl WikiError {
    /// 返回该错误的稳定错误码。
    pub fn error_code(&self) -> ErrorCode {
        match self {
            WikiError::NotFound(_) => ErrorCode::NotFound,
            WikiError::Storage(_) => ErrorCode::Storage,
            WikiError::PermissionDenied(_) => ErrorCode::PermissionDenied,
            WikiError::VersionConflict(_) => ErrorCode::VersionConflict,
            WikiError::Serialization(_) => ErrorCode::Serialization,
            WikiError::Llm(_) => ErrorCode::Llm,
            WikiError::Invalid(_) => ErrorCode::Invalid,
        }
    }
}

pub type Result<T> = std::result::Result<T, WikiError>;
