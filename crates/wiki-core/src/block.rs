//! Block 模型 — 文本类委托 markdown-rs AST，结构类自定义 JSON。
//!
//! ## 设计原则
//!
//! - **文本类 Block**（MarkdownBlock）包装 markdown-rs AST，解析/渲染/文本提取免费。
//! - **结构类 Block**（CustomBlock）以 JSON 为载体，支持树形嵌套（children/parent）。
//! - Markdown AST 是权威来源，`raw` 字段保留原始源码用于 Git diff。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// =============================================================================
// BlockId
// =============================================================================

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BlockId(pub String);

impl BlockId {
    pub fn new() -> Self { Self(Uuid::now_v7().to_string()) }
}
impl Default for BlockId { fn default() -> Self { Self::new() } }
impl std::fmt::Display for BlockId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "{}", self.0) }
}

// =============================================================================
// CustomBlockType
// =============================================================================

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CustomBlockType {
    SmartTable,
    Workflow,
    Mindmap,
    AudioPlayer,
    VideoPlayer,
    Artwork,
    Playlist,
    Mermaid,
    Custom(String),
}

impl CustomBlockType {
    pub fn as_str(&self) -> &str {
        match self {
            Self::SmartTable => "SmartTable", Self::Workflow => "Workflow",
            Self::Mindmap => "Mindmap", Self::AudioPlayer => "AudioPlayer",
            Self::VideoPlayer => "VideoPlayer", Self::Artwork => "Artwork",
            Self::Playlist => "Playlist", Self::Mermaid => "Mermaid",
            Self::Custom(s) => s.as_str(),
        }
    }
    pub fn from_str(s: &str) -> Self {
        match s {
            "SmartTable" => Self::SmartTable, "Workflow" => Self::Workflow,
            "Mindmap" => Self::Mindmap, "AudioPlayer" => Self::AudioPlayer,
            "VideoPlayer" => Self::VideoPlayer, "Artwork" => Self::Artwork,
            "Playlist" => Self::Playlist, "Mermaid" => Self::Mermaid,
            other => Self::Custom(other.to_string()),
        }
    }
}
impl std::fmt::Display for CustomBlockType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "{}", self.as_str()) }
}

// =============================================================================
// Block enum
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Block {
    Markdown(MarkdownBlock),
    Custom(CustomBlock),
}

// =============================================================================
// MarkdownBlock
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarkdownBlock {
    pub id: BlockId,
    pub ast: serde_json::Value,
    pub node_type: String,
    pub raw: String,
    pub version: u64,
    pub embedding: Option<Vec<f32>>,
    pub embedding_version: u64,
    pub meta: BlockMeta,
}

// =============================================================================
// CustomBlock — 支持树形嵌套
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomBlock {
    pub id: BlockId,
    pub ty: CustomBlockType,
    pub data: serde_json::Value,
    pub raw: String,
    /// 有序子块列表（脑图节点、表格的行、工作流子步骤等）
    pub children: Vec<BlockId>,
    /// 父块 ID（None 为根级）
    pub parent: Option<BlockId>,
    pub version: u64,
    pub embedding: Option<Vec<f32>>,
    pub embedding_version: u64,
    pub meta: BlockMeta,
}

// =============================================================================
// BlockMeta
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockMeta {
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub created_by: String,
}

impl BlockMeta {
    pub fn new(author: impl Into<String>) -> Self {
        let now = Utc::now();
        Self { created_at: now, updated_at: now, created_by: author.into() }
    }
}

// =============================================================================
// Block impl
// =============================================================================

impl Block {
    pub fn markdown(ast: serde_json::Value, node_type: impl Into<String>, raw: impl Into<String>, author: impl Into<String>) -> Self {
        Block::Markdown(MarkdownBlock {
            id: BlockId::new(), ast, node_type: node_type.into(), raw: raw.into(),
            version: 0, embedding: None, embedding_version: 0, meta: BlockMeta::new(author),
        })
    }

    pub fn custom(ty: CustomBlockType, data: serde_json::Value, raw: impl Into<String>, author: impl Into<String>) -> Self {
        Block::Custom(CustomBlock {
            id: BlockId::new(), ty, data, raw: raw.into(),
            children: Vec::new(), parent: None,
            version: 0, embedding: None, embedding_version: 0, meta: BlockMeta::new(author),
        })
    }

    pub fn custom_with_parent(ty: CustomBlockType, data: serde_json::Value, raw: impl Into<String>, author: impl Into<String>, parent: BlockId) -> Self {
        Block::Custom(CustomBlock {
            id: BlockId::new(), ty, data, raw: raw.into(),
            children: Vec::new(), parent: Some(parent),
            version: 0, embedding: None, embedding_version: 0, meta: BlockMeta::new(author),
        })
    }

