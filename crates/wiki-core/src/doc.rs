//! Document 模型 — Markdown frontmatter + raw_markdown 真值源 + L0/L1/L2 三层编码。

use crate::block::{self, Block, BlockId, CustomBlockType};
use crate::error::{Result, WikiError};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DocId(pub String);
impl DocId { pub fn new() -> Self { Self(Uuid::now_v7().to_string()) } }
impl Default for DocId { fn default() -> Self { Self::new() } }
impl std::fmt::Display for DocId { fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "{}", self.0) } }

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SpaceId { pub library: String, pub wiki: String }
impl SpaceId {
    pub fn new(library: impl Into<String>, wiki: impl Into<String>) -> Self { Self { library: library.into(), wiki: wiki.into() } }
}
impl Default for SpaceId { fn default() -> Self { Self { library: "default".into(), wiki: "default".into() } } }
impl std::fmt::Display for SpaceId { fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "{}/{}", self.library, self.wiki) } }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentType {
    Article, SmartTable, Workflow, Flowchart, Mindmap, Audio, Video, Artwork, Playlist,
}
impl ContentType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Article => "article", Self::SmartTable => "smart_table", Self::Workflow => "workflow",
            Self::Flowchart => "flowchart", Self::Mindmap => "mindmap", Self::Audio => "audio",
            Self::Video => "video", Self::Artwork => "artwork", Self::Playlist => "playlist",
        }
    }
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "article" => Some(Self::Article), "smart_table" => Some(Self::SmartTable), "workflow" => Some(Self::Workflow),
            "flowchart" => Some(Self::Flowchart), "mindmap" => Some(Self::Mindmap), "audio" => Some(Self::Audio),
            "video" => Some(Self::Video), "artwork" => Some(Self::Artwork), "playlist" => Some(Self::Playlist),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentStatus { Active, Frozen, Hidden, Archived }
impl DocumentStatus {
    pub fn is_readable(&self) -> bool { matches!(self, Self::Active | Self::Frozen) }
    pub fn is_writable(&self) -> bool { matches!(self, Self::Active) }
    pub fn is_searchable(&self) -> bool { matches!(self, Self::Active | Self::Frozen) }
    pub fn from_str(s: &str) -> Option<Self> {
        match s { "active" => Some(Self::Active), "frozen" => Some(Self::Frozen), "hidden" => Some(Self::Hidden), "archived" => Some(Self::Archived), _ => None }
    }
    pub fn as_str(&self) -> &'static str {
        match self { Self::Active => "active", Self::Frozen => "frozen", Self::Hidden => "hidden", Self::Archived => "archived" }
    }
}

// =============================================================================
// Frontmatter
// =============================================================================

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Frontmatter {
    pub title: Option<String>,
    pub content_type: Option<String>,
    pub tags: Option<Vec<String>>,
    pub status: Option<String>,
    pub path: Option<String>,
    pub author: Option<String>,
    pub icon: Option<String>,
    pub cover: Option<String>,
}

pub fn parse_frontmatter(raw: &str) -> (Frontmatter, String) {
    let trimmed = raw.trim_start();
    if !trimmed.starts_with("---\n") && !trimmed.starts_with("---\r\n") {
        let title = raw.lines().find(|l| l.starts_with('#')).map(|l| l.trim_start_matches('#').trim().to_string());
        return (Frontmatter { title, ..Default::default() }, raw.to_string());
    }
    let after_first = &trimmed[3..].trim_start();
    if let Some(end_idx) = after_first.find("\n---") {
        let yaml_str = &after_first[..end_idx];
        let body = after_first[end_idx + 4..].trim_start().to_string();
        let fm: Frontmatter = serde_yaml::from_str(yaml_str).unwrap_or_default();
        (fm, body)
    } else {
        (Frontmatter::default(), raw.to_string())
    }
}

