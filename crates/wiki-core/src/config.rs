//! 统一配置系统 —— 所有可调参数的单一直值源。
//!
//! 取代分散在各 crate 中的硬编码值，支持从文件/环境变量加载（通过 serde）。

use serde::{Deserialize, Serialize};

/// wiki 引擎的全局配置。
///
/// 所有子配置均实现 [`Default`]，未显式指定时使用生产级默认值。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WikiConfig {
    /// 搜索相关配置。
    #[serde(default)]
    pub search: SearchConfig,
    /// RAG / LLM 调用参数。
    #[serde(default)]
    pub rag: RagConfig,
    /// Lint 审计配置。
    #[serde(default)]
    pub lint: LintConfig,
    /// Ingest 管线配置。
    #[serde(default)]
    pub ingest: IngestConfig,
    /// 数据校验规则。
    #[serde(default)]
    pub validation: ValidationConfig,
    /// 重试策略。
    #[serde(default)]
    pub retry: RetryConfig,
    /// 查询/反写配置。
    #[serde(default)]
    pub query: QueryConfig,
}

impl Default for WikiConfig {
    fn default() -> Self {
        Self {
            search: SearchConfig::default(),
            rag: RagConfig::default(),
            lint: LintConfig::default(),
            ingest: IngestConfig::default(),
            validation: ValidationConfig::default(),
            retry: RetryConfig::default(),
            query: QueryConfig::default(),
        }
    }
}

// ===========================================================================
// 搜索配置
// ===========================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchConfig {
    /// Ingest 时最多搜索多少个实体/概念（原 ingest.rs 中 `.take(10)`）。
    pub max_search_queries: usize,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            max_search_queries: 10,
        }
    }
}

// ===========================================================================
// RAG / LLM 配置
// ===========================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RagConfig {
    /// RRF（Reciprocal Rank Fusion）的 k 参数。
    pub rrf_k: f32,
    /// QA 检索时返回的 top-k 数量（原 engine.rs 中硬编码 8）。
    pub qa_top_k: usize,
    /// 搜索时过度拉取的倍数（为权限过滤留余量）。
    pub search_overfetch_multiplier: usize,
    /// QA 回答的 max_tokens。
    pub qa_max_tokens: usize,
    /// QA 回答的 temperature。
    pub qa_temperature: f32,
    /// 摘要生成的 max_tokens。
    pub summarize_max_tokens: usize,
    /// 摘要生成的 temperature。
    pub summarize_temperature: f32,
    /// 行内补全的 temperature。
    pub complete_temperature: f32,
    /// 回退 embedding 维度（当 LLM 返回空向量时）。
    pub fallback_embedding_dim: usize,
}

impl Default for RagConfig {
    fn default() -> Self {
        Self {
            rrf_k: 60.0,
            qa_top_k: 8,
            search_overfetch_multiplier: 2,
            qa_max_tokens: 1024,
            qa_temperature: 0.1,
            summarize_max_tokens: 512,
            summarize_temperature: 0.2,
            complete_temperature: 0.3,
            fallback_embedding_dim: 4,
        }
    }
}

// ===========================================================================
// Lint 审计配置
// ===========================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LintConfig {
    /// 重复检测的余弦相似度阈值（默认 0.92）。
    pub duplicate_threshold: f32,
    /// 是否启用 LLM 驱动的语义过时检测。
    pub stale_check_llm: bool,
    /// 是否自动修复断链。
    pub auto_fix_broken_links: bool,
    /// 是否自动为缺页创建占位页面。
    pub auto_create_missing_pages: bool,
    /// 过时阈值（天数）。
    pub stale_days_threshold: i64,
    /// 全量扫描时的文档数上限（原 `list_docs(0, 500)`）。
    pub list_limit: usize,
    /// 重复检测时的文档数上限（原 `.take(200)`）。
    pub duplicate_scan_limit: usize,
    /// 重复检测时最小文本长度。
    pub duplicate_min_text_len: usize,
    /// 矛盾检测候选对数量上限。
    pub contradiction_max_candidates: usize,
    /// 矛盾检测的相似度下界（低于此值不检查矛盾）。
    pub contradiction_min_similarity: f32,
    /// LLM 过时检测的候选数量上限。
    pub stale_check_llm_max_candidates: usize,
}

impl Default for LintConfig {
    fn default() -> Self {
        Self {
            duplicate_threshold: 0.92,
            stale_check_llm: false,
            auto_fix_broken_links: true,
            auto_create_missing_pages: false,
            stale_days_threshold: 90,
            list_limit: 500,
            duplicate_scan_limit: 200,
            duplicate_min_text_len: 50,
            contradiction_max_candidates: 5,
            contradiction_min_similarity: 0.7,
            stale_check_llm_max_candidates: 50,
        }
    }
}

// ===========================================================================
// Ingest 配置
// ===========================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestConfig {
    /// 搜索已有页面的查询数量上限（原 `.take(10)`）。
    pub search_limit: usize,
    /// 每个查询返回的结果数量。
    pub per_query_top_k: usize,
    /// 摘要标题截断长度（字符）。
    pub summary_title_max_chars: usize,
    /// LLM 解析失败时的兜底摘要长度。
    pub fallback_summary_max_chars: usize,
}

impl Default for IngestConfig {
    fn default() -> Self {
        Self {
            search_limit: 10,
            per_query_top_k: 3,
            summary_title_max_chars: 40,
            fallback_summary_max_chars: 200,
        }
    }
}

// ===========================================================================
// 数据校验配置
// ===========================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationConfig {
    /// 文档标题最大长度（字符）。
    pub max_title_len: usize,
    /// 单个文档最大 Block 数。
    pub max_blocks_per_doc: usize,
    /// Block 树最大嵌套深度。
    pub max_block_depth: usize,
    /// 单个 Block 的最大子块数。
    pub max_children_per_block: usize,
    /// 单个 Block 文本内容的最大长度（字符）。
    pub max_text_len_per_block: usize,
}

impl Default for ValidationConfig {
    fn default() -> Self {
        Self {
            max_title_len: 500,
            max_blocks_per_doc: 10_000,
            max_block_depth: 100,
            max_children_per_block: 100,
            max_text_len_per_block: 100_000,
        }
    }
}

// ===========================================================================
// 查询/反写配置
// ===========================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryConfig {
    /// 反写时搜索目标页面的 top-k。
    pub write_back_search_top_k: usize,
    /// AskFirst 策略的置信度阈值。
    pub write_back_confidence_threshold: f32,
}

impl Default for QueryConfig {
    fn default() -> Self {
        Self {
            write_back_search_top_k: 3,
            write_back_confidence_threshold: 0.7,
        }
    }
}

// ===========================================================================
// 重试配置
// ===========================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryConfig {
    /// 最大重试次数。
    pub max_retries: u32,
    /// 初始退避时间（毫秒）。
    pub initial_backoff_ms: u64,
    /// 最大退避时间（毫秒）。
    pub max_backoff_ms: u64,
    /// 退避乘数。
    pub backoff_multiplier: f32,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            initial_backoff_ms: 500,
            max_backoff_ms: 10_000,
            backoff_multiplier: 2.0,
        }
    }
}
