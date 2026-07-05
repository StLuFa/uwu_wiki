//! 数据校验层 —— 防范超大/非法输入导致的问题。

use crate::block::{Block, BlockId};
use crate::config::ValidationConfig;
use crate::doc::{Document, Op};
use crate::error::{Result, WikiError};
use std::collections::HashSet;

/// 校验文档的整体合法性。
pub fn validate_doc(doc: &Document, rules: &ValidationConfig) -> Result<()> {
    // 标题长度。
    if doc.title.len() > rules.max_title_len {
        return Err(WikiError::Invalid(format!(
            "title too long: {} chars (max {})",
            doc.title.chars().count(),
            rules.max_title_len
        )));
    }

    // Block 总数。
    if doc.blocks.len() > rules.max_blocks_per_doc {
        return Err(WikiError::Invalid(format!(
            "too many blocks: {} (max {})",
            doc.blocks.len(),
            rules.max_blocks_per_doc
        )));
    }

    // 嵌套深度（从 root 开始 DFS）。
    let depth = max_depth(doc, &doc.root, 0);
    if depth > rules.max_block_depth {
        return Err(WikiError::Invalid(format!(
            "block tree too deep: {depth} (max {})",
            rules.max_block_depth
        )));
    }

    // 每块校验。
    for block in &doc.blocks {
        validate_block(block, rules)?;
    }

    // 子块数校验。
    for block in &doc.blocks {
        if block.children.len() > rules.max_children_per_block {
            return Err(WikiError::Invalid(format!(
                "block {} has too many children: {} (max {})",
                block.id,
                block.children.len(),
                rules.max_children_per_block
            )));
        }
    }

    Ok(())
}

/// 校验单个 Block 的内容合法性。
pub fn validate_block(block: &Block, rules: &ValidationConfig) -> Result<()> {
    let text_len = block.content.as_plain_text().chars().count();
    if text_len > rules.max_text_len_per_block {
        return Err(WikiError::Invalid(format!(
            "block {} text too long: {text_len} chars (max {})",
            block.id,
            rules.max_text_len_per_block
        )));
    }
    Ok(())
}

/// 校验 Op 在目标文档上下文中是否合法（应用前检查）。
pub fn validate_op(op: &Op, doc: &Document, rules: &ValidationConfig) -> Result<()> {
    match op {
        Op::InsertBlock { parent, block, .. } => {
            if doc.block(parent).is_none() {
                return Err(WikiError::NotFound(format!("parent block {parent}")));
            }
            validate_block(block, rules)?;
            // 插入后不超限。
            if doc.blocks.len() + 1 > rules.max_blocks_per_doc {
                return Err(WikiError::Invalid(format!(
                    "insert would exceed max blocks ({})",
                    rules.max_blocks_per_doc
                )));
            }
            // 子块数不超限。
            if let Some(p) = doc.block(parent) {
                if p.children.len() + 1 > rules.max_children_per_block {
                    return Err(WikiError::Invalid(format!(
                        "parent block {parent} would have too many children"
                    )));
                }
            }
        }
        Op::UpdateBlock { id, patch } => {
            if doc.block(id).is_none() {
                return Err(WikiError::NotFound(format!("block {id}")));
            }
            // 校验 patch 不导致文本超限。
            if let Some(text_val) = patch.get("text").and_then(|v| v.as_str()) {
                if text_val.chars().count() > rules.max_text_len_per_block {
                    return Err(WikiError::Invalid(format!(
                        "update would make block {id} text too long"
                    )));
                }
            }
        }
        Op::DeleteBlock { id } => {
            if doc.block(id).is_none() {
                return Err(WikiError::NotFound(format!("block {id}")));
            }
        }
        Op::MoveBlock { id, new_parent, .. } => {
            if doc.block(id).is_none() {
                return Err(WikiError::NotFound(format!("block {id}")));
            }
            if doc.block(new_parent).is_none() {
                return Err(WikiError::NotFound(format!("new parent {new_parent}")));
            }
            if let Some(p) = doc.block(new_parent) {
                if p.children.len() + 1 > rules.max_children_per_block {
                    return Err(WikiError::Invalid(format!(
                        "move would exceed max children for block {new_parent}"
                    )));
                }
            }
        }
        Op::UpdateDocMeta { patch, .. } => {
            if let Some(title) = patch.get("title").and_then(|v| v.as_str()) {
                if title.chars().count() > rules.max_title_len {
                    return Err(WikiError::Invalid(format!(
                        "new title too long: {} chars (max {})",
                        title.chars().count(),
                        rules.max_title_len
                    )));
                }
            }
        }
    }
    Ok(())
}

/// 计算 Block 树的最大深度。
fn max_depth(doc: &Document, id: &BlockId, current: usize) -> usize {
    let block = match doc.block(id) {
        Some(b) => b,
        None => return current,
    };
    let child_max = block
        .children
        .iter()
        .map(|c| max_depth(doc, c, current + 1))
        .max()
        .unwrap_or(current);
    child_max.max(current + 1)
}

/// 检测 Block 树中的循环引用（`children` 或 `parent` 指针形成环）。
pub fn detect_cycle(doc: &Document) -> Result<()> {
    let mut visited = HashSet::new();
    let mut in_stack = HashSet::new();
    if has_cycle(doc, &doc.root, &mut visited, &mut in_stack) {
        return Err(WikiError::Invalid("block tree contains a cycle".into()));
    }
    Ok(())
}

fn has_cycle(
    doc: &Document,
    id: &BlockId,
    visited: &mut HashSet<BlockId>,
    in_stack: &mut HashSet<BlockId>,
) -> bool {
    if in_stack.contains(id) {
        return true;
    }
    if visited.contains(id) {
        return false;
    }
    visited.insert(id.clone());
    in_stack.insert(id.clone());
    if let Some(block) = doc.block(id) {
        for child in &block.children {
            if has_cycle(doc, child, visited, in_stack) {
                return true;
            }
        }
    }
    in_stack.remove(id);
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::{BlockContent, BlockType};
    use crate::doc::SpaceId;

    #[test]
    fn valid_doc_passes() {
        let root = Block::new(BlockType::Paragraph, BlockContent::text("hello"), "a");
        let doc = Document::new("Valid", root, SpaceId::default());
        assert!(validate_doc(&doc, &ValidationConfig::default()).is_ok());
    }

    #[test]
    fn title_too_long_rejected() {
        let root = Block::new(BlockType::Paragraph, BlockContent::text("x"), "a");
        let mut doc = Document::new("x".repeat(501), root, SpaceId::default());
        // Fix title
        doc.title = "x".repeat(501);
        let rules = ValidationConfig {
            max_title_len: 500,
            ..Default::default()
        };
        assert!(validate_doc(&doc, &rules).is_err());
    }

    #[test]
    fn cycle_detected() {
        let root = Block::new(BlockType::Paragraph, BlockContent::text("r"), "a");
        let root_id = root.id.clone();
        let mut doc = Document::new("T", root, SpaceId::default());
        // Manually create a cycle: root → root
        if let Some(r) = doc.block_mut(&root_id) {
            r.children.push(root_id.clone());
        }
        assert!(detect_cycle(&doc).is_err());
    }
}
