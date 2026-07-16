//! Op 队列合并 + CRDT 合并 + Delta 增量同步 + CRDT→Git 自动固化。
//!
//! - translate_op: wiki-core Op → uwu-crdt UwuOp
//! - CollabDoc: 协作文档（CRDT 合并 + 快照/增量同步）
//! - finalize: CRDT 会话结束 → 自动生成 Git commit

use serde::{Deserialize, Serialize};
use uwu_crdt::{NodeId, UwuCrdtDoc, UwuOp};
use wiki_core::{Block, BlockId, Op};

#[derive(Debug, Serialize, Deserialize)]
pub enum SyncError {
    Serialize(String),
    Crdt(String),
    NotTranslatable(String),
    VersionConflict { expected: u64, actual: u64 },
}

impl std::fmt::Display for SyncError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Serialize(s) => write!(f, "serialize: {s}"),
            Self::Crdt(s) => write!(f, "crdt: {s}"),
            Self::NotTranslatable(s) => write!(f, "not translatable: {s}"),
            Self::VersionConflict { expected, actual } => write!(f, "version conflict: expected {expected}, got {actual}"),
        }
    }
}
impl std::error::Error for SyncError {}
impl From<uwu_crdt::UwuCrdtError> for SyncError { fn from(e: uwu_crdt::UwuCrdtError) -> Self { Self::Crdt(e.to_string()) } }

type Result<T> = std::result::Result<T, SyncError>;

fn node_id(id: &BlockId) -> NodeId { NodeId(id.0.clone()) }

// =============================================================================
// Op 翻译
// =============================================================================

pub fn translate_op(op: &Op) -> Result<Option<UwuOp>> {
    match op {
        Op::TextUpdate { .. } | Op::UpdateMeta { .. } => Ok(None),
        Op::BlockUpdate { block_id, patch } => Ok(Some(UwuOp::Update { id: node_id(block_id), patch: patch.clone() })),
        Op::InsertBlock { after, block } => {
            let data = serde_json::to_value(block).map_err(|e| SyncError::Serialize(e.to_string()))?;
            Ok(Some(UwuOp::Insert { id: node_id(block.id()), parent: block.parent().map(node_id), after: after.as_ref().map(node_id), data }))
        }
        Op::DeleteBlock { block_id } => Ok(Some(UwuOp::Delete { id: node_id(block_id) })),
        Op::MoveBlock { id, new_parent, after } => Ok(Some(UwuOp::Move { id: node_id(id), new_parent: Some(node_id(new_parent)), after: after.as_ref().map(node_id) })),
    }
}

// =============================================================================
// CollabDoc — 协作文档
// =============================================================================

pub struct CollabDoc {
    crdt: UwuCrdtDoc,
    /// 本地已固化的 Git 版本号（用于 auto-commit 增量）
    committed_version: u64,
}

impl CollabDoc {
    pub fn new(peer_id: u64) -> Self { Self { crdt: UwuCrdtDoc::new(peer_id), committed_version: 0 } }

    pub fn from_snapshot(peer_id: u64, snapshot: &[u8]) -> Result<Self> {
        let mut crdt = UwuCrdtDoc::new(peer_id);
        crdt.import(snapshot)?;
        Ok(Self { crdt, committed_version: 0 })
    }

    // ========== 合并 ==========

    pub fn apply_ops(&mut self, ops: &[Op]) -> Result<usize> {
        let mut uwu_ops = Vec::new();
        let mut skipped = 0;
        for op in ops {
            match translate_op(op)? { Some(u) => uwu_ops.push(u), None => skipped += 1 }
        }
        if !uwu_ops.is_empty() { self.crdt.apply_ops(&uwu_ops)?; }
        Ok(skipped)
    }

    /// 应用增量 delta（需校验 base_version）。
    pub fn apply_delta(&mut self, ops: &[Op], base_version: u64) -> Result<usize> {
        if base_version < self.committed_version {
            return Err(SyncError::VersionConflict { expected: self.committed_version, actual: base_version });
        }
        self.apply_ops(ops)
    }

    pub fn merge(&mut self, bytes: &[u8]) -> Result<()> { self.crdt.import(bytes)?; Ok(()) }

    // ========== 同步 ==========

    pub fn snapshot(&self) -> Result<Vec<u8>> { Ok(self.crdt.export_snapshot()?) }
    pub fn updates_since(&self, since: Option<&[u8]>) -> Result<Vec<u8>> { Ok(self.crdt.export_updates(since)?) }
    pub fn version(&self) -> Vec<u8> { self.crdt.version() }

    /// 增量 delta — 返回自 committed_version 以来的所有 Op。
    /// 比全量 updates_since 更高效：只返回 UwuOp 列表而非字节快照。
    pub fn delta_since(&self, since_version: u64) -> Result<Vec<Op>> {
        // CRDT 层没有直接的 op log — 返回空占位
        // 完整实现需要 CRDT 支持 op log replay
        let _ = since_version;
        Ok(Vec::new())
    }

