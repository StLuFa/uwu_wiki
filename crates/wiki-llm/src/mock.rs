//! 测试/开发用 mock 实现。
//!
//! - [`MockLlmClient`]：可配置的 LLM 后端桩，无需真实 LLM 即可测试下游 crate。
//! - [`AllowAllPermissionFilter`]：放行全部 Block 的权限过滤器。
//!
//! 仅用于测试和开发，不进生产路径。

use async_trait::async_trait;
use std::sync::Arc;
use wiki_collab::{PermissionFilter, RequestContext};
use wiki_core::Result;

use crate::{LlmClient, LlmOpts};

// ===========================================================================
// 类型别名
// ===========================================================================

/// Complete 回调类型。
type CompleteFn = Arc<dyn Fn(&str, &LlmOpts) -> String + Send + Sync>;
/// Embed 回调类型。
type EmbedFn = Arc<dyn Fn(&[String]) -> Vec<Vec<f32>> + Send + Sync>;

// ===========================================================================
// MockLlmClient
// ===========================================================================

/// 可配置的 mock LLM 客户端。
///
/// # 示例
///
/// ```ignore
/// let mock = MockLlmClient::new()
///     .with_complete(|prompt, _opts| format!("answer: {prompt}"))
///     .with_embed(|texts| texts.iter().map(|_| vec![0.1; 8]).collect());
/// ```
pub struct MockLlmClient {
    complete_fn: CompleteFn,
    embed_fn: EmbedFn,
}

impl MockLlmClient {
    /// 新建 mock，默认行为：
    /// - `complete` 返回 `"mock: {prompt}"`
    /// - `embed` 返回 4 维零向量
    pub fn new() -> Self {
        Self {
            complete_fn: Arc::new(|prompt, _opts| format!("mock: {prompt}")),
            embed_fn: Arc::new(|texts| texts.iter().map(|_| vec![0.0_f32; 4]).collect()),
        }
    }

    /// 设置 `complete` 返回逻辑。
    pub fn with_complete<F>(mut self, f: F) -> Self
    where
        F: Fn(&str, &LlmOpts) -> String + Send + Sync + 'static,
    {
        self.complete_fn = Arc::new(f);
        self
    }

    /// 设置 `embed` 返回逻辑。
    pub fn with_embed<F>(mut self, f: F) -> Self
    where
        F: Fn(&[String]) -> Vec<Vec<f32>> + Send + Sync + 'static,
    {
        self.embed_fn = Arc::new(f);
        self
    }

    /// 快捷构造：complete 始终返回固定文本。
    pub fn with_fixed_complete(mut self, text: impl Into<String>) -> Self {
        let s = text.into();
        self.complete_fn = Arc::new(move |_, _| s.clone());
        self
    }

    /// 快捷构造：embed 返回固定维度（默认 8 维）的确定性伪向量。
    /// 每个输入文本生成一个不同的简单向量（基于长度，不具语义意义）。
    pub fn with_deterministic_embed(dim: usize) -> Self {
        Self::new().with_embed(move |texts| {
            texts
                .iter()
                .map(|t| {
                    let mut v = vec![0.0_f32; dim];
                    // 确定性「散列」：文本长度 + 首字节 → 填充向量首位
                    let seed = (t.len() as f32) * 0.01 + t.as_bytes().first().copied().unwrap_or(0) as f32 * 0.001;
                    v[0] = (seed.sin() + 1.0) / 2.0; // [0, 1]
                    if dim > 1 {
                        v[1] = (seed.cos() + 1.0) / 2.0;
                    }
                    v
                })
                .collect()
        })
    }
}

impl Default for MockLlmClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl LlmClient for MockLlmClient {
    async fn complete(&self, prompt: &str, opts: &LlmOpts) -> Result<String> {
        Ok((self.complete_fn)(prompt, opts))
    }

    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok((self.embed_fn)(texts))
    }
}

// ===========================================================================
// AllowAllPermissionFilter
// ===========================================================================

/// 放行全部 Block 的权限过滤器（测试/开发用）。
///
/// 生产环境应替换为 [`AclPermissionFilter`](wiki_collab::AclPermissionFilter) 或等价实现。
#[derive(Default, Clone, Copy)]
pub struct AllowAllPermissionFilter;

#[async_trait]
impl PermissionFilter for AllowAllPermissionFilter {
    async fn can_read(&self, _ctx: &RequestContext, _block_id: &str) -> bool {
        true
    }

    async fn filter_readable(&self, _ctx: &RequestContext, block_ids: Vec<String>) -> Vec<String> {
        block_ids
    }
}

// ===========================================================================
// DenyListPermissionFilter
// ===========================================================================

/// 拒绝列表中 Block 的权限过滤器（测试用）。
///
/// 列表中 block 不可读，其余放行。
pub struct DenyListPermissionFilter {
    deny: std::collections::HashSet<String>,
}

impl DenyListPermissionFilter {
    pub fn new(deny_ids: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            deny: deny_ids.into_iter().map(|s| s.into()).collect(),
        }
    }
}

#[async_trait]
impl PermissionFilter for DenyListPermissionFilter {
    async fn can_read(&self, _ctx: &RequestContext, block_id: &str) -> bool {
        !self.deny.contains(block_id)
    }

    async fn filter_readable(&self, _ctx: &RequestContext, block_ids: Vec<String>) -> Vec<String> {
        block_ids
            .into_iter()
            .filter(|id| !self.deny.contains(id.as_str()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mock_llm_defaults() {
        let client = MockLlmClient::new();
        let ans = client
            .complete("hello", &LlmOpts::default())
            .await
            .unwrap();
        assert!(ans.contains("mock:"));
        assert!(ans.contains("hello"));

        let vecs = client.embed(&["a".into(), "b".into()]).await.unwrap();
        assert_eq!(vecs.len(), 2);
        assert_eq!(vecs[0].len(), 4);
    }

    #[tokio::test]
    async fn mock_llm_with_custom_complete() {
        let client = MockLlmClient::new().with_fixed_complete("bonjour");
        let ans = client.complete("any", &LlmOpts::default()).await.unwrap();
        assert_eq!(ans, "bonjour");
    }

    #[tokio::test]
    async fn mock_llm_deterministic_embed() {
        let client = MockLlmClient::with_deterministic_embed(8);
        let a = client.embed(&["rust".into()]).await.unwrap();
        let b = client.embed(&["rust".into()]).await.unwrap();
        // 确定性：相同输入 → 相同输出
        assert_eq!(a, b);
        assert_eq!(a[0].len(), 8);
        // 不同输入 → （大概率）不同输出
        let c = client.embed(&["python".into()]).await.unwrap();
        assert_ne!(a[0], c[0]);
    }

    #[tokio::test]
    async fn allow_all_permission_filter() {
        let filter = AllowAllPermissionFilter;
        let ctx = RequestContext {
            user_id: "any".into(),
            roles: vec![],
        };
        assert!(filter.can_read(&ctx, "secret").await);
        let ids = vec!["a".into(), "b".into()];
        assert_eq!(filter.filter_readable(&ctx, ids).await.len(), 2);
    }

    #[tokio::test]
    async fn deny_list_permission_filter() {
        let filter = DenyListPermissionFilter::new(["secret", "classified"]);
        let ctx = RequestContext {
            user_id: "u1".into(),
            roles: vec![],
        };
        assert!(!filter.can_read(&ctx, "secret").await);
        assert!(filter.can_read(&ctx, "public").await);

        let readable = filter
            .filter_readable(&ctx, vec!["secret".into(), "public".into(), "classified".into()])
            .await;
        assert_eq!(readable, vec!["public".to_string()]);
    }
}
