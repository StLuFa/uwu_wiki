//! Markdown → Block 树解析器。
//!
//! 将 Markdown 文本转换为 wiki-core 的 [`Block`] / [`Document`] 结构，
//! 与现有的 [`MarkdownRenderer`](crate::registry::MarkdownRenderer) 形成双向转换。

use crate::block::{Block, BlockContent, BlockType};
use crate::doc::{Document, SpaceId};

/// Markdown 解析器。
///
/// ## 支持的语法
/// - `# Title` / `## H2` … — Heading（自动从标题推断文档标题）
/// - 空行分隔段落 — Paragraph
/// - `- item` / `* item` — BulletedList
/// - `> text` — Quote
/// - ` ```lang\ncode\n``` ` — Code（提取 lang）
/// - `---` — Divider
/// - 其他文本 → Paragraph
pub struct MarkdownParser;

impl MarkdownParser {
    /// 解析整个文档，返回以第一个 Heading 为标题的 [`Document`]。
    /// 若文档无标题，取前三行首行或默认 `"Imported"`。
    pub fn parse_document(markdown: &str, space_id: SpaceId) -> Document {
        let blocks = Self::parse_blocks(markdown);

        // 尝试从第一个 Heading 推断标题。
        let title = blocks
            .iter()
            .find(|b| matches!(b.ty, BlockType::Heading))
            .map(|b| b.content.as_plain_text())
            .or_else(|| {
                markdown
                    .lines()
                    .next()
                    .map(|l| l.trim().trim_start_matches('#').trim().to_string())
            })
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| "Imported".into());

        let root = Block::new(BlockType::Paragraph, BlockContent::text(&title), "markdown-import");
        let root_id = root.id.clone();

        let blocks: Vec<Block> = std::iter::once(root.clone())
            .chain(blocks.into_iter().map(|mut b| {
                // 让每个子块指向 root 作为 parent。
                b.parent = Some(root_id.clone());
                b
            }))
            .collect();

        // 建立根块的 children 顺序（先收集再设置，避免借用冲突）。
        let child_ids: Vec<_> = blocks
            .iter()
            .filter(|b| b.id != root_id)
            .map(|b| b.id.clone())
            .collect();

