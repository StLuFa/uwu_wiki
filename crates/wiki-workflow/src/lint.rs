//! Lint 管线：定期知识审计，保持 wiki 健康。
//!
//! 流程（ARCHITECTURE.md §8.4）：
//!
//! ```text
//! 1. 全量扫描（走索引，非全表扫描）
//! 2. 6 项检查：孤页 / 缺页 / 断链 / 过时 / 重复 / 矛盾
//! 3. 生成 LintReport
//! 4. 自动修复项执行（断链清理 / 缺页占位创建）
//! ```

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Arc;
use wiki_core::{cosine_similarity, DocId, LinkTarget, Result, SpaceId, WikiConfig, WikiSpace};
use wiki_llm::{LlmCapability, TextUnit};

// ===========================================================================
// 数据结构
// ===========================================================================

/// Lint 报告。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LintReport {
    pub space_id: SpaceId,
    pub ran_at: DateTime<Utc>,
    /// 无入链的孤页。
    pub orphan_pages: Vec<DocId>,
    /// 文中提及但无对应页面的实体名。
    pub missing_pages: Vec<String>,
    /// 发现的矛盾。
    pub contradictions: Vec<String>,
    /// 可能过时的页面。
    pub stale_pages: Vec<(DocId, String)>,
    /// 重复候选：(a, b, 相似度)。
    pub duplicate_candidates: Vec<(DocId, DocId, f32)>,
    /// 已修复的断链数。
    pub broken_links_fixed: usize,
    /// 总检查页面数。
    pub total_pages: usize,
}

// ===========================================================================
// WikiLinter
// ===========================================================================

/// Wiki 健康检查器。
pub struct WikiLinter {
    llm: Arc<dyn LlmCapability>,
    space: Arc<WikiSpace>,
    config: Arc<WikiConfig>,
}

impl WikiLinter {
    pub fn new(llm: Arc<dyn LlmCapability>, space: Arc<WikiSpace>, config: Arc<WikiConfig>) -> Self {
        Self { llm, space, config }
    }

    /// 执行全量 Lint 扫描。
    #[tracing::instrument(skip(self))]
    pub async fn run(&self) -> Result<LintReport> {
        let now = Utc::now();
        let all_docs = self.space.list_docs(0, self.config.lint.list_limit).await?;
        let total = all_docs.len();

        // 并行执行各检查项（顺序执行，实际可并行）。
        let orphan_pages = self.check_orphans(&all_docs).await?;
        let broken_links_fixed = if self.config.lint.auto_fix_broken_links {
            self.check_and_fix_broken_links(&all_docs).await?
        } else {
            0
        };
        let missing_pages = self.check_missing_pages(&all_docs).await?;
        let stale_pages = self.check_stale_pages(&all_docs).await?;
        let duplicate_candidates = self.check_duplicates(&all_docs).await?;
        let contradictions = self.check_contradictions(&all_docs, &duplicate_candidates).await?;

        Ok(LintReport {
            space_id: self.space.id.clone(),
            ran_at: now,
            orphan_pages,
            missing_pages,
            contradictions,
            stale_pages,
            duplicate_candidates,
            broken_links_fixed,
            total_pages: total,
        })
    }

    // ---- 孤页检测 ----

    /// 孤页 = 无任何入链的页面（不被任何其他页面引用）。
    async fn check_orphans(&self, all_docs: &[DocId]) -> Result<Vec<DocId>> {
        let mut orphans = Vec::new();
        for doc_id in all_docs {
            let backlinks = self.space.backlinks(&LinkTarget::Doc(doc_id.clone())).await?;
            if backlinks.is_empty() {
                orphans.push(doc_id.clone());
            }
        }
        Ok(orphans)
    }

    // ---- 断链检测 ----

    /// 检查所有页面引用的 DocId 是否仍存在，自动清理断链。
    async fn check_and_fix_broken_links(&self, all_docs: &[DocId]) -> Result<usize> {
        let _existing: HashSet<&DocId> = all_docs.iter().collect();
        let store = self.space.storage().link_store();
        let broken = store.broken_links().await?;
        let count = broken.len();

        // 清理断链：每个断链从出链表中移除。
        for link in &broken {
            let from = wiki_core::BlockId(link.from.0.clone());
            // 简单策略：移除该 block 的所有出链，重新索引时会重建。
            store.upsert_links(&from, &[]).await?;
        }
        Ok(count)
    }

    // ---- 缺页检测 ----

