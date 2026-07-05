//! `LlmCapability` 的默认实现：依赖注入 5 个端口，完成完整的 LLM 能力。
//!
//! # 注入端口
//!
//! | 端口 | 来源 | 用途 |
//! |---|---|---|
//! | `LlmClient` | 宿主（agent-core 或 mock） | LLM 推理 |
//! | `VectorStore` | 宿主（Qdrant / 内存） | 语义检索 |
//! | `TextIndex` | 宿主（PG tsvector / 内存） | 全文精确检索 |
//! | `DocStore` | 宿主（PG / 内存） | 读取 Block 全文构建 RAG context |
//! | `PermissionFilter` | wiki-collab | 检索后过滤无权 Block |
//!
//! # 架构约束
//!
//! - 本模块不 `use` agent-core 或任何 LLM 后端具体类型。
//! - 所有外部能力通过 trait 注入，单测用 mock 替代。
//! - 检索路径内置权限过滤——无权 Block 绝不进入 LLM 上下文。

use async_trait::async_trait;
use std::sync::Arc;
use wiki_collab::{PermissionFilter, RequestContext};
use wiki_core::{DocStore, TextIndex, VectorStore};

use crate::{rag, LlmCapability, LlmClient, LlmOpts, QaAnswer, TextUnit};

// ===========================================================================
// DefaultLlmEngine
// ===========================================================================

/// `LlmCapability` 的默认实现。
///
/// 构造时注入 5 个端口；`search` / `qa` 路径内置权限过滤。
pub struct DefaultLlmEngine {
    llm: Arc<dyn LlmClient>,
    vector: Arc<dyn VectorStore>,
    text: Arc<dyn TextIndex>,
    #[allow(dead_code)]
    docs: Arc<dyn DocStore>,
    permission: Arc<dyn PermissionFilter>,
}

impl DefaultLlmEngine {
    /// 构造完整引擎。
    ///
    /// 若不需要权限过滤（测试/单用户），可传入 [`AllowAllPermissionFilter`](crate::mock::AllowAllPermissionFilter)。
    pub fn new(
        llm: Arc<dyn LlmClient>,
        vector: Arc<dyn VectorStore>,
        text: Arc<dyn TextIndex>,
        docs: Arc<dyn DocStore>,
        permission: Arc<dyn PermissionFilter>,
    ) -> Self {
        Self {
            llm,
            vector,
            text,
            docs,
            permission,
        }
    }

    /// 构造不持有 DocStore 的引擎（search / embed / complete / summarize 可用）。
    pub fn without_docs(
        llm: Arc<dyn LlmClient>,
        vector: Arc<dyn VectorStore>,
        text: Arc<dyn TextIndex>,
        permission: Arc<dyn PermissionFilter>,
    ) -> Self {
        Self {
            llm,
            vector,
            text,
            docs: Arc::new(NoopDocStore),
            permission,
        }
    }

    fn text_query(&self, query: &str) -> wiki_core::TextQuery {
        rag::make_text_query(query)
    }
}

// ===========================================================================
// LlmCapability 实现
// ===========================================================================

#[async_trait]
impl LlmCapability for DefaultLlmEngine {
    // ---- embed ----

    async fn embed(&self, units: &[TextUnit]) -> crate::Result<Vec<Vec<f32>>> {
        if units.is_empty() {
            return Ok(vec![]);
        }
        let texts: Vec<String> = units.iter().map(|u| u.text.clone()).collect();
        self.llm.embed(&texts).await
    }

    // ---- search ----

    async fn search(&self, query: &str, top_k: usize) -> crate::Result<Vec<(TextUnit, f32)>> {
        // 1. 生成查询 embedding。
        let query_vecs = self
            .llm
            .embed(&[query.to_string()])
            .await?;
        let query_vec = query_vecs
            .into_iter()
            .next()
            .unwrap_or_else(|| vec![0.0; 4]);

        // 2. 混合检索（语义 + 全文）。
        let tq = self.text_query(query);
        let hits = rag::hybrid_search(
            self.vector.as_ref(),
            self.text.as_ref(),
            query_vec,
            &tq,
            top_k * 2, // 多召回，留给权限过滤余量
        )
        .await?;

        // 3. 权限过滤：使用系统上下文（调用方如需用户级过滤，应使用 search_with_permission）。
        let system_ctx = RequestContext {
            user_id: "system".into(),
            roles: vec![wiki_collab::SpaceRole::Owner],
        };
        let readable_ids: Vec<String> = self
            .permission
            .filter_readable(
                &system_ctx,
                hits.iter().map(|h| h.block_id.clone()).collect(),
            )
            .await;

        let readable: std::collections::HashSet<&str> =
            readable_ids.iter().map(|s| s.as_str()).collect();

        let filtered: Vec<_> = hits
            .into_iter()
            .filter(|h| readable.contains(h.block_id.as_str()))
            .take(top_k)
            .collect();

        Ok(rag::hits_to_text_units(&filtered))
    }

