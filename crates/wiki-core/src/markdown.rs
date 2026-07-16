//! Markdown 渲染器 — 基于 markdown-rs 的薄封装。

use crate::error::Result;

/// 解析 Markdown 为 AST JSON。
pub fn parse_to_ast(md: &str) -> Result<serde_json::Value> {
    let node = markdown::to_mdast(md, &markdown::ParseOptions::gfm())
        .map_err(|e| crate::error::WikiError::Invalid(format!("markdown parse: {e}")))?;
    // mdast::Node doesn't impl Serialize, so convert via Debug/Display
    let json_str = format!("{:?}", node);
    serde_json::from_str(&json_str)
        .map_err(|e| crate::error::WikiError::Invalid(format!("serialize ast: {e}")))
}

/// Markdown → HTML。
pub fn md_to_html(md: &str) -> String {
    markdown::to_html(md)
}

/// AST → HTML（通过原始 Markdown 重新渲染）。
pub fn ast_to_html(raw_md: &str) -> String {
    markdown::to_html(raw_md)
}

/// AST JSON → 纯文本（递归遍历文本节点提取）。
pub fn ast_to_string(input: &str) -> Option<String> {
    let ast: serde_json::Value = serde_json::from_str(input).ok()?;
    Some(extract_text(&ast))
}

fn extract_text(node: &serde_json::Value) -> String {
    match node.get("type").and_then(|v| v.as_str()) {
        Some("text") => node.get("value").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        Some("inlineCode") => format!("`{}`", node.get("value").and_then(|v| v.as_str()).unwrap_or("")),
        _ => {
            let mut s = String::new();
            if let Some(children) = node.get("children").and_then(|v| v.as_array()) {
                for child in children { s.push_str(&extract_text(child)); }
            }
            s
        }
    }
}