    /// 缺页 = 正文中 `[[entity]]` 提及但无对应 doc。
    async fn check_missing_pages(&self, _all_docs: &[DocId]) -> Result<Vec<String>> {
        // 收集所有出链中的 Broken 目标。
        let store = self.space.storage().link_store();
        let broken = store.broken_links().await?;
        let missing: Vec<String> = broken
            .iter()
            .filter_map(|l| {
                if let LinkTarget::Broken(ref name) = l.to {
                    Some(name.clone())
                } else {
                    None
                }
            })
            .collect();

        // 若配置允许，为缺页创建空白占位页。
        if self.config.lint.auto_create_missing_pages {
            for name in &missing {
                let root = wiki_core::Block::new(
                    wiki_core::BlockType::Paragraph,
                    wiki_core::BlockContent::text(format!("# {name}\n\n(自动创建的占位页面)")),
                    "lint",
                );
                // 忽略创建失败（可能已存在同名页面）。
                let _ = self.space.create_doc(name, root).await;
            }
        }

        Ok(missing)
    }

    // ---- 过时检测 ----

    /// 过时 = 超过阈值天数未更新。
    ///
    /// 若启用 `stale_check_llm`，对时间过时的文档进一步用 LLM 做语义级判断：
    /// 只有 LLM 确认内容已过时的才会被标记。这避免了"经典但不过时"的文档被误报。
    async fn check_stale_pages(&self, all_docs: &[DocId]) -> Result<Vec<(DocId, String)>> {
        let threshold = Utc::now() - Duration::days(self.config.lint.stale_days_threshold);
        let mut stale = Vec::new();
        let max_candidates = self.config.lint.stale_check_llm_max_candidates;
        let mut llm_checked = 0usize;

        for doc_id in all_docs {
            if let Ok(Some(doc)) = self.space.get_doc(doc_id).await {
                let last_updated = doc.updated_at();
                if last_updated < threshold {
                    let base_reason = format!(
                        "{} 天未更新（最后更新于 {}）",
                        self.config.lint.stale_days_threshold,
                        last_updated.format("%Y-%m-%d")
                    );

                    let mut is_stale = true;
                    let mut llm_reason = String::new();

                    // LLM 语义过时检测。
                    if self.config.lint.stale_check_llm && llm_checked < max_candidates {
                        llm_checked += 1;

                        let text: String = doc
                            .blocks
                            .iter()
                            .map(|b| b.content.as_plain_text())
                            .collect::<Vec<_>>()
                            .join("\n");
                        let snippet: String = text.chars().take(2000).collect();

                        let prompt = format!(
                            "当前日期: {today}。评估以下 wiki 页面内容是否已过时或不再准确。\
                             \n\n## 页面内容\n{snippet}\n\n## 任务\n\
                             以 JSON 输出（只输出 JSON）：\n\
                             ```json\n\
                             {{\n  \"is_stale\": true,\n  \"reason\": \"过时原因的简要中文描述\"\n\
                             }}\n\
                             ```\n\
                             若内容仍然准确、不过时，将 is_stale 设为 false。",
                            today = Utc::now().format("%Y-%m-%d"),
                        );

                        if let Ok(response) = self.llm.qa(&prompt, None).await {
                            let json_str =
                                super::ingest::extract_json_block(&response.answer)
                                    .unwrap_or(&response.answer);
                            if let Ok(parsed) =
                                serde_json::from_str::<serde_json::Value>(json_str)
                            {
                                let llm_says_stale = parsed
                                    .get("is_stale")
                                    .and_then(|v| v.as_bool())
                                    .unwrap_or(true); // 解析失败默认认为过时
                                if !llm_says_stale {
                                    is_stale = false;
                                }
                                if let Some(r) = parsed
                                    .get("reason")
                                    .and_then(|v| v.as_str())
                                    .filter(|s| !s.is_empty())
                                {
                                    llm_reason = format!(" [LLM: {r}]");
                                }
                            }
                        }
                    }

                    if is_stale {
                        stale.push((doc_id.clone(), format!("{base_reason}{llm_reason}")));
                    }
                }
            }
        }
        Ok(stale)
    }

    // ---- 重复检测 ----

