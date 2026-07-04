//! # wiki-collab
//!
//! 协作层：CRDT 合并计算（复用 uwu-crdt，不持有存储）+ 权限控制。
//!
//! 权限过滤实现 [`PermissionFilter`]，供检索层注入（RAG 越权泄露防护，见 ARCHITECTURE §7）。
//!
//! [`sync`] 模块把 wiki-core 领域 [`Op`](wiki_core::Op) 翻译成 uwu-crdt 的
//! [`UwuOp`](uwu_crdt::UwuOp)，交由 [`UwuCrdtDoc`](uwu_crdt::UwuCrdtDoc) 无冲突合并。

pub mod sync;

pub use sync::{CollabDoc, SyncError};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ===========================================================================
// 权限模型
// ===========================================================================

/// 空间角色。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum SpaceRole {
    Viewer,
    Editor,
    Owner,
}

/// 请求上下文（携带发起者身份）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestContext {
    pub user_id: String,
    pub roles: Vec<SpaceRole>,
}

impl RequestContext {
    pub fn highest_role(&self) -> Option<SpaceRole> {
        self.roles.iter().copied().max()
    }
}

/// 检索层权限端口。检索管线注入，过滤无权 Block，防止越权内容进入 LLM 上下文。
#[async_trait]
pub trait PermissionFilter: Send + Sync {
    async fn can_read(&self, ctx: &RequestContext, block_id: &str) -> bool;
    /// 批量过滤（检索热路径用，避免逐条 await）。
    async fn filter_readable(&self, ctx: &RequestContext, block_ids: Vec<String>) -> Vec<String>;
}

// ===========================================================================
// PermissionFilter 参考实现：Block 级 ACL
// ===========================================================================

/// 简单的 Block 级读权限表（骨架）：block_id → 允许读取的最低角色。
#[derive(Default)]
pub struct AclPermissionFilter {
    /// 未列出的 block 默认要求 Viewer（即所有角色可读）。
    block_min_role: HashMap<String, SpaceRole>,
}

impl AclPermissionFilter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn restrict(&mut self, block_id: impl Into<String>, min_role: SpaceRole) {
        self.block_min_role.insert(block_id.into(), min_role);
    }

    fn allowed(&self, ctx: &RequestContext, block_id: &str) -> bool {
        let required = self
            .block_min_role
            .get(block_id)
            .copied()
            .unwrap_or(SpaceRole::Viewer);
        ctx.highest_role().map(|r| r >= required).unwrap_or(false)
    }
}

#[async_trait]
impl PermissionFilter for AclPermissionFilter {
    async fn can_read(&self, ctx: &RequestContext, block_id: &str) -> bool {
        self.allowed(ctx, block_id)
    }

    async fn filter_readable(&self, ctx: &RequestContext, block_ids: Vec<String>) -> Vec<String> {
        block_ids
            .into_iter()
            .filter(|id| self.allowed(ctx, id))
            .collect()
    }
}

// ===========================================================================
// 协作会话骨架
// ===========================================================================

/// 协作会话：连接 / 心跳 / 离线 Op 队列（骨架）。
pub struct CollabSession {
    pub session_id: String,
    pub user: RequestContext,
    /// 离线 Op 队列（序列化）。
    offline_queue: Vec<serde_json::Value>,
}

impl CollabSession {
    pub fn new(session_id: impl Into<String>, user: RequestContext) -> Self {
        Self {
            session_id: session_id.into(),
            user,
            offline_queue: Vec::new(),
        }
    }

    pub fn enqueue_offline(&mut self, op: serde_json::Value) {
        self.offline_queue.push(op);
    }

    pub fn drain_offline(&mut self) -> Vec<serde_json::Value> {
        std::mem::take(&mut self.offline_queue)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn acl_filters_unauthorized_blocks() {
        let mut acl = AclPermissionFilter::new();
        acl.restrict("secret-block", SpaceRole::Owner);

        let viewer = RequestContext {
            user_id: "u1".into(),
            roles: vec![SpaceRole::Viewer],
        };

        assert!(!acl.can_read(&viewer, "secret-block").await);
        assert!(acl.can_read(&viewer, "public-block").await);

        let readable = acl
            .filter_readable(&viewer, vec!["secret-block".into(), "public-block".into()])
            .await;
        assert_eq!(readable, vec!["public-block".to_string()]);
    }

    #[test]
    fn role_ordering() {
        assert!(SpaceRole::Owner > SpaceRole::Editor);
        assert!(SpaceRole::Editor > SpaceRole::Viewer);
    }
}
