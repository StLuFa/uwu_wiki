//! Block 类型注册表 + 渲染 trait。

use crate::block::{Block, BlockType};
use crate::doc::Document;
use std::collections::HashMap;

/// 自定义 Block 类型注册表。核心不硬编码具体类型。
#[derive(Default)]
pub struct BlockTypeRegistry {
    custom: HashMap<String, CustomBlockSpec>,
}

/// 自定义类型规格（骨架）。
pub struct CustomBlockSpec {
    pub name: String,
    /// 校验内容是否合法。
    pub validate: fn(&serde_json::Value) -> bool,
}

impl BlockTypeRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, spec: CustomBlockSpec) {
        self.custom.insert(spec.name.clone(), spec);
    }

    pub fn is_registered(&self, name: &str) -> bool {
        self.custom.contains_key(name)
    }

    /// 校验一个 Block 的内容对其类型是否合法。
    pub fn validate(&self, block: &Block) -> bool {
        match &block.ty {
            BlockType::Custom(name) => self
                .custom
                .get(name)
                .map(|spec| (spec.validate)(&block.content.0))
                .unwrap_or(false),
            // 内置类型骨架阶段一律通过。
            _ => true,
        }
    }
}

/// Block → 目标格式渲染 trait。
pub trait Render {
    fn render_markdown(&self, block: &Block) -> String;
}

/// 默认 Markdown 渲染器。
pub struct MarkdownRenderer;

impl MarkdownRenderer {
    /// 渲染单个 Block（不含子块）。
    fn render_one(&self, block: &Block) -> String {
        let text = block.content.as_plain_text();
        match &block.ty {
            BlockType::Heading => {
                // 支持 content.level（1-6），缺省 h1。
                let level = block
                    .content
                    .0
                    .get("level")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(1)
                    .clamp(1, 6) as usize;
                format!("{} {text}", "#".repeat(level))
            }
            BlockType::Quote => format!("> {text}"),
            BlockType::Code => {
                let lang = block
                    .content
                    .0
                    .get("lang")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                format!("```{lang}\n{text}\n```")
            }
            BlockType::BulletedList => format!("- {text}"),
            BlockType::NumberedList => format!("1. {text}"),
            BlockType::Divider => "---".to_string(),
            BlockType::Callout => format!("> [!NOTE]\n> {text}"),
            _ => text,
        }
    }

    /// 渲染整篇文档：从 `doc.root` 前序遍历，按深度缩进子块。
    pub fn render_document(&self, doc: &Document) -> String {
        let mut out = String::new();
        self.render_subtree(doc, &doc.root, 0, &mut out);
        out.trim_end().to_string()
    }

    fn render_subtree(
        &self,
        doc: &Document,
        id: &crate::block::BlockId,
        depth: usize,
        out: &mut String,
    ) {
        if let Some(block) = doc.block(id) {
            let rendered = self.render_one(block);
            // 顶层不缩进；子层每级两空格。
            let indent = "  ".repeat(depth.saturating_sub(1));
            for line in rendered.lines() {
                if depth > 0 {
                    out.push_str(&indent);
                }
                out.push_str(line);
                out.push('\n');
            }
            for child in &block.children {
                self.render_subtree(doc, child, depth + 1, out);
            }
        }
    }
}

impl Render for MarkdownRenderer {
    fn render_markdown(&self, block: &Block) -> String {
        self.render_one(block)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::{Block, BlockContent, BlockType};
    use crate::doc::{Document, Op, SpaceId};

    #[test]
    fn renders_heading_levels() {
        let r = MarkdownRenderer;
        let mut b = Block::new(BlockType::Heading, BlockContent::text("Title"), "a");
        b.content.0 = serde_json::json!({ "text": "Title", "level": 3 });
        assert_eq!(r.render_markdown(&b), "### Title");
    }

    #[test]
    fn renders_code_with_lang() {
        let r = MarkdownRenderer;
        let mut b = Block::new(BlockType::Code, BlockContent::text("fn main(){}"), "a");
        b.content.0 = serde_json::json!({ "text": "fn main(){}", "lang": "rust" });
        assert_eq!(r.render_markdown(&b), "```rust\nfn main(){}\n```");
    }

    #[test]
    fn renders_document_tree_with_indent() {
        let root = Block::new(BlockType::Paragraph, BlockContent::text("root"), "a");
        let root_id = root.id.clone();
        let mut doc = Document::new("D", root, SpaceId::default());

        let mut heading = Block::new(BlockType::Heading, BlockContent::text("H"), "a");
        heading.content.0 = serde_json::json!({ "text": "H", "level": 2 });
        let hid = heading.id.clone();
        doc.apply(Op::InsertBlock { parent: root_id.clone(), after: None, block: heading }).unwrap();

        let bullet = Block::new(BlockType::BulletedList, BlockContent::text("item"), "a");
        doc.apply(Op::InsertBlock { parent: hid, after: None, block: bullet }).unwrap();

        let md = r_render(&doc);
        assert!(md.contains("root"));
        assert!(md.contains("## H"));
        assert!(md.contains("- item"));
    }

    fn r_render(doc: &Document) -> String {
        MarkdownRenderer.render_document(doc)
    }
}