// =============================================================================
// DerivationChain
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DerivationChain {
    pub l2_hash: String,
    pub l1_rule: Option<DerivationRule>,
    pub l0_rule: Option<DerivationRule>,
    pub last_derived: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DerivationRule { Llm { prompt_template: String, model: String }, Extractive { algorithm: String }, Manual }

impl DerivationChain {
    pub fn new(l2_content: &str) -> Self {
        Self { l2_hash: blake3::hash(l2_content.as_bytes()).to_hex().to_string(), l1_rule: None, l0_rule: None, last_derived: Utc::now() }
    }
    pub fn is_stale(&self, current_l2: &str) -> bool {
        blake3::hash(current_l2.as_bytes()).to_hex().to_string() != self.l2_hash
    }
    pub fn set_l1(&mut self, rule: DerivationRule) { self.l1_rule = Some(rule); self.last_derived = Utc::now(); }
    pub fn set_l0(&mut self, rule: DerivationRule) { self.l0_rule = Some(rule); self.last_derived = Utc::now(); }
}

// =============================================================================
// Document
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Document {
    pub id: DocId, pub title: String, pub space_id: SpaceId,
    pub content_type: ContentType, pub status: DocumentStatus, pub path: Option<String>,
    pub version: u64, pub tags: Vec<String>, pub icon: Option<String>, pub cover: Option<String>,
    pub author: String, pub created_at: DateTime<Utc>, pub updated_at: DateTime<Utc>,
    pub raw_markdown: String, pub blocks: Vec<Block>,
    pub summary: Option<String>, pub overview: Option<String>,
    pub derivation: Option<DerivationChain>,
}

impl Document {
    pub fn parse(raw: impl Into<String>, space_id: SpaceId) -> Result<Self> {
        let raw: String = raw.into();
        let (fm, body) = parse_frontmatter(&raw);
        let content_type = fm.content_type.as_deref().and_then(ContentType::from_str).unwrap_or(ContentType::Article);
        let status = fm.status.as_deref().and_then(DocumentStatus::from_str).unwrap_or(DocumentStatus::Active);
        let blocks = parse_markdown_to_blocks(&body)?;
        let deriv = DerivationChain::new(&body);
        let now = Utc::now();
        Ok(Self {
            id: DocId::new(), space_id, title: fm.title.unwrap_or_else(|| "Untitled".into()),
            content_type, status, path: fm.path, version: 0,
            tags: fm.tags.unwrap_or_default(), icon: fm.icon, cover: fm.cover,
            author: fm.author.unwrap_or_else(|| "unknown".into()),
            created_at: now, updated_at: now,
            raw_markdown: raw, blocks,
            summary: None, overview: None,
            derivation: Some(deriv),
        })
    }

    pub fn new(title: impl Into<String>, content_type: ContentType, raw_markdown: impl Into<String>, space_id: SpaceId, author: impl Into<String>) -> Result<Self> {
        let raw: String = raw_markdown.into();
        let blocks = parse_markdown_to_blocks(&raw)?;
        let deriv = DerivationChain::new(&raw);
        let now = Utc::now();
        Ok(Self {
            id: DocId::new(), title: title.into(), space_id, content_type,
            status: DocumentStatus::Active, path: None, version: 0,
            tags: Vec::new(), icon: None, cover: None, author: author.into(),
            created_at: now, updated_at: now,
            raw_markdown: raw, blocks,
            summary: None, overview: None,
            derivation: Some(deriv),
        })
    }

    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        out.push_str("---\n");
        out.push_str(&format!("title: {}\n", self.title));
        out.push_str(&format!("content_type: {}\n", self.content_type.as_str()));
        if !self.tags.is_empty() { out.push_str(&format!("tags: [{}]\n", self.tags.join(", "))); }
        out.push_str(&format!("status: {}\n", self.status.as_str()));
        if let Some(ref p) = self.path { out.push_str(&format!("path: {}\n", p)); }
        out.push_str(&format!("author: {}\n", self.author));
        out.push_str(&format!("created_at: {}\n", self.created_at.to_rfc3339()));
        out.push_str("---\n\n");
        for b in &self.blocks { out.push_str(&b.to_diffable_text()); }
        out
    }

    pub fn reparse(&mut self) -> Result<()> {
        let (_, body) = parse_frontmatter(&self.raw_markdown);
        self.blocks = parse_markdown_to_blocks(&body)?;
        self.updated_at = Utc::now();
        Ok(())
    }

    pub fn block(&self, id: &BlockId) -> Option<&Block> { block::find_block(&self.blocks, id) }
    pub fn block_mut(&mut self, id: &BlockId) -> Option<&mut Block> { block::find_block_mut(&mut self.blocks, id) }
    pub fn children_of(&self, parent: &BlockId) -> Vec<&Block> { block::children_of(&self.blocks, parent) }
    pub fn descendants(&self, root: &BlockId) -> Vec<BlockId> { block::descendants(&self.blocks, root) }

    pub fn body_owned(&self) -> String {
        let raw = self.raw_markdown.clone();
        let (_, body) = parse_frontmatter(&raw);
        body
    }

    pub fn set_summary(&mut self, text: String, rule: DerivationRule) {
        self.summary = Some(text);
        if let Some(ref mut d) = self.derivation { d.set_l0(rule); }
    }
    pub fn set_overview(&mut self, text: String, rule: DerivationRule) {
        self.overview = Some(text);
        if let Some(ref mut d) = self.derivation { d.set_l1(rule); }
    }
    pub fn is_derivation_stale(&self) -> bool {
        let body = self.body_owned();
        self.derivation.as_ref().map_or(true, |d| d.is_stale(&body))
    }

    pub fn walk(&self, root: &BlockId, f: &mut impl FnMut(&Block)) {
        if let Some(b) = self.block(root) { f(b); for child in b.children().to_vec() { self.walk(&child, f); } }
    }
}

