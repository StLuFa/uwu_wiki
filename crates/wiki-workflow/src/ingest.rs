//! Ingest 管线：将外部原料消化为结构化 wiki 知识。
//!
//! 流程（ARCHITECTURE.md §8.2）：
//!
//! ```text
//! 1. 分析原料 → 提取 entity / concept / claim
//! 2. 搜索已有 wiki → 找相关页面
//! 3. LLM 对比 → 新增 / 更新 / 矛盾标注
//! 4. 生成 Op 列表 → 批量写入 wiki
//! 5. 创建摘要文档 → 写入 ingest 日志
//! ```

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use wiki_core::{Block, BlockContent, BlockType, DocId, Op, Result, WikiSpace};
use wiki_llm::{LlmCapability, QaAnswer};

// ===========================================================================
// 数据类型
// ===========================================================================

/// 原料来源。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestSource {
    /// 原始内容（Markdown / 纯文本 / URL 内容）。
    pub content: String,
    /// 可选标题（用作生成页面的默认标题）。
    pub title: Option<String>,
    /// 来源 URL（元数据，记录出处）。
    pub source_url: Option<String>,
}

/// Ingest 结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestResult {
    /// 生成的摘要文档 ID。
    pub summary_doc_id: Option<DocId>,
    /// 被更新的已有页面。
    pub touched_docs: Vec<DocId>,
    /// 新建的页面。
    pub created_docs: Vec<DocId>,
    /// 发现的矛盾。
    pub contradictions: Vec<Contradiction>,
}

/// 知识矛盾。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Contradiction {
    pub doc_id: DocId,
    pub block_id: Option<String>,
    /// 已有说法。
    pub existing_claim: String,
    /// 新来源的说法。
    pub incoming_claim: String,
    /// 严重程度。
    pub severity: ContradictionSeverity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContradictionSeverity {
    Minor,
    Major,
    Critical,
}

impl std::fmt::Display for ContradictionSeverity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Minor => write!(f, "minor"),
            Self::Major => write!(f, "major"),
            Self::Critical => write!(f, "critical"),
        }
    }
}

// ===========================================================================
// LLM 输出结构（用于解析结构化响应）
// ===========================================================================

/// 原料分析结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SourceAnalysis {
    /// 提取的实体名称列表。
    entities: Vec<String>,
    /// 提取的概念/主题。
    concepts: Vec<String>,
    /// 关键声明。
    claims: Vec<String>,
    /// 原料摘要（1-2 段）。
    summary: String,
}

