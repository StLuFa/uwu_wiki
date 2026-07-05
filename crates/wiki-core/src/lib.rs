//! # wiki-core
//!
//! uwu_wiki 核心：Block 引擎 + Document/Op 模型 + 全部存储/LLM 端口 **trait 定义**。
//!
//! ## 设计约束（见 ARCHITECTURE.md §1.5）
//!
//! - **核心纯粹性**：除 serde/uuid/chrono 外零依赖；**不含存储/LLM 实现**。
//! - **端口/适配器**：全部存储能力以 trait（端口）暴露，实现由宿主注入。
//! - **单向依赖**：`wiki-core` 不依赖任何其他 wiki-* crate 或引擎。
//!
//! 参考实现见 `wiki-testkit`（dev-dependency）；生产由 `agent-context-db` 注入。

// ---- 子模块 ----
pub mod block;
pub mod config;
pub mod doc;
pub mod error;
pub mod link;
pub mod markdown;
pub mod registry;
pub mod space;
pub mod storage;
pub mod utils;
pub mod validate;

// ---- 核心类型 ----
pub use block::{Block, BlockContent, BlockId, BlockMeta, BlockType};
pub use doc::{DocId, Document, Op, SpaceId};
pub use error::{ErrorCode, Result, WikiError};

// ---- 配置 ----
pub use config::WikiConfig;

// ---- 链接 ----
pub use link::{parse_links, resolve_links, LinkGraph, LinkTarget, WikiLink};

// ---- 渲染 ----
pub use registry::{BlockTypeRegistry, MarkdownRenderer, Render};

// ---- 存储端口 ----
pub use storage::{
    BlobId, BlobStore, BlockChange, BoolOp, ChangeKind, DocDiff, DocStore, DocVersionStore,
    LinkStore, MatchMode, OpLog, TextHit, TextIndex, TextQuery, VectorSearchResult, VectorStore,
    VersionEntry, VersionId, WikiStorage,
};

// ---- 空间（主入口）----
pub use space::WikiSpace;

// ---- 工具 ----
pub use utils::cosine_similarity;

// =============================================================================
// 测试
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_version_and_staleness() {
        let mut b = Block::new(BlockType::Paragraph, BlockContent::text("hello"), "agent-1");
        assert!(!b.is_embedding_stale());

        b.embedding = Some(vec![0.1, 0.2]);
        b.embedding_version = b.version;
        assert!(!b.is_embedding_stale());

        b.bump_version();
        assert!(b.is_embedding_stale(), "内容更新后 embedding 应标记陈旧");
    }

    #[test]
    fn document_block_lookup() {
        let root = Block::new(BlockType::Paragraph, BlockContent::text("root"), "a");
        let root_id = root.id.clone();
        let doc = Document::new("Test", root, SpaceId::default());
        assert!(doc.block(&root_id).is_some());
        assert_eq!(doc.root, root_id);
    }

    #[test]
    fn empty_block_text_is_valid() {
        let b = Block::new(BlockType::Paragraph, BlockContent::text(""), "a");
        assert_eq!(b.content.as_plain_text(), "");
    }

    #[test]
    fn deep_nesting() {
        let root = Block::new(BlockType::Paragraph, BlockContent::text("r"), "a");
        let root_id = root.id.clone();
        let mut doc = Document::new("Deep", root, SpaceId::default());
        let mut parent = root_id;
        for i in 0..50 {
            let child = Block::new(BlockType::Paragraph, BlockContent::text(format!("l{i}")), "a");
            let cid = child.id.clone();
            doc.apply(Op::InsertBlock { parent: parent.clone(), after: None, block: child }).unwrap();
            parent = cid;
        }
        assert_eq!(doc.blocks.len(), 51);
        assert_eq!(doc.descendants(&doc.root).len(), 51);
    }

    #[test]
    fn special_chars_in_title_and_text() {
        let root = Block::new(BlockType::Paragraph, BlockContent::text("你好 🌍! @#$%"), "a");
        let doc = Document::new("标题 — 特殊字符 «emoji»", root, SpaceId::default());
        assert!(doc.title.contains('—'));
        assert!(doc.title.contains('«'));
    }

    #[test]
    fn insert_at_last_child_position() {
        let root = Block::new(BlockType::Paragraph, BlockContent::text("r"), "a");
        let root_id = root.id.clone();
        let mut doc = Document::new("T", root, SpaceId::default());
        let a = Block::new(BlockType::Paragraph, BlockContent::text("a"), "t");
        let aid = a.id.clone();
        let b = Block::new(BlockType::Paragraph, BlockContent::text("b"), "t");
        let bid = b.id.clone();
        doc.apply(Op::InsertBlock { parent: root_id.clone(), after: None, block: a }).unwrap();
        doc.apply(Op::InsertBlock { parent: root_id.clone(), after: Some(aid.clone()), block: b }).unwrap();
        let children = doc.children(&root_id);
        assert_eq!(children[0].id, aid);
        assert_eq!(children[1].id, bid);
    }
}