        let mut doc = Document::new(title, root, space_id);
        doc.blocks = blocks;
        if let Some(r) = doc.block_mut(&root_id) {
            r.children = child_ids;
        }
        doc
    }

    /// 解析 Markdown 文本为扁平 Block 列表（不含根块）。
    pub fn parse_blocks(markdown: &str) -> Vec<Block> {
        let mut blocks = Vec::new();
        let lines: Vec<&str> = markdown.lines().collect();
        let mut i = 0;

        while i < lines.len() {
            let line = lines[i];

            // 空行跳过。
            if line.trim().is_empty() {
                i += 1;
                continue;
            }

            // 代码块：``` ... ```
            if line.trim_start().starts_with("```") {
                let lang = line.trim_start().trim_start_matches("```").trim().to_string();
                let mut code_lines: Vec<&str> = Vec::new();
                i += 1;
                while i < lines.len() && !lines[i].trim_start().starts_with("```") {
                    code_lines.push(lines[i]);
                    i += 1;
                }
                i += 1; // 跳过结尾 ```
                let mut content = serde_json::json!({"text": code_lines.join("\n")});
                if !lang.is_empty() {
                    content["lang"] = serde_json::Value::String(lang);
                }
                blocks.push(Block::new(
                    BlockType::Code,
                    BlockContent(content),
                    "markdown-import",
                ));
                continue;
            }

            // Heading: # / ## / ### …
            if line.starts_with('#') {
                let level = line.chars().take_while(|c| *c == '#').count().min(6);
                let text = line[level..].trim().to_string();
                let content = serde_json::json!({"text": text, "level": level});
                blocks.push(Block::new(
                    BlockType::Heading,
                    BlockContent(content),
                    "markdown-import",
                ));
                i += 1;
                continue;
            }

            // Divider: --- / ***
            if line.trim() == "---" || line.trim() == "***" {
                blocks.push(Block::new(
                    BlockType::Divider,
                    BlockContent::text(""),
                    "markdown-import",
                ));
                i += 1;
                continue;
            }

            // Unordered list: - item / * item
            if let Some(rest) = line
                .strip_prefix("- ")
                .or_else(|| line.strip_prefix("* "))
                .or_else(|| line.strip_prefix("+ "))
            {
                blocks.push(Block::new(
                    BlockType::BulletedList,
                    BlockContent::text(rest.trim()),
                    "markdown-import",
                ));
                i += 1;
                continue;
            }

            // Ordered list: 1. item
            if line
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .count()
                > 0
                && line.trim_start().chars().skip_while(|c| c.is_ascii_digit()).take(2).collect::<String>()
                    == ". "
            {
                let text = line
                    .trim_start()
                    .trim_start_matches(|c: char| c.is_ascii_digit())
                    .trim_start_matches(". ")
                    .to_string();
                blocks.push(Block::new(
                    BlockType::NumberedList,
                    BlockContent::text(text),
                    "markdown-import",
                ));
                i += 1;
                continue;
            }

            // Blockquote: > text
            if line.starts_with("> ") {
                blocks.push(Block::new(
                    BlockType::Quote,
                    BlockContent::text(line[2..].trim()),
                    "markdown-import",
                ));
                i += 1;
                continue;
            }

            // Callout: > [!NOTE]
            if line.starts_with("> [!") {
                let text = line[2..].trim().to_string();
                blocks.push(Block::new(
                    BlockType::Callout,
                    BlockContent::text(text),
                    "markdown-import",
                ));
                i += 1;
                continue;
            }

            // 默认：段落。合并连续非空行。
            let mut para_lines = Vec::new();
            while i < lines.len() && !lines[i].trim().is_empty() {
                para_lines.push(lines[i]);
                i += 1;
            }
            blocks.push(Block::new(
                BlockType::Paragraph,
                BlockContent::text(para_lines.join("\n")),
                "markdown-import",
            ));
        }

        blocks
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_heading() {
        let blocks = MarkdownParser::parse_blocks("# Hello World");
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].ty, BlockType::Heading);
        let text = blocks[0].content.as_plain_text();
        assert_eq!(text, "Hello World");
        assert_eq!(blocks[0].content.0["level"], 1);
    }

    #[test]
    fn parses_code_block_with_lang() {
        let md = "```rust\nfn main() {}\n```";
        let blocks = MarkdownParser::parse_blocks(md);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].ty, BlockType::Code);
        assert_eq!(blocks[0].content.0["lang"], "rust");
    }

    #[test]
    fn parses_mixed_content() {
        let md = "# Title\n\nSome paragraph text.\n\n- item 1\n- item 2\n\n> quote line";
        let blocks = MarkdownParser::parse_blocks(md);
        assert_eq!(blocks.len(), 5);
        assert_eq!(blocks[0].ty, BlockType::Heading);
        assert_eq!(blocks[1].ty, BlockType::Paragraph);
        assert_eq!(blocks[2].ty, BlockType::BulletedList);
        assert_eq!(blocks[3].ty, BlockType::BulletedList);
        assert_eq!(blocks[4].ty, BlockType::Quote);
    }

    #[test]
    fn parses_divider() {
        let blocks = MarkdownParser::parse_blocks("---");
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].ty, BlockType::Divider);
    }

    #[test]
    fn parses_document_with_title() {
        let doc = MarkdownParser::parse_document("# Rust Guide\n\nContent here.", SpaceId::default());
        assert_eq!(doc.title, "Rust Guide");
        assert!(doc.blocks.len() > 1); // root + at least heading
    }

    #[test]
    fn handles_empty_input() {
        let blocks = MarkdownParser::parse_blocks("");
        assert!(blocks.is_empty());
    }
}