// =============================================================================
// Op
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Op {
    TextUpdate { patch: String, base_version: u64 },
    BlockUpdate { block_id: BlockId, patch: serde_json::Value },
    InsertBlock { after: Option<BlockId>, block: Block },
    DeleteBlock { block_id: BlockId },
    MoveBlock { id: BlockId, new_parent: BlockId, after: Option<BlockId> },
    UpdateMeta { patch: serde_json::Value },
}

// =============================================================================
// Markdown → Block 解析
// =============================================================================

pub fn parse_markdown_to_blocks(md: &str) -> Result<Vec<Block>> {
    let node = markdown::to_mdast(md, &markdown::ParseOptions::gfm())
        .map_err(|e| WikiError::Invalid(format!("markdown parse: {e}")))?;
    let ast = format!("{:?}", node);
    let ast_value: serde_json::Value = serde_json::from_str(&ast)
        .map_err(|e| WikiError::Invalid(format!("serialize ast: {e}")))?;
    let mut blocks = Vec::new();
    flatten_ast_to_blocks(&ast_value, md, &mut blocks, "system")?;
    Ok(blocks)
}

fn flatten_ast_to_blocks(node: &serde_json::Value, raw_source: &str, out: &mut Vec<Block>, author: &str) -> Result<()> {
    let nt = node.get("type").and_then(|v| v.as_str()).unwrap_or("unknown");
    match nt {
        "html" => {
            let hv = node.get("value").and_then(|v| v.as_str()).unwrap_or("");
            if let Some((ty, data)) = parse_custom_component(hv) {
                out.push(Block::custom(ty, data, hv, author));
            } else {
                out.push(Block::markdown(node.clone(), "html", extract_raw_text(raw_source, node), author));
            }
        }
        "heading" | "paragraph" | "code" | "list" | "listItem" | "blockquote" | "thematicBreak"
        | "table" | "tableRow" | "tableCell" | "image" | "link" | "emphasis" | "strong"
        | "inlineCode" | "text" | "break" | "delete" | "definition" | "footnoteDefinition" | "footnoteReference" => {
            out.push(Block::markdown(node.clone(), nt, extract_raw_text(raw_source, node), author));
        }
        _ => {}
    }
    if let Some(children) = node.get("children").and_then(|v| v.as_array()) {
        for child in children { flatten_ast_to_blocks(child, raw_source, out, author)?; }
    }
    Ok(())
}

fn extract_raw_text(source: &str, node: &serde_json::Value) -> String {
    if let (Some(s), Some(e)) = (
        node.get("position").and_then(|p| p.get("start").and_then(|x| x.get("offset"))).and_then(|v| v.as_u64()),
        node.get("position").and_then(|p| p.get("end").and_then(|x| x.get("offset"))).and_then(|v| v.as_u64()),
    ) {
        if s < e && (e as usize) <= source.len() { return source[s as usize..e as usize].to_string(); }
    }
    String::new()
}

pub fn parse_custom_component(html: &str) -> Option<(CustomBlockType, serde_json::Value)> {
    let html = html.trim();
    if !html.starts_with('<') || !html.ends_with("/>") { return None; }
    let inner = &html[1..html.len()-2].trim();
    let mut parts = inner.splitn(2, char::is_whitespace);
    let tag = parts.next()?;
    let ty = CustomBlockType::from_str(tag);
    if matches!(ty, CustomBlockType::Custom(_)) { return None; }
    Some((ty, parse_attrs(parts.next().unwrap_or(""))))
}

fn parse_attrs(a: &str) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for part in a.split_whitespace() {
        if let Some(eq) = part.find('=') {
            map.insert(part[..eq].to_string(), serde_json::Value::String(part[eq+1..].trim_matches('"').trim_matches('\'').trim_end_matches('/').to_string()));
        }
    }
    serde_json::Value::Object(map)
}