    // ---- complete ----

    async fn complete(&self, unit: &TextUnit, partial: &str) -> crate::Result<String> {
        let prompt = format!(
            "你是内容补全助手。基于以下文本续写：\n\n完整文本: {}\n待补全位置: {}| ←\n\n请直接从光标位置续写，不要重复已有内容。",
            unit.text, partial
        );
        self.llm
            .complete(&prompt, &LlmOpts {
                temperature: Some(0.3),
                ..Default::default()
            })
            .await
    }

    // ---- qa ----

    async fn qa(
        &self,
        question: &str,
        _scope_root: Option<&str>,
    ) -> crate::Result<QaAnswer> {
        // 1. 生成查询 embedding。
        let query_vecs = self
            .llm
            .embed(&[question.to_string()])
            .await?;
        let query_vec = query_vecs
            .into_iter()
            .next()
            .unwrap_or_else(|| vec![0.0; 4]);

        // 2. 混合检索。
        let tq = self.text_query(question);
        let hits = rag::hybrid_search(
            self.vector.as_ref(),
            self.text.as_ref(),
            query_vec,
            &tq,
            8, // QA 检索 top-8
        )
        .await?;

        // 3. 权限过滤（系统级别，调用方如需用户级过滤应使用 qa_with_permission）。
        let system_ctx = RequestContext {
            user_id: "system".into(),
            roles: vec![wiki_collab::SpaceRole::Owner],
        };
        let readable_ids = self
            .permission
            .filter_readable(
                &system_ctx,
                hits.iter().map(|h| h.block_id.clone()).collect(),
            )
            .await;
        let readable: std::collections::HashSet<&str> =
            readable_ids.iter().map(|s| s.as_str()).collect();
        let filtered: Vec<_> = hits
            .into_iter()
            .filter(|h| readable.contains(h.block_id.as_str()))
            .collect();

        // 4. 构建上下文 prompt。
        let context = rag::build_rag_context(&filtered);
        let prompt = rag::build_qa_prompt(question, &context);

        // 5. LLM 推理。
        let raw_answer = self
            .llm
            .complete(&prompt, &LlmOpts {
                temperature: Some(0.1),
                max_tokens: Some(1024),
                ..Default::default()
            })
            .await?;

        // 6. 提取引用。
        let citations = rag::extract_citations(&raw_answer);
        let answer = strip_citation_line(&raw_answer);

        Ok(QaAnswer { answer, citations })
    }

    // ---- summarize ----

    async fn summarize(&self, units: &[TextUnit]) -> crate::Result<String> {
        if units.is_empty() {
            return Ok("(无内容)".to_string());
        }
        let prompt = rag::build_summarize_prompt(units);
        self.llm
            .complete(&prompt, &LlmOpts {
                temperature: Some(0.2),
                max_tokens: Some(512),
                ..Default::default()
            })
            .await
    }
}

// ===========================================================================
// 扩展方法（不在 LlmCapability trait 上，提供用户级权限过滤）
// ===========================================================================

impl DefaultLlmEngine {
    /// 带用户级权限过滤的语义搜索。
    ///
    /// 与 [`LlmCapability::search`] 的区别：使用传入的 `RequestContext` 而非系统上下文。
    pub async fn search_with_permission(
        &self,
        ctx: &RequestContext,
        query: &str,
        top_k: usize,
    ) -> crate::Result<Vec<(TextUnit, f32)>> {
        let query_vecs = self
            .llm
            .embed(&[query.to_string()])
            .await?;
        let query_vec = query_vecs
            .into_iter()
            .next()
            .unwrap_or_else(|| vec![0.0; 4]);

        let tq = self.text_query(query);
        let hits = rag::hybrid_search(
            self.vector.as_ref(),
            self.text.as_ref(),
            query_vec,
            &tq,
            top_k * 2,
        )
        .await?;

        let readable_ids = self
            .permission
            .filter_readable(ctx, hits.iter().map(|h| h.block_id.clone()).collect())
            .await;
        let readable: std::collections::HashSet<&str> =
            readable_ids.iter().map(|s| s.as_str()).collect();

        let filtered: Vec<_> = hits
            .into_iter()
            .filter(|h| readable.contains(h.block_id.as_str()))
            .take(top_k)
            .collect();

        Ok(rag::hits_to_text_units(&filtered))
    }

