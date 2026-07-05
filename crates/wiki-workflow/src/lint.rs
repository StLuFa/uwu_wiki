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
use wiki_core::{DocId, LinkTarget, Result, SpaceId, WikiSpace};
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

/// Lint 配置。
#[derive(Debug, Clone)]
pub struct LintConfig {
    /// 重复检测的余弦相似度阈值（默认 0.92）。
    pub duplicate_threshold: f32,
    /// 是否用 LLM 检查语义过时。
    pub stale_check_llm: bool,
    /// 是否自动修复断链。
    pub auto_fix_broken_links: bool,
    /// 是否自动为缺页创建占位页面。
    pub auto_create_missing_pages: bool,
    /// 过时阈值（多少天未更新视为可能过时）。
    pub stale_days_threshold: i64,
}

impl Default for LintConfig {
    fn default() -> Self {
        Self {
            duplicate_threshold: 0.92,
            stale_check_llm: false,
            auto_fix_broken_links: true,
            auto_create_missing_pages: false,
            stale_days_threshold: 90,
        }
    }
}

// ===========================================================================
// WikiLinter
// ===========================================================================

/// Wiki 健康检查器。
pub struct WikiLinter {
    llm: Arc<dyn LlmCapability>,
    space: Arc<WikiSpace>,
    config: LintConfig,
}

impl WikiLinter {
    pub fn new(llm: Arc<dyn LlmCapability>, space: Arc<WikiSpace>, config: LintConfig) -> Self {
        Self { llm, space, config }
    }

    /// 执行全量 Lint 扫描。
    pub async fn run(&self) -> Result<LintReport> {
        let now = Utc::now();
        let all_docs = self.space.list_docs(0, 500).await?;
        let total = all_docs.len();

        // 并行执行各检查项（顺序执行，实际可并行）。
        let orphan_pages = self.check_orphans(&all_docs).await?;
        let broken_links_fixed = if self.config.auto_fix_broken_links {
            self.check_and_fix_broken_links(&all_docs).await?
        } else {
            0
        };
        let missing_pages = self.check_missing_pages(&all_docs).await?;
        let stale_pages = self.check_stale_pages(&all_docs).await?;
        let duplicate_candidates = self.check_duplicates(&all_docs).await?;
        let contradictions = self.check_contradictions(&all_docs).await?;

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
        if self.config.auto_create_missing_pages {
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
    async fn check_stale_pages(&self, all_docs: &[DocId]) -> Result<Vec<(DocId, String)>> {
        let threshold = Utc::now() - Duration::days(self.config.stale_days_threshold);
        let mut stale = Vec::new();

        for doc_id in all_docs {
            if let Ok(Some(doc)) = self.space.get_doc(doc_id).await {
                let last_updated = doc.updated_at();
                if last_updated < threshold {
                    let reason = format!(
                        "{} 天未更新（最后更新于 {}）",
                        self.config.stale_days_threshold,
                        last_updated.format("%Y-%m-%d")
                    );
                    // 若启用 LLM 检查，可在此做语义过时判断。
                    if self.config.stale_check_llm {
                        // TODO: LLM 语义过时检测。
                        // 抽样读取页面内容，判断信息是否已过时。
                    }
                    stale.push((doc_id.clone(), reason));
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
        for doc_id in all_docs.iter().take(200) {
            // 限制扫描规模。
            if let Ok(Some(doc)) = self.space.get_doc(doc_id).await {
                let plain = doc
                    .blocks
                    .iter()
                    .map(|b| b.content.as_plain_text())
                    .collect::<Vec<_>>()
                    .join(" ");
                if plain.len() > 50 {
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
                if sim >= self.config.duplicate_threshold {
                    duplicates.push((texts[i].0.clone(), texts[j].0.clone(), sim));
                }
            }
        }

        // 按相似度降序。
        duplicates.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap());
        Ok(duplicates)
    }

    // ---- 矛盾检测 ----

    /// 矛盾检测：LLM 抽样高优先级页面，检查语义矛盾。
    async fn check_contradictions(&self, all_docs: &[DocId]) -> Result<Vec<String>> {
        let _ = all_docs; // TODO: LLM 抽样语义矛盾检测
        // 完整实现需用 LLM 抽样检查页面间的语义矛盾。
        // 当前为骨架——返回空列表。
        Ok(Vec::new())
    }
}

// ===========================================================================
// 工具函数
// ===========================================================================

/// 余弦相似度。
fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na * nb)
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
        WikiLinter::new(engine, space, LintConfig::default())
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

        let linter = WikiLinter::new(engine, space.clone(), LintConfig::default());
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
