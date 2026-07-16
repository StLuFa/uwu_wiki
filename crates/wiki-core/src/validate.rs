//! 文档校验。

use crate::doc::Document;
use crate::error::{Result, WikiError};

/// 校验文档结构。
pub fn validate_document(doc: &Document) -> Result<()> {
    if doc.title.is_empty() {
        return Err(WikiError::Invalid("title cannot be empty".into()));
    }
    if doc.raw_markdown.is_empty() && doc.blocks.is_empty() {
        return Err(WikiError::Invalid("document cannot be empty".into()));
    }
    Ok(())
}
