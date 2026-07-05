//! # wiki-workflow
//!
//! LLM Wiki 工作流：Ingest（原料→知识）/ Query（问答→反写）/ Lint（审计）。
//! LLM 是 wiki 的全职编辑 —— 知识随时间复利增长。
//!
//! ## 三管线
//!
//! | 管线 | 入口 | 说明 |
//! |---|---|---|
//! | Ingest | [`IngestPipeline::run`] | 消化外部原料，自动更新/创建 wiki 页面 |
//! | Query | [`QueryPipeline::ask`] / [`QueryPipeline::ask_and_write_back`] | RAG 问答 + 可选知识反写 |
//! | Lint | [`WikiLinter::run`] | 全量健康检查（孤页/断链/重复/过时/矛盾） |
//!
//! ## 快速上手
//!
//! ```ignore
//! use wiki_workflow::{WikiDomain, IngestSource};
//!
//! let domain = WikiDomain::new(llm_capability, wiki_space);
//!
//! // 消化原料
//! let result = domain.ingest(&IngestSource {
//!     content: "Rust 的 async/await 基于 Future trait...".into(),
//!     title: Some("Rust Async".into()),
//!     source_url: None,
//! }).await?;
//!
//! // 问答
//! let answer = domain.query("什么是 async?").await?;
//!
//! // 审计
//! let report = domain.lint().await?;
//! ```

pub mod ingest;
pub mod lint;
pub mod query;

use std::sync::Arc;
use wiki_core::{Result, WikiSpace};
use wiki_llm::LlmCapability;

// Re-export key types for convenience.
pub use ingest::{Contradiction, ContradictionSeverity, IngestPipeline, IngestSource, IngestResult};
pub use lint::{LintConfig, LintReport, WikiLinter};
pub use query::{QueryPipeline, QueryResult, WriteBackPolicy, WriteBackResult};

// ===========================================================================
// WikiDomain —— 三管线统一入口
// ===========================================================================

/// 工作流域。注入的 [`LlmCapability`] 端口 + [`WikiSpace`] 存储，不 `use` 任何 LLM 引擎具体类型。
pub struct WikiDomain {
    llm: Arc<dyn LlmCapability>,
    space: Arc<WikiSpace>,
}

impl WikiDomain {
    pub fn new(llm: Arc<dyn LlmCapability>, space: Arc<WikiSpace>) -> Self {
        Self { llm, space }
    }

    /// 消化新原料进 wiki。
    ///
    /// 委托给 [`IngestPipeline`]，详见 [`ingest`] 模块文档。
    pub async fn ingest(&self, source: &IngestSource) -> Result<IngestResult> {
        let pipeline = IngestPipeline::new(self.llm.clone(), self.space.clone());
        pipeline.run(source).await
    }

    /// RAG 问答（不反写）。
    ///
    /// 委托给 [`QueryPipeline::ask`]。
    pub async fn query(&self, question: &str) -> Result<QueryResult> {
        let pipeline = QueryPipeline::new(self.llm.clone(), self.space.clone());
        pipeline.ask(question).await
    }

    /// RAG 问答 + 按策略反写。
    ///
    /// 委托给 [`QueryPipeline::ask_and_write_back`]。
    pub async fn query_with_write_back(
        &self,
        question: &str,
        policy: &WriteBackPolicy,
    ) -> Result<QueryResult> {
        let pipeline = QueryPipeline::new(self.llm.clone(), self.space.clone());
        pipeline.ask_and_write_back(question, policy).await
    }

    /// 触发全量审计。
    ///
    /// 委托给 [`WikiLinter::run`]，使用默认 [`LintConfig`]。
    pub async fn lint(&self) -> Result<LintReport> {
        let linter = WikiLinter::new(
            self.llm.clone(),
            self.space.clone(),
            LintConfig::default(),
        );
        linter.run().await
    }

    /// 触发审计（自定义配置）。
    pub async fn lint_with_config(&self, config: LintConfig) -> Result<LintReport> {
        let linter = WikiLinter::new(self.llm.clone(), self.space.clone(), config);
        linter.run().await
    }

    /// 获取内部 LlmCapability 引用。
    pub fn llm(&self) -> &Arc<dyn LlmCapability> {
        &self.llm
    }

    /// 获取内部 WikiSpace 引用。
    pub fn space(&self) -> &Arc<WikiSpace> {
        &self.space
    }
}

// ===========================================================================
// 测试
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use wiki_core::{SpaceId, WikiStorage};
    use wiki_llm::mock::MockLlmClient;
    use wiki_llm::DefaultLlmEngine;
    use wiki_testkit::MemoryWikiStorage;

    fn build_domain() -> WikiDomain {
        let storage = MemoryWikiStorage::new();
        let mock_llm = Arc::new(
            MockLlmClient::with_deterministic_embed(8).with_fixed_complete(
                r#"{"entities":["Rust"],"concepts":["async"],"claims":["Rust supports async"],"summary":"Rust async programming."}"#,
            ),
        );
        let engine = Arc::new(DefaultLlmEngine::new(
            mock_llm,
            storage.vector_store(),
            storage.text_index(),
            storage.doc_store(),
            Arc::new(wiki_llm::mock::AllowAllPermissionFilter),
        ));
        let space = Arc::new(WikiSpace::new(SpaceId::default(), Arc::new(storage)));
        WikiDomain::new(engine, space)
    }

    #[tokio::test]
    async fn ingest_via_domain() {
        let domain = build_domain();
        let result = domain
            .ingest(&IngestSource {
                content: "Rust async 编程指南".into(),
                title: Some("Rust Async".into()),
                source_url: None,
            })
            .await
            .unwrap();
        // 摘要文档应被创建。
        assert!(result.summary_doc_id.is_some());
    }

    #[tokio::test]
    async fn query_via_domain() {
        let domain = build_domain();
        let result = domain.query("什么是 async?").await.unwrap();
        assert!(!result.answer.is_empty());
        assert!(result.write_back.is_none());
    }

    #[tokio::test]
    async fn lint_via_domain() {
        let domain = build_domain();
        let report = domain.lint().await.unwrap();
        // 空知识库。
        assert_eq!(report.total_pages, 0);
    }

    #[tokio::test]
    async fn domain_exposes_llm_and_space() {
        let domain = build_domain();
        let _ = domain.llm();
        let _ = domain.space();
    }
}