    /// 重复 = 两个页面内容高度相似。
    async fn check_duplicates(
        &self,
        all_docs: &[DocId],
    ) -> Result<Vec<(DocId, DocId, f32)>> {
        // 收集所有文档的纯文本。
        let mut texts: Vec<(DocId, String)> = Vec::new();
        for doc_id in all_docs.iter().take(self.config.lint.duplicate_scan_limit) {
            // 限制扫描规模。
            if let Ok(Some(doc)) = self.space.get_doc(doc_id).await {
                let plain = doc
                    .blocks
                    .iter()
                    .map(|b| b.content.as_plain_text())
                    .collect::<Vec<_>>()
                    .join(" ");
                if plain.len() > self.config.lint.duplicate_min_text_len {
                    texts.push((doc_id.clone(), plain));
                }
            }
        }

        // 嵌入所有文档文本。
        let units: Vec<TextUnit> = texts
            .iter()
            .map(|(id, text)| TextUnit {
                id: id.0.clone(),
                text: text.clone(),
                path: vec![id.0.clone()],
            })
            .collect();

        let embeddings = self.llm.embed(&units).await?;

        // 两两比较余弦相似度。
        let mut duplicates = Vec::new();
        for i in 0..embeddings.len() {
            for j in (i + 1)..embeddings.len() {
                let sim = cosine_similarity(&embeddings[i], &embeddings[j]);
                if sim >= self.config.lint.duplicate_threshold {
                    duplicates.push((texts[i].0.clone(), texts[j].0.clone(), sim));
                }
            }
        }

        // 按相似度降序。
        duplicates.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap());
        Ok(duplicates)
    }

    // ---- 矛盾检测 ----

    /// 矛盾检测：对高相似度但非重复的文档对，用 LLM 做语义级矛盾检查。
    ///
    /// 筛选相似度在 `[contradiction_min_similarity, duplicate_threshold)` 区间的文档对，
    /// 取 top-N（由 `contradiction_max_candidates` 控制），对每对调用 LLM 检测事实矛盾。
    async fn check_contradictions(
        &self,
        all_docs: &[DocId],
        duplicate_candidates: &[(DocId, DocId, f32)],
    ) -> Result<Vec<String>> {
        let _ = all_docs;

        // 筛选"高度相似但非明显重复"的候选对（相似度在中间区间）。
        let min_sim = self.config.lint.contradiction_min_similarity;
        let max_sim = self.config.lint.duplicate_threshold;
        let max_candidates = self.config.lint.contradiction_max_candidates;

        let candidates: Vec<&(DocId, DocId, f32)> = duplicate_candidates
            .iter()
            .filter(|(_, _, sim)| *sim >= min_sim && *sim < max_sim)
            .take(max_candidates)
            .collect();

        if candidates.is_empty() {
            return Ok(Vec::new());
        }

        let mut contradictions = Vec::new();

        for (doc_a, doc_b, sim) in candidates {
            // 读取两个文档的文本内容。
            let text_a = match self.space.get_doc(doc_a).await {
                Ok(Some(doc)) => doc
                    .blocks
                    .iter()
                    .map(|b| b.content.as_plain_text())
                    .collect::<Vec<_>>()
                    .join("\n"),
                _ => continue,
            };
            let text_b = match self.space.get_doc(doc_b).await {
                Ok(Some(doc)) => doc
                    .blocks
                    .iter()
                    .map(|b| b.content.as_plain_text())
                    .collect::<Vec<_>>()
                    .join("\n"),
                _ => continue,
            };

            // 截断过长文本以控制 token 消耗。
            let max_chars = 2000;
            let text_a: String = text_a.chars().take(max_chars).collect();
            let text_b: String = text_b.chars().take(max_chars).collect();

            // 用 LLM 做语义矛盾检测。
            let prompt = format!(
                "你是一个知识库质量检查器。请检查以下两段文本是否存在事实性矛盾。\n\
                 \n\
                 ## 文本 A (相似度: {sim:.3})\n\
                 {text_a}\n\
                 \n\
                 ## 文本 B\n\
                 {text_b}\n\
                 \n\
                 ## 任务\n\
                 识别所有事实性矛盾。以 JSON 格式输出（只输出 JSON）：\n\
                 ```json\n\
                 {{\n\
                   \"has_contradiction\": true,\n\
                   \"description\": \"对矛盾的简要中文描述\"\n\
                 }}\n\
                 ```\n\
                 若无矛盾则返回：\n\
                 ```json\n\
                 {{\n\
                   \"has_contradiction\": false,\n\
                   \"description\": \"\"\n\
                 }}\n\
                 ```"
            );

            let response = self.llm.qa(&prompt, None).await.unwrap_or_else(|_| {
                wiki_llm::QaAnswer {
                    answer: r#"{"has_contradiction":false,"description":""}"#.into(),
                    citations: vec![],
                }
            });

            // 解析 LLM 响应。
            let json_str = super::ingest::extract_json_block(&response.answer)
                .unwrap_or(&response.answer);

            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(json_str) {
                if parsed
                    .get("has_contradiction")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
                {
                    let desc = parsed
                        .get("description")
                        .and_then(|v| v.as_str())
                        .unwrap_or("(无描述)")
                        .to_string();
                    contradictions.push(format!(
                        "[{} ↔ {}] {}",
                        doc_a, doc_b, desc
                    ));
                }
            }
        }

        Ok(contradictions)
    }

    // ---- 定时调度 ----

    /// 启动周期性 Lint 扫描。
    ///
    /// 每隔 `interval` 执行一次全量扫描，通过 `on_report` 回调传递结果。
    /// 返回 [`SchedulerHandle`]，调用 `.shutdown()` 可优雅停止。
    pub fn run_periodic<F>(
        self: Arc<Self>,
        interval: std::time::Duration,
        on_report: F,
    ) -> SchedulerHandle
    where
        F: Fn(LintReport) + Send + 'static,
    {
        let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);

        tokio::spawn(async move {
            let mut interval_timer = tokio::time::interval(interval);
            // 跳过第一次立即触发（让调用方有时间准备），首次在 interval 后执行。
            interval_timer.tick().await;

            loop {
                tokio::select! {
                    _ = interval_timer.tick() => {
                        match self.run().await {
                            Ok(report) => on_report(report),
                            Err(e) => {
                                tracing::warn!(error = %e, "lint scan failed, will retry at next interval");
                            }
                        }
                    }
                    _ = shutdown_rx.changed() => {
                        if *shutdown_rx.borrow() {
                            break;
                        }
                    }
                }
            }
        });

        SchedulerHandle {
            shutdown: shutdown_tx,
        }
    }
}

