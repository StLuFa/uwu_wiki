//! 知识库空间 — Op 回放 + 树操作 + CRDT→Git 自动固化。

use crate::block::{self, Block, BlockId};
use crate::doc::{Document, Op, SpaceId};
use crate::error::{Result, WikiError};
use chrono::Utc;

#[derive(Clone)]
pub struct WikiSpace { id: SpaceId }

impl WikiSpace {
    pub fn new(id: SpaceId) -> Self { Self { id } }
    pub fn id(&self) -> &SpaceId { &self.id }

    pub fn create_document(&self, title: &str, ct: crate::doc::ContentType, raw: &str, author: &str) -> Result<Document> {
        Document::new(title, ct, raw, self.id.clone(), author)
    }

    pub fn parse_document(&self, raw: &str) -> Result<Document> {
        Document::parse(raw, self.id.clone())
    }

    pub fn reparse(&self, doc: &mut Document) -> Result<()> { doc.reparse() }

    pub fn apply_ops(&self, doc: &mut Document, ops: &[Op]) -> Result<()> {
        for op in ops { self.apply_op(doc, op)?; }
        Ok(())
    }

    fn apply_op(&self, doc: &mut Document, op: &Op) -> Result<()> {
        match op {
            Op::TextUpdate { base_version, .. } => {
                if doc.version != *base_version {
                    return Err(WikiError::Invalid(format!("version conflict: expected {} got {}", base_version, doc.version)));
                }
                doc.version += 1;
            }
            Op::BlockUpdate { block_id, patch } => {
                let blk = doc.block_mut(block_id).ok_or_else(|| WikiError::NotFound(format!("block {block_id}")))?;
                match blk {
                    Block::Custom(custom) => { merge_json(&mut custom.data, patch); custom.version += 1; }
                    _ => return Err(WikiError::Invalid("BlockUpdate only for custom blocks".into())),
                }
                doc.version += 1;
            }
            Op::InsertBlock { after, block } => {
                let bid = block.id().clone();
                if let Some(ref pid) = block.parent().cloned() {
                    if let Some(parent) = doc.block_mut(pid) {
                        match parent {
                            Block::Custom(p) => {
                                if let Some(after_id) = after {
                                    let pos = p.children.iter().position(|c| c == after_id).map(|i| i + 1).unwrap_or(p.children.len());
                                    p.children.insert(pos, bid.clone());
                                } else {
                                    p.children.push(bid.clone());
                                }
                            }
                            _ => {}
                        }
                    }
                }
                doc.blocks.push(block.clone());
                doc.version += 1;
            }
            Op::DeleteBlock { block_id } => {
                let to_remove = doc.descendants(block_id);
                let parent_id = doc.block(block_id).and_then(|b| b.parent().cloned());
                if let Some(pid) = parent_id.as_ref() {
                    if let Some(p) = doc.block_mut(pid) {
                        p.remove_child(block_id);
                    }
                }
                doc.blocks.retain(|b| !to_remove.contains(b.id()));
                doc.version += 1;
            }
            Op::MoveBlock { id, new_parent, after } => {
                if block::is_descendant_of(&doc.blocks, new_parent, id) {
                    return Err(WikiError::Invalid("cannot move block into its own subtree".into()));
                }
                let old_parent = doc.block(id).and_then(|b| b.parent().cloned());
                if let Some(op) = old_parent.as_ref() {
                    if let Some(p) = doc.block_mut(op) { p.remove_child(id); }
                }
                if let Some(b) = doc.block_mut(id) { b.set_parent(Some(new_parent.clone())); }
                if let Some(np) = doc.block_mut(new_parent) {
                    match np {
                        Block::Custom(p) => {
                            if let Some(after_id) = after.as_ref() {
                                let pos = p.children.iter().position(|c| c == after_id).map(|i| i+1).unwrap_or(p.children.len());
                                p.children.insert(pos, id.clone());
                            } else { p.children.push(id.clone()); }
                        }
                        _ => {}
                    }
                }
                doc.version += 1;
            }
            Op::UpdateMeta { patch } => {
                apply_doc_meta(doc, patch);
                doc.version += 1;
            }
        }
        doc.updated_at = Utc::now();
        Ok(())
    }
}

fn merge_json(target: &mut serde_json::Value, patch: &serde_json::Value) {
    match (target, patch) {
        (serde_json::Value::Object(t), serde_json::Value::Object(p)) => { for (k, v) in p { t.insert(k.clone(), v.clone()); } }
        (t, p) => *t = p.clone(),
    }
}

fn apply_doc_meta(doc: &mut Document, patch: &serde_json::Value) {
    if let Some(t) = patch.get("title").and_then(|v| v.as_str()) { doc.title = t.to_string(); }
    if let Some(tags) = patch.get("tags").and_then(|v| v.as_array()) {
        doc.tags = tags.iter().filter_map(|v| v.as_str().map(str::to_string)).collect();
    }
    if let Some(s) = patch.get("status").and_then(|v| v.as_str()) {
        if let Some(st) = crate::doc::DocumentStatus::from_str(s) { doc.status = st; }
    }
    if let Some(p) = patch.get("path").and_then(|v| v.as_str()) { doc.path = Some(p.to_string()); }
}
