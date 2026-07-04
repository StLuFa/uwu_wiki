//! # wiki-llm
//!
//! LLM 横切层 —— **领域无关**。只认 [`TextUnit`]，不 `use` 文档/表格/图具体类型。
//! LLM 后端由注入的 [`LlmClient`] 提供，本 crate 不依赖 agent-core。
//!
//! ## 快速上手
//!
//! ```ignore
//! use wiki_llm::{DefaultLlmEngine, LlmCapability, MockLlmClient};
//! use wiki_testkit::MemoryWikiStorage;
//! use wiki_llm::mock::AllowAllPermissionFilter;
//!
//! let storage = MemoryWikiStorage::new();
//! let engine = DefaultLlmEngine::new(
//!     Arc::new(MockLlmClient::new()),
//!     storage.vector_store(),
//!     storage.text_index(),
//!     storage.doc_store(),
//!     Arc::new(AllowAllPermissionFilter),
//! );
//!
//! // 语义搜索
//! let results = engine.search("rust async", 5).await?;
//!
//! // RAG 问答
//! let answer = engine.qa("什么是 async/await?", None).await?;
//! ```

pub mod embed;
pub mod engine;
pub mod mock;
pub mod rag;

pub use embed::{diff_embed, DiffEmbedResult, EmbedUnit, EmbeddingCache};
pub use engine::DefaultLlmEngine;
pub use mock::{AllowAllPermissionFilter, DenyListPermissionFilter, MockLlmClient};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use wiki_core::Result;

// ===========================================================================
// LlmClient —— 依赖注入的 LLM 后端抽象（复用 agent-context-db 同名抽象语义）
// ===========================================================================

#[derive(Debug, Clone, Default)]
pub struct LlmOpts {
    pub max_tokens: Option<usize>,
    pub temperature: Option<f32>,
    pub model: Option<String>,
}

/// LLM 调用后端。由宿主在构造期注入，wiki-llm 不持有具体实现。
#[async_trait]
pub trait LlmClient: Send + Sync {
    async fn complete(&self, prompt: &str, opts: &LlmOpts) -> Result<String>;
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
}

// ===========================================================================
// TextUnit —— 领域无关文本单元
// ===========================================================================

/// 三类实体（Block / 表格行 / 图节点）统一适配成它。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextUnit {
    /// 领域实体 ID 的字符串化。
    pub id: String,
    /// 待处理文本。
    pub text: String,
    /// 溯源路径（doc→block / table→row / graph→node）。
    pub path: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QaAnswer {
    pub answer: String,
    /// 引用来源的 TextUnit id。
    pub citations: Vec<String>,
}

// ===========================================================================
// LlmCapability —— 领域无关能力端口
// ===========================================================================

/// 领域无关的 LLM 能力集。各领域 crate 把自身实体适配为 `TextUnit` 后调用。
///
/// 默认实现在 [`DefaultLlmEngine`]；测试用 mock 见 [`MockLlmClient`]。
///
/// **权限说明**：`search`/`qa` 使用系统级权限（Owner 角色），无访问控制。
/// 需要用户级权限过滤时，使用 [`DefaultLlmEngine::search_with_permission`]
/// 或 [`DefaultLlmEngine::qa_with_permission`]。
#[async_trait]
pub trait LlmCapability: Send + Sync {
    /// 批量生成 embedding。
    async fn embed(&self, units: &[TextUnit]) -> Result<Vec<Vec<f32>>>;
    /// 语义 + 全文混合检索（系统级权限）。
    async fn search(&self, query: &str, top_k: usize) -> Result<Vec<(TextUnit, f32)>>;
    /// 行内补全（基于单位内容续写）。
    async fn complete(&self, unit: &TextUnit, partial: &str) -> Result<String>;
    /// RAG 问答：检索 → 构建上下文 → LLM 回答（系统级权限）。
    async fn qa(&self, question: &str, scope_root: Option<&str>) -> Result<QaAnswer>;
    /// 批量摘要。
    async fn summarize(&self, units: &[TextUnit]) -> Result<String>;
}

/// 增量 embedding：仅重算内容版本落后的单元（配合 wiki-core 陈旧检测 #8）。
///
/// 输入三元组 `(unit, content_version, embedding_version)`，
/// 返回 `embedding_version < content_version` 的 unit id。
pub fn stale_unit_ids(units: &[(TextUnit, u64, u64)]) -> Vec<&str> {
    units
        .iter()
        .filter(|(_, cv, ev)| ev < cv)
        .map(|(u, _, _)| u.id.as_str())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_stale_units() {
        let u = TextUnit {
            id: "b1".into(),
            text: "x".into(),
            path: vec![],
        };
        let units = vec![(u.clone(), 3u64, 1u64), (u, 2u64, 2u64)];
        let stale = stale_unit_ids(&units);
        assert_eq!(stale, vec!["b1"]);
    }
}