    pub fn id(&self) -> &BlockId {
        match self { Block::Markdown(b) => &b.id, Block::Custom(b) => &b.id }
    }
    pub fn version(&self) -> u64 {
        match self { Block::Markdown(b) => b.version, Block::Custom(b) => b.version }
    }
    pub fn children(&self) -> &[BlockId] {
        match self { Block::Markdown(_) => &[], Block::Custom(b) => &b.children }
    }
    pub fn parent(&self) -> Option<&BlockId> {
        match self { Block::Markdown(_) => None, Block::Custom(b) => b.parent.as_ref() }
    }
    pub fn embedding(&self) -> Option<&Vec<f32>> {
        match self { Block::Markdown(b) => b.embedding.as_ref(), Block::Custom(b) => b.embedding.as_ref() }
    }
    pub fn is_embedding_stale(&self) -> bool {
        match self { Block::Markdown(b) => b.embedding.is_some() && b.embedding_version < b.version, Block::Custom(b) => b.embedding.is_some() && b.embedding_version < b.version }
    }
    pub fn bump_version(&mut self) {
        match self {
            Block::Markdown(b) => { b.version += 1; b.meta.updated_at = Utc::now(); }
            Block::Custom(b) => { b.version += 1; b.meta.updated_at = Utc::now(); }
        }
    }

    /// 往 CustomBlock 添加子块。
    pub fn add_child(&mut self, child_id: BlockId) {
        match self { Block::Custom(b) => b.children.push(child_id), _ => {} }
    }

    /// 往 CustomBlock 移除子块。
    pub fn remove_child(&mut self, child_id: &BlockId) {
        match self { Block::Custom(b) => b.children.retain(|c| c != child_id), _ => {} }
    }

    /// 设置 CustomBlock 的父块。
    pub fn set_parent(&mut self, parent_id: Option<BlockId>) {
        match self { Block::Custom(b) => b.parent = parent_id, _ => {} }
    }

    pub fn as_plain_text(&self) -> String {
        match self {
            Block::Markdown(b) => crate::markdown::ast_to_string(&b.raw).unwrap_or_else(|| b.raw.clone()),
            Block::Custom(b) => extract_text_from_json(&b.data),
        }
    }

    pub fn to_diffable_text(&self) -> String {
        match self {
            Block::Markdown(b) => b.raw.clone(),
            Block::Custom(b) => format!("<{} data={} />\n", b.ty, b.data),
        }
    }
}

fn extract_text_from_json(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(arr) => arr.iter().map(extract_text_from_json).collect::<Vec<_>>().join(" "),
        serde_json::Value::Object(map) => map.values().map(extract_text_from_json).collect::<Vec<_>>().join(" "),
        _ => String::new(),
    }
}

// =============================================================================
// Block 树操作
// =============================================================================

/// 在 blocks 列表中查找某父块的所有直接子块。
pub fn children_of<'a>(blocks: &'a [Block], parent_id: &BlockId) -> Vec<&'a Block> {
    blocks.iter().filter(|b| b.parent() == Some(parent_id)).collect()
}

/// 前序遍历子树（含 root），返回 BlockId 顺序。
pub fn descendants(blocks: &[Block], root: &BlockId) -> Vec<BlockId> {
    let mut out = Vec::new();
    let mut stack = vec![root.clone()];
    while let Some(current) = stack.pop() {
        out.push(current.clone());
        if let Some(block) = blocks.iter().find(|b| b.id() == &current) {
            for child in block.children().iter().rev() {
                stack.push(child.clone());
            }
        }
    }
    out
}

/// 检查 node 是否是 ancestor 的后代（防环）。
pub fn is_descendant_of(blocks: &[Block], node: &BlockId, ancestor: &BlockId) -> bool {
    if node == ancestor { return true; }
    for child in children_of(blocks, ancestor) {
        if is_descendant_of(blocks, node, child.id()) { return true; }
    }
    false
}

/// 在 blocks 中查找块。
pub fn find_block<'a>(blocks: &'a [Block], id: &BlockId) -> Option<&'a Block> {
    blocks.iter().find(|b| b.id() == id)
}

/// 在 blocks 中可变查找块。
pub fn find_block_mut<'a>(blocks: &'a mut [Block], id: &BlockId) -> Option<&'a mut Block> {
    blocks.iter_mut().find(|b| b.id() == id)
}
