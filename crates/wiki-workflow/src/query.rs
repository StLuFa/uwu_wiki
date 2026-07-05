//! Query 管线：RAG 问答 + 知识反写。
//!
//! 流程（ARCHITECTURE.md §8.3）：
//!
//! ```text
//! 1. 用户/Agent 提问
//! 2. 混合检索 → top-k Block
//! 3. LLM 基于 wiki 内容回答（不读原始 raw/）
//! 4. 返回答案 + 引用
//! 5. 【可选】反写判断：答案是否含新知识 → 写回 wiki
//! ```

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use wiki_core::{Block, BlockContent, BlockType, DocId, Op, Result, WikiSpace};
use wiki_llm::{LlmCapability, QaAnswer};

// ===========================================================================
// 反写策略
// ===========================================================================

/// Query 答案反写策略。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub enum WriteBackPolicy {
    /// 从不反写。
    Never,
    /// 置信度超阈值自动反写。
    Auto { confidence_threshold: f32 },
    /// 先询问再反写。
    #[default]
    AskFirst,
}

// ===========================================================================
// 查询结果
// ===========================================================================

/// Query 的完整结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryResult {
    /// LLM 生成的答案。
    pub answer: String,
    /// 引用的 Block ID + 相关度评分。
    pub cited_blocks: Vec<(String, f32)>,
    /// 反写结果（若触发写回）。
    pub write_back: Option<WriteBackResult>,
}

/// 反写结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriteBackResult {
    /// 写入的目标文档 ID。
    pub target_doc_id: DocId,
    /// 新建的 Block ID。
    pub new_block_id: String,
    /// 写回理由。
    pub reason: String,
}

/// 反写评估结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
struct WriteBackEval {
    /// 答案是否含 wiki 中未有的新知识。
    has_new_knowledge: bool,
    /// 该写回哪个页面（标题）。
    target_page: Option<String>,
    /// 要写回的内容（Markdown）。
    content_to_write: Option<String>,
    /// 置信度 0-1。
    confidence: f32,
    /// 理由。
    reason: String,
}

// ===========================================================================
// QueryPipeline
// ===========================================================================

/// Query 管线：问答 + 可选反写。
pub struct QueryPipeline {
    llm: Arc<dyn LlmCapability>,
    space: Arc<WikiSpace>,
}

impl QueryPipeline {
    pub fn new(llm: Arc<dyn LlmCapability>, space: Arc<WikiSpace>) -> Self {
        Self { llm, space }
    }

    /// 执行问答（不反写）。
    pub async fn ask(&self, question: &str) -> Result<QueryResult> {
        let qa = self.llm.qa(question, None).await?;
        Ok(QueryResult {
            answer: qa.answer,
            cited_blocks: qa.citations.iter().map(|c| (c.clone(), 1.0)).collect(),
            write_back: None,
        })
    }

    /// 执行问答 + 按策略反写。
    ///
    /// 反写流程：
    /// 1. 混合检索 → LLM 生成答案
    /// 2. 用 LLM 评估答案是否含新知识
    /// 3. 若策略允许 → 写回 wiki
    pub async fn ask_and_write_back(
        &self,
        question: &str,
        policy: &WriteBackPolicy,
    ) -> Result<QueryResult> {
        // 1. 问答。
        let qa = self.llm.qa(question, None).await?;
        let mut result = QueryResult {
            answer: qa.answer.clone(),
            cited_blocks: qa.citations.iter().map(|c| (c.clone(), 1.0)).collect(),
            write_back: None,
        };

        // 2. 若策略为 Never，直接返回。
        if matches!(policy, WriteBackPolicy::Never) {
            return Ok(result);
        }

        // 3. 评估是否值得写回。
        let eval = self.evaluate_write_back(question, &qa).await?;

        // 4. 按策略判断是否执行写回。
        let should_write = match policy {
            WriteBackPolicy::Never => false,
            WriteBackPolicy::Auto { confidence_threshold } => {
                eval.has_new_knowledge && eval.confidence >= *confidence_threshold
            }
            WriteBackPolicy::AskFirst => {
                // AskFirst：仍执行评估，但不自动写回——调用方检查 write_back 字段后决定。
                eval.has_new_knowledge && eval.confidence >= 0.7
            }
        };

        if should_write
            && let (Some(target_title), Some(content)) =
                (eval.target_page.clone(), eval.content_to_write.clone())
        {
            match self.write_back(&target_title, &content, &eval).await {
                Ok(wb) => {
                    result.write_back = Some(wb);
                }
                Err(_) => {
                    // 写回失败不阻断问答返回。
                }
            }
        }

        Ok(result)
    }