    /// 带用户级权限过滤的 RAG 问答。
    pub async fn qa_with_permission(
        &self,
        ctx: &RequestContext,
        question: &str,
    ) -> crate::Result<QaAnswer> {
        let query_vecs = self
            .llm
            .embed(&[question.to_string()])
            .await?;
        let query_vec = query_vecs
            .into_iter()
            .next()
            .unwrap_or_else(|| vec![0.0; 4]);

        let tq = self.text_query(question);
        let hits = rag::hybrid_search(
            self.vector.as_ref(),
            self.text.as_ref(),
            query_vec,
            &tq,
            8,
        )
        .await?;

        let readable_ids = self
            .permission
            .filter_readable(ctx, hits.iter().map(|h| h.block_id.clone()).collect())
            .await;
        let readable: std::collections::HashSet<&str> =
            readable_ids.iter().map(|s| s.as_str()).collect();
        let filtered: Vec<_> = hits
            .into_iter()
            .filter(|h| readable.contains(h.block_id.as_str()))
            .collect();

        let context = rag::build_rag_context(&filtered);
        let prompt = rag::build_qa_prompt(question, &context);

        let raw_answer = self
            .llm
            .complete(&prompt, &LlmOpts {
                temperature: Some(0.1),
                max_tokens: Some(1024),
                ..Default::default()
            })
            .await?;

        let citations = rag::extract_citations(&raw_answer);
        let answer = strip_citation_line(&raw_answer);

        Ok(QaAnswer { answer, citations })
    }

    /// 获取内部 LlmClient 引用（供高级用法定制）。
    pub fn llm_client(&self) -> &Arc<dyn LlmClient> {
        &self.llm
    }
}

// ===========================================================================
// 内部辅助
// ===========================================================================