/// 单页更新计划。
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PageUpdatePlan {
    /// 目标页面 ID（新建则为 None）。
    doc_id: Option<String>,
    /// 建议页面标题。
    title: String,
    /// 操作类型。
    action: UpdateAction,
    /// 要追加/更新的内容（Markdown）。
    content: String,
    /// 操作理由。
    reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
enum UpdateAction {
    /// 追加到已有页面。
    AppendToExisting,
    /// 创建新页面。
    CreateNew,
    /// 仅标注矛盾（不修改内容）。
    FlagContradiction,
}

// ===========================================================================
// IngestPipeline
// ===========================================================================

/// Ingest 管线：LLM 驱动的原料→知识转换。
pub struct IngestPipeline {
    llm: Arc<dyn LlmCapability>,
    space: Arc<WikiSpace>,
}

impl IngestPipeline {
    pub fn new(llm: Arc<dyn LlmCapability>, space: Arc<WikiSpace>) -> Self {
        Self { llm, space }
    }

    /// 执行完整 Ingest 流程。
    pub async fn run(&self, source: &IngestSource) -> Result<IngestResult> {
        // 1. 分析原料：提取实体/概念/声明。
        let analysis = self.analyze_source(source).await?;

        // 2. 为每个实体/概念搜索已有页面。
        let related_pages = self.find_related_pages(&analysis).await?;

        // 3. 生成更新计划（LLM 对比新旧知识）。
        let plan = self.build_update_plan(source, &analysis, &related_pages).await?;

        // 4. 执行计划：写入 wiki。
        let (touched, created, contradictions) = self.execute_plan(&plan).await?;

        // 5. 创建摘要文档。
        let summary_id = self
            .create_summary_doc(source, &analysis, &touched, &created)
            .await?;

        Ok(IngestResult {
            summary_doc_id: summary_id,
            touched_docs: touched,
            created_docs: created,
            contradictions,
        })
    }

    // ---- 步骤 1：分析原料 ----

    async fn analyze_source(&self, source: &IngestSource) -> Result<SourceAnalysis> {
        let prompt = format!(
            "你是一个知识库编辑。请分析以下原料，提取关键信息。\n\
             \n\
             ## 原料\n\
             {content}\n\
             \n\
             ## 任务\n\
             请以 JSON 格式输出分析结果（只输出 JSON，不要其他文字）：\n\
             ```json\n\
             {{\n\
               \"entities\": [\"实体名称1\", \"实体名称2\"],\n\
               \"concepts\": [\"概念/主题1\", \"概念/主题2\"],\n\
               \"claims\": [\"关键声明1\", \"关键声明2\"],\n\
               \"summary\": \"原料的 1-2 段中文摘要\"\n\
             }}\n\
             ```",
            content = &source.content
        );

        // 用 qa 模式获取结构化输出。
        let response = self
            .llm
            .qa(&prompt, None)
            .await
            .unwrap_or_else(|_| QaAnswer {
                answer: r#"{"entities":[],"concepts":[],"claims":[],"summary":""}"#.into(),
                citations: vec![],
            });

        // 尝试解析 JSON（可能被 markdown 代码块包裹）。
        let json_str = extract_json_block(&response.answer).unwrap_or(&response.answer);
        Ok(serde_json::from_str(json_str).unwrap_or_else(|_| SourceAnalysis {
            entities: vec![],
            concepts: vec![],
            claims: vec![],
            summary: source.content.chars().take(200).collect(),
        }))
    }

    // ---- 步骤 2：搜索已有页面 ----

    async fn find_related_pages(&self, analysis: &SourceAnalysis) -> Result<Vec<(String, DocId)>> {
        let mut related = Vec::new();
        // 用 entity + concept 名称搜索已有页面。
        let queries: Vec<String> = analysis
            .entities
            .iter()
            .chain(analysis.concepts.iter())
            .take(10) // 限制搜索数量
            .cloned()
            .collect();

        for q in &queries {
            let hits = self.llm.search(q, 3).await?;
            for (unit, _score) in hits {
                // TextUnit.path 可能包含 doc_id（若 indexing 时写入）。
                let doc_id = unit
                    .path
                    .first()
                    .cloned()
                    .unwrap_or_else(|| format!("auto-{}", slugify(q)));
                if !related.iter().any(|(_, d): &(String, DocId)| d.0 == doc_id) {
                    related.push((q.clone(), DocId(doc_id)));
                }
            }
        }
        Ok(related)
    }

    // ---- 步骤 3：生成更新计划 ----

    async fn build_update_plan(
        &self,
        _source: &IngestSource,
        analysis: &SourceAnalysis,
        related: &[(String, DocId)],
    ) -> Result<Vec<PageUpdatePlan>> {
        let related_desc: String = related
            .iter()
            .map(|(name, doc_id)| format!("- {name} (doc: {doc_id})"))
            .collect::<Vec<_>>()
            .join("\n");

        let prompt = format!(
            "你是一个知识库编辑。新旧知识对比如下：\n\
             \n\
             ## 已有页面\n\
             {related_desc}\n\
             \n\
             ## 新原料摘要\n\
             {summary}\n\
             \n\
             ## 新原料关键声明\n\
             {claims}\n\
             \n\
             ## 任务\n\
             判断新知识应如何整合到已有页面。以 JSON 数组输出（只输出 JSON）：\n\
             ```json\n\
             [{{\n\
               \"doc_id\": \"已有页面 ID（创建新页面则为 null）\",\n\
               \"title\": \"页面标题\",\n\
               \"action\": \"AppendToExisting | CreateNew | FlagContradiction\",\n\
               \"content\": \"要追加的 Markdown 内容\",\n\
               \"reason\": \"操作理由\"\n\
             }}]\n\
             ```\n\
             \n\
             规则：\n\
             - 新增内容用 AppendToExisting\n\
             - 实体无对应页面用 CreateNew\n\
             - 与已有内容矛盾用 FlagContradiction（不覆盖）",
            summary = &analysis.summary,
            claims = analysis.claims.join("\n- ")
        );

        let response = self
            .llm
            .qa(&prompt, None)
            .await
            .unwrap_or_else(|_| QaAnswer {
                answer: "[]".into(),
                citations: vec![],
            });

        let json_str = extract_json_block(&response.answer).unwrap_or(&response.answer);
        Ok(serde_json::from_str(json_str).unwrap_or_default())
    }

    // ---- 步骤 4：执行计划 ----

    async fn execute_plan(
        &self,
        plan: &[PageUpdatePlan],
    ) -> Result<(Vec<DocId>, Vec<DocId>, Vec<Contradiction>)> {
        let mut touched = Vec::new();
        let mut created = Vec::new();
        let mut contradictions = Vec::new();

        for p in plan {
            match p.action {
                UpdateAction::AppendToExisting => {
                    if let Some(ref doc_id_str) = p.doc_id {
                        let doc_id = DocId(doc_id_str.clone());
                        if let Ok(Some(doc)) = self.space.get_doc(&doc_id).await {
                            // 追加新 Block 到文档。
                            let block = Block::new(
                                BlockType::Paragraph,
                                BlockContent::text(&p.content),
                                "ingest",
                            );
                            let op = Op::InsertBlock {
                                parent: doc.root.clone(),
                                after: doc.last_child(&doc.root),
                                block,
                            };
                            if self.space.apply_ops(&doc_id, vec![op]).await.is_ok() {
                                touched.push(doc_id);
                            }
                        }
                    }
                }
                UpdateAction::CreateNew => {
                    let title = &p.title;
                    let root = Block::new(
                        BlockType::Paragraph,
                        BlockContent::text(&p.content),
                        "ingest",
                    );
                    if let Ok(doc) = self.space.create_doc(title, root).await {
                        created.push(doc.id);
                    }
                }
                UpdateAction::FlagContradiction => {
                    if let Some(ref doc_id_str) = p.doc_id {
                        contradictions.push(Contradiction {
                            doc_id: DocId(doc_id_str.clone()),
                            block_id: None,
                            existing_claim: String::new(),
                            incoming_claim: p.content.clone(),
                            severity: ContradictionSeverity::Major,
                        });
                    }
                }
            }
        }

        Ok((touched, created, contradictions))
    }

    // ---- 步骤 5：创建摘要文档 ----

    async fn create_summary_doc(
        &self,
        source: &IngestSource,
        analysis: &SourceAnalysis,
        touched: &[DocId],
        created: &[DocId],
    ) -> Result<Option<DocId>> {
        let title = source
            .title
            .clone()
            .unwrap_or_else(|| format!("Ingest: {}", &analysis.summary.chars().take(40).collect::<String>()));

        let meta = if let Some(ref url) = source.source_url {
            format!("\n\n> 来源: {url}")
        } else {
            String::new()
        };

        let content = format!(
            "# {}\n\n## 摘要\n\n{}\n\n## 关联页面\n\n{}## 触动页面\n\n{}\n{}",
            title,
            analysis.summary,
            created
                .iter()
                .map(|d| format!("- (新) {d}\n"))
                .collect::<String>(),
            touched
                .iter()
                .map(|d| format!("- (更新) {d}\n"))
                .collect::<String>(),
            meta,
        );

        let root = Block::new(
            BlockType::Heading,
            BlockContent::text(&content),
            "ingest",
        );
        let doc = self.space.create_doc(&title, root).await?;
        Ok(Some(doc.id))
    }
}

// ===========================================================================
// 工具函数
// ===========================================================================

/// 从 LLM 输出中提取 JSON 块（可能被 ```json ... ``` 包裹）。
pub fn extract_json_block(text: &str) -> Option<&str> {
    if let Some(start) = text.find("```json") {
        let after = &text[start + 7..];
        if let Some(end) = after.find("```") {
            return Some(after[..end].trim());
        }
    }
    // 找到最早的 `{` 或 `[`，从该位置开始提取。
    let brace = text.find('{');
    let bracket = text.find('[');
    match (brace, bracket) {
        (Some(b), Some(k)) => Some(&text[std::cmp::min(b, k)..]),
        (Some(b), None) => Some(&text[b..]),
        (None, Some(k)) => Some(&text[k..]),
        (None, None) => None,
    }
}

/// 简单 slug 生成。
pub fn slugify(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_lowercase()
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

    fn build_pipeline() -> IngestPipeline {
        let storage = MemoryWikiStorage::new();
        let mock_llm = Arc::new(
            MockLlmClient::with_deterministic_embed(8).with_fixed_complete(
                r#"{"entities":["Rust"],"concepts":["async programming"],"claims":["Rust uses async/await"],"summary":"Rust supports async programming with async/await syntax."}"#,
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
        IngestPipeline::new(engine, space)
    }

    #[tokio::test]
    async fn ingest_creates_summary_doc() {
        let pipeline = build_pipeline();
        let source = IngestSource {
            content: "Rust 是一门支持 async/await 的系统编程语言。".into(),
            title: Some("Rust Async 介绍".into()),
            source_url: Some("https://example.com".into()),
        };
        let result = pipeline.run(&source).await.unwrap();
        // 至少创建了摘要文档。
        assert!(result.summary_doc_id.is_some());
    }

    #[test]
    fn extract_json_from_code_block() {
        let text = "prefix\n```json\n{\"key\": \"value\"}\n```\nsuffix";
        assert_eq!(extract_json_block(text), Some("{\"key\": \"value\"}"));
    }

    #[test]
    fn extract_json_array() {
        let text = "some text [{\"a\":1}, {\"b\":2}] more text";
        assert!(extract_json_block(text).unwrap().starts_with("["));
    }

    #[test]
    fn extract_json_fallback_to_braces() {
        let text = "result: {\"entities\": []}";
        assert!(extract_json_block(text).unwrap().starts_with("{"));
    }

    #[test]
    fn slugify_generates_url_safe() {
        assert_eq!(slugify("Hello World!"), "hello-world");
        assert_eq!(slugify("Rust Async/await"), "rust-async-await");
    }
}
