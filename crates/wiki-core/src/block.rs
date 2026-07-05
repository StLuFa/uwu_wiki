//! Block 模型 —— 文档、表格、图的最小单元。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Block 唯一标识（UUID v7，时间有序，便于排序）。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BlockId(pub String);

impl BlockId {
    pub fn new() -> Self {
        Self(Uuid::now_v7().to_string())
    }
}

impl Default for BlockId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for BlockId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// 内置 Block 类型。自定义类型通过 [`crate::registry::BlockTypeRegistry`] 注册。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlockType {
    Paragraph,
    Heading,
    BulletedList,
    NumberedList,
    Toggle,
    Quote,
    Callout,
    Code,
    Divider,
    Image,
    Embed,
    /// 指向 wiki-table 实例的引用块。
    TableRef,
    /// 指向 wiki-graph 实例的引用块。
    GraphRef,
    /// 数据库视图（table / kanban / gallery / timeline）。
    DatabaseView,
    /// 注册的自定义类型。
    Custom(String),
}

/// Block 内容 —— 类型特定的 JSON 封装。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BlockContent(pub serde_json::Value);

impl BlockContent {
    pub fn text(s: impl Into<String>) -> Self {
        Self(serde_json::json!({ "text": s.into() }))
    }

    /// 提取纯文本表示（供 embedding / 全文索引用）。
    pub fn as_plain_text(&self) -> String {
        self.0
            .get("text")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_default()
    }
}

/// Block 元数据。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockMeta {
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub created_by: String,
}

impl BlockMeta {
    pub fn new(author: impl Into<String>) -> Self {
        let now = Utc::now();
        Self {
            created_at: now,
            updated_at: now,
            created_by: author.into(),
        }
    }
}

/// 文档 / 表格 / 图的最小单元。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Block {
    pub id: BlockId,
    pub ty: BlockType,
    pub content: BlockContent,
    /// 有序子块列表。
    pub children: Vec<BlockId>,
    pub parent: Option<BlockId>,
    /// 乐观并发版本号。
    pub version: u64,
    /// 懒生成，LLM Worker 异步填充。
    pub embedding: Option<Vec<f32>>,
    /// 生成 `embedding` 时的 `version`，用于陈旧检测（#8）。
    pub embedding_version: u64,
    pub meta: BlockMeta,
}

impl Block {
    pub fn new(ty: BlockType, content: BlockContent, author: impl Into<String>) -> Self {
        Self {
            id: BlockId::new(),
            ty,
            content,
            children: Vec::new(),
            parent: None,
            version: 0,
            embedding: None,
            embedding_version: 0,
            meta: BlockMeta::new(author),
        }
    }

    /// embedding 是否落后于当前内容版本（陈旧）。
    pub fn is_embedding_stale(&self) -> bool {
        self.embedding.is_some() && self.embedding_version < self.version
    }

    /// 内容更新后推进版本（不动 `embedding_version`，交由 worker 对齐）。
    pub fn bump_version(&mut self) {
        self.version += 1;
        self.meta.updated_at = Utc::now();
    }
}