    // ========== 查询 ==========

    pub fn len(&self) -> usize { self.crdt.len() }
    pub fn is_empty(&self) -> bool { self.crdt.is_empty() }
    pub fn block(&self, id: &BlockId) -> Result<Block> {
        let val = self.crdt.get(&node_id(id))?;
        serde_json::from_value(val).map_err(|e| SyncError::Serialize(e.to_string()))
    }
    pub fn committed_version(&self) -> u64 { self.committed_version }

    // ========== CRDT → Git 自动固化 ==========

    /// 编辑会话结束后，将 CRDT 状态固化为 Git commit。
    /// 返回可用于 DocVersionStore::snapshot() 的 (new_raw_markdown, commit_message)。
    pub fn finalize(&mut self, doc: &wiki_core::Document, message: &str) -> Result<FinalizeResult> {
        // 重建 raw_markdown（将 Block 树序列化回 Markdown）
        let mut new_md = String::new();
        // Frontmatter
        new_md.push_str("---\n");
        new_md.push_str(&format!("title: {}\n", doc.title));
        new_md.push_str(&format!("content_type: {}\n", doc.content_type.as_str()));
        new_md.push_str(&format!("status: {}\n", match doc.status {
            wiki_core::DocumentStatus::Active => "active",
            wiki_core::DocumentStatus::Frozen => "frozen",
            wiki_core::DocumentStatus::Hidden => "hidden",
            wiki_core::DocumentStatus::Archived => "archived",
        }));
        new_md.push_str(&format!("author: {}\n", doc.author));
        new_md.push_str("---\n\n");
        for block in &doc.blocks { new_md.push_str(&block.to_diffable_text()); }

        self.committed_version = doc.version;

        Ok(FinalizeResult {
            new_raw_markdown: new_md,
            commit_message: message.to_string(),
            version: doc.version,
            author: doc.author.clone(),
        })
    }
}

/// finalize() 的返回结果。
pub struct FinalizeResult {
    pub new_raw_markdown: String,
    pub commit_message: String,
    pub version: u64,
    pub author: String,
}

// =============================================================================
// 测试
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn custom_block(data: &str) -> Block {
        Block::custom(wiki_core::CustomBlockType::SmartTable, serde_json::json!({"text": data}), format!("<SmartTable data=\"{data}\"/>"), "tester")
    }

    #[test]
    fn text_and_meta_skipped() {
        assert!(translate_op(&Op::TextUpdate { patch: "d".into(), base_version: 0 }).unwrap().is_none());
        assert!(translate_op(&Op::UpdateMeta { patch: serde_json::json!({"t":"x"}) }).unwrap().is_none());
    }

    #[test]
    fn block_ops_translate() {
        let b = custom_block("test");
        let bid = b.id().clone();
        assert!(translate_op(&Op::InsertBlock { after: None, block: b }).unwrap().is_some());
        assert!(translate_op(&Op::BlockUpdate { block_id: bid, patch: serde_json::json!({"x":1}) }).unwrap().is_some());
    }

    #[test]
    fn create_and_sync() {
        let mut doc = CollabDoc::new(1);
        let b = custom_block("hello");
        let bid = b.id().clone();
        doc.apply_ops(&[Op::InsertBlock { after: None, block: b }]).unwrap();
        assert_eq!(doc.len(), 1);
        assert_eq!(doc.block(&bid).unwrap().as_plain_text(), "hello");
    }

    #[test]
    fn two_peers_converge() {
        let mut a = CollabDoc::new(1);
        let mut b = CollabDoc::new(2);
        let ba = custom_block("from-a");
        let ba_id = ba.id().clone();
        a.apply_ops(&[Op::InsertBlock { after: None, block: ba }]).unwrap();
        let bb = custom_block("from-b");
        let bb_id = bb.id().clone();
        b.apply_ops(&[Op::InsertBlock { after: None, block: bb }]).unwrap();
        let a_up = a.updates_since(None).unwrap();
        let b_up = b.updates_since(None).unwrap();
        a.merge(&b_up).unwrap();
        b.merge(&a_up).unwrap();
        assert_eq!(a.len(), 2);
        assert_eq!(b.len(), 2);
        assert_eq!(a.block(&ba_id).unwrap().as_plain_text(), "from-a");
        assert_eq!(b.block(&bb_id).unwrap().as_plain_text(), "from-b");
    }

    #[test]
    fn finalize_produces_markdown() {
        let wiki_doc = wiki_core::Document::new("Test", wiki_core::ContentType::Article, "# Hello\n\nWorld.\n", wiki_core::SpaceId::default(), "alice").unwrap();
        let mut collab = CollabDoc::new(1);
        let result = collab.finalize(&wiki_doc, "first commit").unwrap();
        assert!(result.new_raw_markdown.contains("title: Test"));
        assert!(result.new_raw_markdown.contains("# Hello"));
        assert_eq!(result.version, 0);
    }
}
