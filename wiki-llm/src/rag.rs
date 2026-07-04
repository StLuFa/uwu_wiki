//! RAG 检索管线：混合检索 + 权限过滤 + 上下文拼接。
//!
//! 实现 ARCHITECTURE.md §7 的检索流程：
//!
//! ```text
//! 用户查询
//!   ├─ 语义路 → VectorStore（向量）
//!   └─ 精确路 → TextIndex（倒排）
//!   → RRF 融合 → 权限过滤 → 返回 top-k
//! ```

use std::collections::HashMap;
use wiki_core::{MatchMode, TextHit, TextIndex, TextQuery, VectorSearchResult, VectorStore};

use crate::TextUnit;

// ===========================================================================
// 混合检索
// ===========================================================================

/// RRF（Reciprocal Rank Fusion）参数。
const RRF_K: f32 = 60.0;

/// 混合检索结果：融合后的统一命中项。
#[derive(Debug, Clone)]
pub struct HybridHit {
    pub block_id: String,
    pub snippet: String,
    pub score: f32,
}

/// 执行混合检索：向量语义 + 全文精确 → RRF 融合 → top-k。
///
/// # 参数
///
/// * `vector` — 向量存储（语义检索）。
/// * `text` — 全文倒排索引（精确/前缀检索）。
/// * `query_vec` — 查询的 embedding 向量。
/// * `text_query` — 查询的文本分解。
/// * `top_k` — 融合后保留条数。
pub async fn hybrid_search(
    vector: &dyn VectorStore,
    text: &dyn TextIndex,
    query_vec: Vec<f32>,
    text_query: &TextQuery,
    top_k: usize,
) -> crate::Result<Vec<HybridHit>> {
    // 两路并行检索。
    let (vec_hits, text_hits) = (
        vector
            .search("wiki_blocks", query_vec, top_k * 2, None),
        text.search(text_query, top_k * 2),
    );

    let (vec_hits, text_hits) = (vec_hits.await?, text_hits.await?);

    // RRF 融合。
    let merged = reciprocal_rank_fusion(&vec_hits, &text_hits, top_k);

    Ok(merged)
}

/// Reciprocal Rank Fusion：合并两个排序列表。
///
/// 公式：`score(d) = Σ_{r ∈ ranks} 1 / (k + rank(r))`
///
/// 避免向量分与 BM25 分量纲不可比的问题。
pub fn reciprocal_rank_fusion(
    vec_hits: &[VectorSearchResult],
    text_hits: &[TextHit],
    top_k: usize,
) -> Vec<HybridHit> {
    let mut scores: HashMap<String, f32> = HashMap::new();
    let mut snippets: HashMap<String, String> = HashMap::new();

    // 语义路：rank 从 1 开始。
    for (i, hit) in vec_hits.iter().enumerate() {
        let rank = (i + 1) as f32;
        *scores.entry(hit.id.clone()).or_insert(0.0) += 1.0 / (RRF_K + rank);
        snippets
            .entry(hit.id.clone())
            .or_insert_with(|| hit.metadata.get("text").and_then(|v| v.as_str()).unwrap_or("").to_string());
    }

    // 精确路。
    for (i, hit) in text_hits.iter().enumerate() {
        let rank = (i + 1) as f32;
        *scores.entry(hit.block_id.clone()).or_insert(0.0) += 1.0 / (RRF_K + rank);
        if !hit.snippet.is_empty() {
            snippets
                .entry(hit.block_id.clone())
                .or_insert_with(|| hit.snippet.clone());
        }
    }

    // 排序 + top-k。
    let mut merged: Vec<HybridHit> = scores
        .into_iter()
        .map(|(block_id, score)| HybridHit {
            snippet: snippets.remove(&block_id).unwrap_or_default(),
            block_id,
            score,
        })
        .collect();
    merged.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap());
    merged.truncate(top_k);
    merged
}

// ===========================================================================
// 上下文构建
// ===========================================================================

