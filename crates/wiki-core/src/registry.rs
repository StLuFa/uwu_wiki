//! BlockType 注册表与渲染器 trait。
//!
//! 自定义 Block 类型通过注册表映射到渲染器。

use crate::block::Block;
use crate::error::Result;
use std::collections::HashMap;

/// 渲染 trait — 将 Block 渲染为 HTML 字符串。
pub trait Render: Send + Sync {
    fn render(&self, block: &Block) -> Result<String>;
}

/// Markdown 渲染器 — 给文本类 Block。
pub struct MarkdownRenderer;

impl Render for MarkdownRenderer {
    fn render(&self, block: &Block) -> Result<String> {
        match block {
            Block::Markdown(b) => Ok(crate::markdown::ast_to_html(&b.raw)),
            Block::Custom(_) => Ok(String::new()), // Custom blocks handled by their own renderers
        }
    }
}

/// BlockType 注册表 — CustomBlockType → Renderer 映射。
#[derive(Default)]
pub struct BlockTypeRegistry {
    renderers: HashMap<String, Box<dyn Render>>,
}

impl BlockTypeRegistry {
    pub fn new() -> Self {
        let mut reg = Self { renderers: HashMap::new() };
        // 默认注册 Markdown 渲染器
        reg.register("markdown", Box::new(MarkdownRenderer));
        reg
    }

    pub fn register(&mut self, name: &str, renderer: Box<dyn Render>) {
        self.renderers.insert(name.to_string(), renderer);
    }

    pub fn get(&self, name: &str) -> Option<&dyn Render> {
        self.renderers.get(name).map(|r| r.as_ref())
    }

    pub fn render(&self, block: &Block) -> Result<String> {
        match block {
            Block::Markdown(_) => {
                self.get("markdown")
                    .ok_or_else(|| crate::error::WikiError::Invalid("no markdown renderer".into()))?
                    .render(block)
            }
            Block::Custom(b) => {
                let name = b.ty.as_str();
                self.get(name)
                    .ok_or_else(|| crate::error::WikiError::Invalid(format!("no renderer for {name}")))?
                    .render(block)
            }
        }
    }
}