/// 从回答中移除 "引用: [...]" 行（已在 extract_citations 中提取过）。
fn strip_citation_line(answer: &str) -> String {
    answer
        .lines()
        .filter(|l| {
            let trimmed = l.trim_start();
            !trimmed.starts_with("引用:") && !trimmed.starts_with("Citations:")
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim_end()
        .to_string()
}

// ===========================================================================
// NoopDocStore（无文档存储时的占位实现）
// ===========================================================================
use wiki_core::{DocId, Document, Result as CoreResult};

struct NoopDocStore;

#[async_trait]
impl DocStore for NoopDocStore {
    async fn get(&self, _id: &DocId) -> CoreResult<Option<Document>> {
        Ok(None)
    }

    async fn save(&self, _doc: &Document) -> CoreResult<()> {
        Ok(())
    }

    async fn delete(&self, _id: &DocId) -> CoreResult<()> {
        Ok(())
    }

    async fn list(&self, _offset: usize, _limit: usize) -> CoreResult<Vec<DocId>> {
        Ok(vec![])
    }
}

// ===========================================================================
// 测试
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::{AllowAllPermissionFilter, MockLlmClient};
    use wiki_core::WikiStorage;
    use wiki_testkit::MemoryWikiStorage;

    fn build_engine() -> DefaultLlmEngine {
        let storage = MemoryWikiStorage::new();
        let llm = Arc::new(MockLlmClient::with_deterministic_embed(8).with_fixed_complete(
            "答案：Rust 的 async 基于 Future trait。\n---\n引用: [b1], [b2]",
        ));
        DefaultLlmEngine::new(
            llm,
            storage.vector_store(),
            storage.text_index(),
            storage.doc_store(),
            Arc::new(AllowAllPermissionFilter),
        )
    }

    async fn index_sample_block(engine: &DefaultLlmEngine, id: &str, text: &str) {
        engine
            .text
            .index_block(id, text, serde_json::json!({}))
            .await
            .unwrap();
        // 也写入向量：用一个简单确定性向量。
        let emb = engine
            .llm
            .embed(&[text.to_string()])
            .await
            .unwrap();
        engine
            .vector
            .upsert(
                "wiki_blocks",
                id,
                emb.into_iter().next().unwrap(),
                serde_json::json!({"text": text}),
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn embed_batches_units() {
        let engine = build_engine();
        let units = vec![
            TextUnit { id: "a".into(), text: "hello".into(), path: vec![] },
            TextUnit { id: "b".into(), text: "world".into(), path: vec![] },
        ];
        let vecs = engine.embed(&units).await.unwrap();
        assert_eq!(vecs.len(), 2);
        assert_eq!(vecs[0].len(), 8);
    }

    #[tokio::test]
    async fn embed_empty_returns_empty() {
        let engine = build_engine();
        let vecs = engine.embed(&[]).await.unwrap();
        assert!(vecs.is_empty());
    }

    #[tokio::test]
    async fn search_returns_relevant_blocks() {
        let engine = build_engine();
        index_sample_block(&engine, "b1", "Rust async/await 入门教程").await;
        index_sample_block(&engine, "b2", "Python 多线程指南").await;
        index_sample_block(&engine, "b3", "Rust tokio 运行时原理").await;

        let results = engine.search("rust async", 3).await.unwrap();
        assert!(!results.is_empty());

        // b1 和 b3 关于 Rust，应排在前面。
        let ids: Vec<&str> = results.iter().map(|(u, _)| u.id.as_str()).collect();
        assert!(ids.contains(&"b1"));
    }

    #[tokio::test]
    async fn search_empty_index_returns_empty() {
        let engine = build_engine();
        let results = engine.search("nothing", 5).await.unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn qa_builds_answer_with_citations() {
        let engine = build_engine();
        index_sample_block(&engine, "b1", "Rust 的 async 函数返回 Future 对象。").await;

        let qa = engine.qa("什么是 async?", None).await.unwrap();
        assert!(qa.answer.contains("Future"));
        assert!(qa.answer.contains("Rust"));
        // mock 输出格式包含引用行
        assert_eq!(qa.citations, vec!["b1".to_string(), "b2".to_string()]);
    }

    #[tokio::test]
    async fn qa_no_context_handled_gracefully() {
        let engine = build_engine();
        // 空索引。
        let qa = engine.qa("什么?", None).await.unwrap();
        // mock 仍返回固定文本。
        assert!(!qa.answer.is_empty());
    }

    #[tokio::test]
    async fn complete_appends_to_partial() {
        let engine = build_engine();
        let unit = TextUnit {
            id: "x".into(),
            text: "Rust 是一门系统编程语言。".into(),
            path: vec![],
        };
        let completion = engine.complete(&unit, "Rust 是").await.unwrap();
        assert!(!completion.is_empty());
    }

    #[tokio::test]
    async fn summarize_joins_and_delegates() {
        let engine = build_engine();
        let units = vec![
            TextUnit { id: "a".into(), text: "AAA".into(), path: vec![] },
        ];
        let s = engine.summarize(&units).await.unwrap();
        assert!(!s.is_empty());
    }

    #[tokio::test]
    async fn search_with_permission_filters() {
        let engine = build_engine();
        index_sample_block(&engine, "public", "public info").await;
        index_sample_block(&engine, "secret", "secret info").await;

        // 用 DenyList 过滤 "secret"。
        let deny_filter: Arc<dyn PermissionFilter> =
            Arc::new(crate::mock::DenyListPermissionFilter::new(["secret"]));
        let restricted_engine = DefaultLlmEngine::new(
            engine.llm_client().clone(),
            engine.vector.clone(),
            engine.text.clone(),
            engine.docs.clone(),
            deny_filter,
        );

        let ctx = RequestContext {
            user_id: "viewer".into(),
            roles: vec![wiki_collab::SpaceRole::Viewer],
        };
        let results = restricted_engine
            .search_with_permission(&ctx, "info", 5)
            .await
            .unwrap();
        let ids: Vec<&str> = results.iter().map(|(u, _)| u.id.as_str()).collect();
        assert!(ids.contains(&"public"));
        assert!(!ids.contains(&"secret"));
    }

    #[test]
    fn strip_citation_removes_reference_line() {
        let input = "答案正文\n---\n引用: [a], [b]";
        let stripped = strip_citation_line(input);
        assert_eq!(stripped, "答案正文\n---");
    }

    #[test]
    fn strip_citation_keeps_content_without_refs() {
        let input = "普通回答，无引用信息";
        assert_eq!(strip_citation_line(input), input);
    }
}
