//! LLM 调用重试与指数退避。

use std::time::Duration;
use wiki_core::config::RetryConfig;
use wiki_core::{Result, WikiError};

/// 对可能瞬态失败的操作执行指数退避重试。
///
/// 仅对 `WikiError::Llm` 和 `WikiError::Storage` 重试；
/// `NotFound` / `Invalid` / `PermissionDenied` / `Serialization` / `VersionConflict` 立即返回。
pub async fn with_retry<T, F, Fut>(
    config: &RetryConfig,
    operation_name: &str,
    mut f: F,
) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    let mut attempt = 0u32;
    let mut backoff_ms = config.initial_backoff_ms;

    loop {
        attempt += 1;
        match f().await {
            Ok(val) => return Ok(val),
            Err(e) => {
                if !is_retryable(&e) || attempt >= config.max_retries {
                    return Err(e);
                }
                let delay = Duration::from_millis(backoff_ms.min(config.max_backoff_ms));
                eprintln!(
                    "[wiki-llm] {operation_name} attempt {attempt}/{} failed: {e}; retrying in {}ms",
                    config.max_retries, delay.as_millis()
                );
                tokio::time::sleep(delay).await;
                backoff_ms = ((backoff_ms as f32) * config.backoff_multiplier) as u64;
            }
        }
    }
}

/// 判断错误是否值得重试。
fn is_retryable(err: &WikiError) -> bool {
    matches!(err, WikiError::Llm(_) | WikiError::Storage(_))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    #[tokio::test]
    async fn retry_succeeds_on_first_attempt() {
        let config = RetryConfig::default();
        let result = with_retry(&config, "test", || async {
            Ok::<_, WikiError>(42)
        })
        .await;
        assert_eq!(result.unwrap(), 42);
    }

    #[tokio::test]
    async fn retry_eventually_succeeds() {
        let config = RetryConfig {
            max_retries: 3,
            initial_backoff_ms: 1,
            max_backoff_ms: 10,
            backoff_multiplier: 2.0,
        };
        let counter = Arc::new(AtomicU32::new(0));
        let c = counter.clone();
        let result = with_retry(&config, "test", move || {
            let c = c.clone();
            async move {
                let n = c.fetch_add(1, Ordering::SeqCst);
                if n < 2 {
                    Err(WikiError::Llm("transient".into()))
                } else {
                    Ok(99)
                }
            }
        })
        .await;
        assert_eq!(result.unwrap(), 99);
        assert_eq!(counter.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn non_retryable_error_returns_immediately() {
        let config = RetryConfig {
            max_retries: 5,
            initial_backoff_ms: 1,
            max_backoff_ms: 10,
            backoff_multiplier: 2.0,
        };
        let counter = Arc::new(AtomicU32::new(0));
        let c = counter.clone();
        let result = with_retry(&config, "test", move || {
            let c = c.clone();
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                Err::<i32, _>(WikiError::Invalid("bad".into()))
            }
        })
        .await;
        assert!(result.is_err());
        assert_eq!(counter.load(Ordering::SeqCst), 1, "不可重试错误应立即返回");
    }

    #[tokio::test]
    async fn exhausts_retries_then_fails() {
        let config = RetryConfig {
            max_retries: 2,
            initial_backoff_ms: 1,
            max_backoff_ms: 10,
            backoff_multiplier: 2.0,
        };
        let counter = Arc::new(AtomicU32::new(0));
        let c = counter.clone();
        let result = with_retry(&config, "test", move || {
            let c = c.clone();
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                Err::<i32, _>(WikiError::Storage("down".into()))
            }
        })
        .await;
        assert!(result.is_err());
        // 初始 1 次 + (max_retries - 1) 次重试 = 2 次尝试后退出
        // (attempt 在第一次失败后自增到 2，比较 2 >= max_retries=2 为 true，退出)
        assert_eq!(counter.load(Ordering::SeqCst), 2);
    }
}