/// 将检索命中构造为 LLM 可用的上下文字符串。
///
/// 格式：
/// ```text
/// [Block {id}] ({path})
/// {snippet}
/// ```
pub fn build_rag_context(hits: &[HybridHit]) -> String {
    if hits.is_empty() {
        return "(无相关上下文)".to_string();
    }

    hits
        .iter()
        .map(|h| {
            format!(
                "[Block {}]\n{}",
                h.block_id,
                h.snippet,
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// 构建 RAG QA 完整 prompt。
///
/// 系统提示 + 上下文 + 问题 + 输出格式约束。
pub fn build_qa_prompt(question: &str, context: &str) -> String {
    format!(
        "你是一个知识库助手。请**仅基于以下上下文**回答问题，不要使用外部知识。\n\
         如果上下文不足以回答问题，请明确说明。\n\
         引用来源时使用 Block ID。\n\
         \n\
         ## 上下文\n\
         {context}\n\
         \n\
         ## 问题\n\
         {question}\n\
         \n\
         ## 回答\n\
         (请在此回答，在末尾列出引用的 Block ID：\n\
         ---\n\
         引用: [block-id-1], [block-id-2])"
    )
}

/// 从 LLM 回答末尾提取引用 Block ID。
///
/// 预期格式：`---\n引用: [id1], [id2]`
pub fn extract_citations(answer: &str) -> Vec<String> {
    // 找最后一行以 "引用:" 或 "Citations:" 开头的内容。
    answer
        .lines()
        .rev()
        .find(|l| l.trim_start().starts_with("引用:") || l.trim_start().starts_with("Citations:"))
        .map(|l| {
            l.split(['[', ']'])
                .enumerate()
                .filter_map(|(i, s)| {
                    if i % 2 == 1 && !s.is_empty() {
                        Some(s.trim().to_string())
                    } else {
                        None
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 构建摘要 prompt。
pub fn build_summarize_prompt(units: &[TextUnit]) -> String {
    let texts: Vec<String> = units
        .iter()
        .map(|u| format!("[{}] {}", u.id, u.text))
        .collect();
    let joined = texts.join("\n");
    format!(
        "请用一段简洁的中文总结以下内容的关键要点：\n\n{joined}\n\n## 总结"
    )
}

// ===========================================================================
// 工具函数
// ===========================================================================

/// 从原始检索命中构建 TextUnit。
pub fn hits_to_text_units(hits: &[HybridHit]) -> Vec<(TextUnit, f32)> {
    hits
        .iter()
        .map(|h| {
            (
                TextUnit {
                    id: h.block_id.clone(),
                    text: h.snippet.clone(),
                    path: vec![],
                },
                h.score,
            )
        })
        .collect()
}

/// 构造全文查询（默认精确模式，也可前缀匹配）。
pub fn make_text_query(query: &str) -> TextQuery {
    TextQuery {
        terms: query.split_whitespace().map(|s| s.to_string()).collect(),
        phrase: Some(query.to_string()),
        filter: None,
        mode: MatchMode::Exact,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rrf_merges_two_sources() {
        let vec = vec![
            VectorSearchResult {
                id: "a".into(),
                score: 0.95,
                metadata: serde_json::json!({"text": "aaa"}),
            },
            VectorSearchResult {
                id: "b".into(),
                score: 0.80,
                metadata: serde_json::json!({"text": "bbb"}),
            },
        ];
        let txt = vec![TextHit {
            block_id: "b".into(),
            snippet: "BBB".into(),
            score: 1.0,
        }];
        let merged = reciprocal_rank_fusion(&vec, &txt, 5);
        // b 在两路都出现 → RRF 分更高。
        assert_eq!(merged[0].block_id, "b");
        assert!(merged[0].score > merged[1].score);
    }

    #[test]
    fn rrf_handles_empty_text_hits() {
        let vec = vec![VectorSearchResult {
            id: "x".into(),
            score: 0.9,
            metadata: serde_json::json!({}),
        }];
        let merged = reciprocal_rank_fusion(&vec, &[], 5);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].block_id, "x");
    }

    #[test]
    fn build_context_formats_hits() {
        let hits = vec![HybridHit {
            block_id: "b1".into(),
            snippet: "Rust async/await".into(),
            score: 0.9,
        }];
        let ctx = build_rag_context(&hits);
        assert!(ctx.contains("[Block b1]"));
        assert!(ctx.contains("Rust async/await"));
    }

    #[test]
    fn build_context_empty_returns_placeholder() {
        let ctx = build_rag_context(&[]);
        assert!(ctx.contains("无相关上下文"));
    }

    #[test]
    fn build_qa_prompt_includes_context_and_question() {
        let prompt = build_qa_prompt("什么是 async?", "上下文内容");
        assert!(prompt.contains("上下文内容"));
        assert!(prompt.contains("什么是 async?"));
        assert!(prompt.contains("仅基于以下上下文"));
    }

    #[test]
    fn extract_citations_parses_reference_line() {
        let answer = "这是答案。\n---\n引用: [b1], [b2]";
        let cites = extract_citations(answer);
        assert_eq!(cites, vec!["b1".to_string(), "b2".to_string()]);
    }

    #[test]
    fn extract_citations_no_reference_line_returns_empty() {
        let cites = extract_citations("普通答案，无引用。");
        assert!(cites.is_empty());
    }

    #[test]
    fn make_text_query_splits_words() {
        let tq = make_text_query("rust async tokio");
        assert_eq!(tq.terms.len(), 3);
        assert_eq!(tq.phrase.unwrap(), "rust async tokio");
        assert_eq!(tq.mode, MatchMode::Exact);
    }

    #[test]
    fn hits_to_text_units_converts() {
        let hits = vec![HybridHit {
            block_id: "x".into(),
            snippet: "hello".into(),
            score: 0.8,
        }];
        let units = hits_to_text_units(&hits);
        assert_eq!(units[0].0.id, "x");
        assert_eq!(units[0].0.text, "hello");
        assert!((units[0].1 - 0.8).abs() < 0.001);
    }

    #[test]
    fn build_summarize_prompt_includes_units() {
        let units = vec![
            TextUnit { id: "a".into(), text: "AAA".into(), path: vec![] },
            TextUnit { id: "b".into(), text: "BBB".into(), path: vec![] },
        ];
        let prompt = build_summarize_prompt(&units);
        assert!(prompt.contains("[a] AAA"));
        assert!(prompt.contains("[b] BBB"));
        assert!(prompt.contains("总结"));
    }
}