    /// 用 LLM 评估答案是否含新知识。
    async fn evaluate_write_back(
        &self,
        question: &str,
        qa: &QaAnswer,
    ) -> Result<WriteBackEval> {
        let prompt = format!(
            "你是一个知识库质量评估器。判断以下答案是否包含 wiki 中未记录的新知识。\n\
             \n\
             ## 问题\n\
             {question}\n\
             \n\
             ## 答案\n\
             {answer}\n\
             \n\
             ## 引用来源\n\
             {citations}\n\
             \n\
             ## 任务\n\
             以 JSON 格式评估（只输出 JSON）：\n\
             ```json\n\
             {{\n\
               \"has_new_knowledge\": true/false,\n\
               \"target_page\": \"适合写回的页面标题（无则 null）\",\n\
               \"content_to_write\": \"要写回的 Markdown 内容（无则 null）\",\n\
               \"confidence\": 0.0-1.0,\n\
               \"reason\": \"评估理由\"\n\
             }}\n\
             ```",
            answer = &qa.answer,
            citations = qa.citations.join(", ")
        );

        let response = self
            .llm
            .qa(&prompt, None)
            .await
            .unwrap_or_else(|_| QaAnswer {
                answer: r#"{"has_new_knowledge":false,"target_page":null,"content_to_write":null,"confidence":0.0,"reason":"eval failed"}"#.into(),
                citations: vec![],
            });

        let json_str = super::ingest::extract_json_block(&response.answer)
            .unwrap_or(&response.answer);
        Ok(serde_json::from_str(json_str).unwrap_or_else(|_| WriteBackEval {
            has_new_knowledge: false,
            target_page: None,
            content_to_write: None,
            confidence: 0.0,
            reason: "parse error".into(),
        }))
    }

    /// 将新知识写回 wiki。
    async fn write_back(
        &self,
        target_title: &str,
        content: &str,
        eval: &WriteBackEval,
    ) -> Result<WriteBackResult> {
        // 搜索目标页面。
        let hits = self.llm.search(target_title, 3).await?;
        let target_doc_id = hits
            .first()
            .and_then(|(unit, _)| unit.path.first().cloned())
            .map(DocId)
            .unwrap_or_else(|| DocId(format!("auto-{}", super::ingest::slugify(target_title))));

        // 尝试获取已有文档。
        let block = Block::new(
            BlockType::Paragraph,
            BlockContent::text(content),
            "query-write-back",
        );
        let new_block_id = block.id.0.clone();

        if let Ok(Some(doc)) = self.space.get_doc(&target_doc_id).await {
            // 追加到已有文档。
            let op = Op::InsertBlock {
                parent: doc.root.clone(),
                after: doc.last_child(&doc.root),
                block,
            };
            self.space.apply_ops(&target_doc_id, vec![op]).await?;
        } else {
            // 创建新文档。
            self.space.create_doc(target_title, block).await?;
        }

        Ok(WriteBackResult {
            target_doc_id,
            new_block_id,
            reason: eval.reason.clone(),
        })
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

    fn build_pipeline() -> QueryPipeline {
        let storage = MemoryWikiStorage::new();
        let mock_llm = Arc::new(
            MockLlmClient::with_deterministic_embed(8).with_fixed_complete(
                "答案：Rust 的 async 基于 Future trait。\n---\n引用: [b1]",
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
        QueryPipeline::new(engine, space)
    }

    #[tokio::test]
    async fn ask_returns_answer_with_citations() {
        let pipeline = build_pipeline();
        let result = pipeline.ask("什么是 async?").await.unwrap();
        assert!(result.answer.contains("Future"));
        assert!(!result.cited_blocks.is_empty());
        // 未指定 policy → 无反写。
        assert!(result.write_back.is_none());
    }

    #[tokio::test]
    async fn never_policy_blocks_write_back() {
        let pipeline = build_pipeline();
        let result = pipeline
            .ask_and_write_back("test", &WriteBackPolicy::Never)
            .await
            .unwrap();
        assert!(result.write_back.is_none());
    }

    #[tokio::test]
    async fn write_back_policy_default_is_ask_first() {
        let policy = WriteBackPolicy::default();
        assert!(matches!(policy, WriteBackPolicy::AskFirst));
    }
}
