//! # wiki-core
//!
//! uwu_wiki 核心：基于 markdown-rs 的 Block 引擎 + Document/Op 模型 + 全部存储端口 trait。
//!
//! ## 设计原则
//!
//! - **markdown-rs 为文本引擎**：所有 Markdown 解析/渲染委托给 markdown-rs。
//! - **自定义 Block 为结构引擎**：表格/工作流/脑图/媒体等以 JSON Block 承载，支持树形嵌套。
//! - **raw_markdown 为真值源**：含 YAML frontmatter，Git 存储永远可 diff。
//! - **端口/适配器**：全部存储能力以 trait 暴露，实现由宿主注入。

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

pub use block::{
    Block, BlockId, BlockMeta, CustomBlock, CustomBlockType, MarkdownBlock,
    children_of, descendants, find_block, find_block_mut, is_descendant_of,
};
pub use doc::{
    ContentType, DerivationChain, DerivationRule, DocId, Document, DocumentStatus,
    Frontmatter, Op, SpaceId, parse_custom_component, parse_frontmatter, parse_markdown_to_blocks,
};
pub use error::{ErrorCode, Result, WikiError};
pub use config::WikiConfig;
pub use link::{parse_links, resolve_links, CrossReference, LinkGraph, LinkTarget, WikiLink};
pub use markdown::{ast_to_html, ast_to_string, md_to_html, parse_to_ast};
pub use registry::{BlockTypeRegistry, MarkdownRenderer, Render};
pub use storage::{
    BlobId, BlobStore, BlockChange, BoolOp, ChangeKind, DocDiff, DocEvent, DocStore,
    DocSummary, DocVersionStore, EventBus, EventStream, HybridHit, LinkStore, MatchMode,
    MergeConflict, MergeResult, OpLog, Permission, PermissionStore, TextHit, TextIndex,
    TextQuery, VectorSearchResult, VectorStore, VersionEntry, VersionId, WikiStorage,
};
pub use space::WikiSpace;
pub use utils::cosine_similarity;
