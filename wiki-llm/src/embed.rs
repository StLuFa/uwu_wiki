//! 增量 embedding：只重算内容版本落后的单元，其余复用缓存。
//!
//! 配合 wiki-core 的 embedding 陈旧检测（ARCHITECTURE.md §15.3）：
//! - 写入时 Block.version += 1，embedding_version 保持旧值。
//! - 检索命中陈旧 block 时发出 `wiki.embed.stale` 事件触发补算。
//! - 本模块提供批量补算逻辑：只对 `embedding_version < content_version` 的单元调 LLM。

use crate::{LlmClient, TextUnit};
use std::collections::HashMap;

// ===========================================================================
// EmbeddingCache
// ===========================================================================

/// 内存 embedding 缓存：block_id → (vector, embedding_version)。
///
/// 生产环境应使用 VectorStore 作为持久化存储；此缓存用于减少重复 LLM 调用。
#[derive(Default)]
pub struct EmbeddingCache {
    entries: HashMap<String, (Vec<f32>, u64)>,
}

impl EmbeddingCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// 从 VectorStore 批量加载已有 embedding（用于冷启动填充缓存）。
    pub fn load(&mut self, block_id: impl Into<String>, vector: Vec<f32>, version: u64) {
        self.entries.insert(block_id.into(), (vector, version));
    }

    /// 查询缓存的 embedding（若版本匹配）。
    pub fn get(&self, block_id: &str, content_version: u64) -> Option<&[f32]> {
        self.entries
            .get(block_id)
            .filter(|(_, ev)| *ev >= content_version)
            .map(|(v, _)| v.as_slice())
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

// ===========================================================================
// 增量 Embedding 批处理
// ===========================================================================

/// 增量 embedding 条目：标记一个实体当前的内容版本和 embedding 版本。
#[derive(Debug, Clone)]
pub struct EmbedUnit {
    pub unit: TextUnit,
    /// 当前内容版本。
    pub content_version: u64,
    /// 最后一次 embedding 时的内容版本。
    pub embedding_version: u64,
}

/// 增量 embedding 结果：新鲜嵌入 + 复用缓存的嵌入。
#[derive(Debug, Clone)]
pub struct DiffEmbedResult {
    /// 本次新生成的 embedding（仅在 stale 的 unit）。
    pub refreshed: Vec<(TextUnit, Vec<f32>)>,
    /// 无需重算、直接复用的已有 embedding。
    pub reused: Vec<(TextUnit, Vec<f32>)>,
}

/// 对一批单元执行增量 embedding：仅对 `embedding_version < content_version` 的单元调 LLM。
///
/// # 参数
///
/// * `client` — LLM 客户端（embed 接口）。
/// * `cache` — 可选的已有 embedding 缓存（先查缓存，命中的不再调 LLM）。
/// * `units` — 待检查的单元列表。
/// * `batch_size` — 一次 LLM 调用最多嵌入多少文本。
pub async fn diff_embed(
    client: &dyn LlmClient,
    cache: Option<&EmbeddingCache>,
    units: &[EmbedUnit],
    batch_size: usize,
) -> crate::Result<DiffEmbedResult> {
    // 1. 分类：stale（需要新 embedding）vs fresh（可复用）。
    let mut stale: Vec<&EmbedUnit> = Vec::new();
    let mut reused: Vec<(TextUnit, Vec<f32>)> = Vec::new();

    for u in units {
        if u.embedding_version >= u.content_version {
            // 已在最新版本。
            if let Some(v) = find_embedding(cache, &u.unit.id, u.embedding_version) {
                reused.push((u.unit.clone(), v));
            }
        } else if let Some(v) = find_embedding(cache, &u.unit.id, u.content_version) {
            // 缓存命中。
            reused.push((u.unit.clone(), v));
        } else {
            stale.push(u);
        }
    }

    // 2. 对 stale 单元批量调 LLM embed。
    let mut refreshed = Vec::new();
    for chunk in stale.chunks(batch_size) {
        let texts: Vec<String> = chunk.iter().map(|u| u.unit.text.clone()).collect();
        let vectors = client.embed(&texts).await?;
        for (u, v) in chunk.iter().zip(vectors) {
            refreshed.push((u.unit.clone(), v));
        }
    }

    Ok(DiffEmbedResult { refreshed, reused })
}

fn find_embedding(cache: Option<&EmbeddingCache>, id: &str, min_version: u64) -> Option<Vec<f32>> {
    cache.and_then(|c| c.get(id, min_version).map(|v| v.to_vec()))
}

// ===========================================================================
// 测试
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::MockLlmClient;

    fn text_unit(id: &str, text: &str) -> TextUnit {
        TextUnit {
            id: id.into(),
            text: text.into(),
            path: vec![],
        }
    }

    fn unit(id: &str, text: &str, content_v: u64, embed_v: u64) -> EmbedUnit {
        EmbedUnit {
            unit: text_unit(id, text),
            content_version: content_v,
            embedding_version: embed_v,
        }
    }

    #[tokio::test]
    async fn diff_embed_only_stale() {
        let client = MockLlmClient::with_deterministic_embed(4);
        let units = vec![
            unit("a", "hello", 3, 3),  // fresh，无需重算（无缓存时不在 reused 中）
            unit("b", "world", 5, 2),  // stale，需重算
        ];
        let result = diff_embed(&client, None, &units, 64).await.unwrap();
        // 仅 stale 单元进入 refreshed。
        assert_eq!(result.refreshed.len(), 1);
        assert_eq!(result.refreshed[0].0.id, "b");
        // fresh 单元无缓存可用时不出现在 reused 中（已在 VectorStore 里是最新的）。
    }

    #[tokio::test]
    async fn cache_hit_avoids_llm() {
        let mut cache = EmbeddingCache::new();
        cache.load("b", vec![0.5; 4], 3); // 缓存中是 v3

        let client = MockLlmClient::with_deterministic_embed(4);
        // b: content=3, embed_v=2 → 缓存命中（cache 有 version 3）
        let units = vec![unit("b", "world", 3, 2)];
        let result = diff_embed(&client, Some(&cache), &units, 64).await.unwrap();
        assert!(result.refreshed.is_empty());
        assert_eq!(result.reused.len(), 1);
        assert_eq!(result.reused[0].1, vec![0.5; 4]);
    }

    #[tokio::test]
    async fn cache_miss_triggers_llm() {
        let mut cache = EmbeddingCache::new();
        cache.load("b", vec![0.5; 4], 1); // 缓存版本 1 < content 3 → 过期

        let client = MockLlmClient::with_deterministic_embed(4);
        let units = vec![unit("b", "world", 3, 1)];
        let result = diff_embed(&client, Some(&cache), &units, 64).await.unwrap();
        assert_eq!(result.refreshed.len(), 1);
    }

    #[tokio::test]
    async fn large_batch_is_chunked() {
        let client = MockLlmClient::with_deterministic_embed(4);
        let units: Vec<_> = (0..10)
            .map(|i| unit(&format!("b{i}"), "text", 2, 0))
            .collect();
        let result = diff_embed(&client, None, &units, 3).await.unwrap();
        assert_eq!(result.refreshed.len(), 10);
        // 分 4 批：3+3+3+1
    }

    #[test]
    fn embedding_cache_version_filtering() {
        let mut cache = EmbeddingCache::new();
        cache.load("x", vec![1.0, 2.0], 5);
        assert!(cache.get("x", 5).is_some());
        assert!(cache.get("x", 6).is_none()); // 缓存版本 5 < 需要 6
    }
}