// ===========================================================================
// 调度器句柄
// ===========================================================================

/// 周期性 Lint 扫描的句柄，用于优雅关闭。
pub struct SchedulerHandle {
    shutdown: tokio::sync::watch::Sender<bool>,
}

impl SchedulerHandle {
    /// 发送关闭信号，后台任务将在当前周期完成后退出。
    pub fn shutdown(self) {
        let _ = self.shutdown.send(true);
    }
}

// ===========================================================================
// 测试
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use wiki_core::{Block, BlockContent, BlockType, SpaceId, WikiStorage};
    use wiki_llm::mock::MockLlmClient;
    use wiki_llm::DefaultLlmEngine;
    use wiki_testkit::MemoryWikiStorage;

    async fn build_linter() -> WikiLinter {
        let storage = MemoryWikiStorage::new();
        let mock_llm = Arc::new(MockLlmClient::with_deterministic_embed(8));
        let engine = Arc::new(DefaultLlmEngine::new(
            mock_llm,
            storage.vector_store(),
            storage.text_index(),
            storage.doc_store(),
            Arc::new(wiki_llm::mock::AllowAllPermissionFilter),
        ));
        let space = Arc::new(WikiSpace::new(SpaceId::default(), Arc::new(storage)));
        WikiLinter::new(engine, space, Arc::new(WikiConfig::default()))
    }

    #[tokio::test]
    async fn lint_empty_space() {
        let linter = build_linter().await;
        let report = linter.run().await.unwrap();
        assert_eq!(report.total_pages, 0);
        assert!(report.orphan_pages.is_empty());
        assert!(report.broken_links_fixed == 0);
    }

    #[tokio::test]
    async fn lint_detects_orphans() {
        let storage = MemoryWikiStorage::new();
        let mock_llm = Arc::new(MockLlmClient::with_deterministic_embed(8));
        let engine = Arc::new(DefaultLlmEngine::new(
            mock_llm,
            storage.vector_store(),
            storage.text_index(),
            storage.doc_store(),
            Arc::new(wiki_llm::mock::AllowAllPermissionFilter),
        ));
        let space = Arc::new(WikiSpace::new(SpaceId::default(), Arc::new(storage)));

        // 创建一个孤页（无任何页面引用它）。
        let root = Block::new(BlockType::Paragraph, BlockContent::text("orphan"), "test");
        space.create_doc("Orphan Page", root).await.unwrap();

        let linter = WikiLinter::new(engine, space.clone(), Arc::new(WikiConfig::default()));
        let report = linter.run().await.unwrap();
        assert_eq!(report.total_pages, 1);
        // 该页面无入链 → 孤页
        assert_eq!(report.orphan_pages.len(), 1);
    }

    #[test]
    fn cosine_identical_returns_one() {
        let v = vec![1.0, 2.0, 3.0];
        assert!((cosine_similarity(&v, &v) - 1.0).abs() < 0.001);
    }

    #[test]
    fn cosine_orthogonal_returns_zero() {
        let a = vec![1.0, 0.0];
        let b = vec![0.0, 1.0];
        assert!((cosine_similarity(&a, &b) - 0.0).abs() < 0.001);
    }

    #[test]
    fn cosine_zero_vector_returns_zero() {
        assert_eq!(cosine_similarity(&[0.0], &[1.0]), 0.0);
    }
}
