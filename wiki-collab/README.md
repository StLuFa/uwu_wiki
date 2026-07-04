# wiki-collab

协作层：CRDT 合并计算（复用 uwu-crdt，不持有存储）+ 权限控制。

---

## 架构定位

```
Client A Op → wiki-collab::sync 权限校验
           → uwu-crdt 内存合并（CRDT 合并算子，无冲突，不持久化）
           → 合并后 Block 树状态写 WikiStorage::doc_store()（PG）
           → Op 序列化写 WikiStorage::op_log()（离线回放）
           → 广播 Delta 给同 Doc 所有在线 Client
           → 离线 Client 重连 → 从 op_log() 拉取 Op 队列回放
```

| 职责 | 承担方 |
|---|---|
| CRDT 合并计算 | `uwu-crdt`（内存，无 I/O） |
| 状态持久化 | `WikiStorage::doc_store()` → PG |
| Op 日志持久化 | `WikiStorage::op_log()` → PG |
| Op 实时广播 | `uwu_event_mesh` |

---

## CRDT 同步 — CollabDoc

把 wiki-core 的领域 `Op` 翻译为 uwu-crdt 的 `UwuOp`，实现无冲突合并：

| wiki-core `Op` | uwu-crdt `UwuOp` |
|---|---|
| `InsertBlock { parent, after, block }` | `Insert { id, parent, after, data }` |
| `UpdateBlock { id, patch }` | `Update { id, patch }` |
| `DeleteBlock { id }` | `Delete { id }` |
| `MoveBlock { id, new_parent, after }` | `Move { id, new_parent, after }` |
| `UpdateDocMeta { .. }` | 文档级元数据，不进 Block 树 CRDT |

```rust
let mut doc = CollabDoc::new(peer_id, root_block)?;

// 应用 wiki Op
doc.apply_ops(&ops)?;

// 全量快照（新副本加入）
let snapshot = doc.snapshot()?;

// 增量同步（自某版本向量以来）
let delta = doc.updates_since(Some(&peer_version))?;

// 合并另一副本的更新
doc.merge(&remote_bytes)?;

// 两副本并发 → 收敛
assert_eq!(a.len(), b.len());
```

---

## 权限控制

### PermissionFilter trait

```rust
#[async_trait]
pub trait PermissionFilter: Send + Sync {
    async fn can_read(&self, ctx: &RequestContext, block_id: &str) -> bool;
    async fn filter_readable(&self, ctx: &RequestContext, block_ids: Vec<String>) -> Vec<String>;
}
```

检索管线注入——过滤发生在 prompt 构建**之前**，无权 Block 绝不进入 LLM 上下文。

### 内置实现：AclPermissionFilter

```rust
let mut acl = AclPermissionFilter::new();
acl.restrict("secret-block", SpaceRole::Owner);

let viewer = RequestContext { user_id: "u1", roles: vec![SpaceRole::Viewer] };
assert!(!acl.can_read(&viewer, "secret-block").await);
assert!(acl.can_read(&viewer, "public-block").await);
```

### 空间角色

```rust
pub enum SpaceRole {
    Viewer,   // 只读
    Editor,   // 读写
    Owner,    // 全部权限（含 ACL 管理）
}
```

---

## 协作会话

```rust
pub struct CollabSession {
    pub session_id: String,
    pub user: RequestContext,
    offline_queue: Vec<serde_json::Value>,  // 离线 Op 队列
}
```

- `enqueue_offline(op)` — 离线时暂存
- `drain_offline()` — 重连后批量回放

---

## 目录

```
wiki-collab/src/
├── lib.rs         CollabSession / PermissionFilter / AclPermissionFilter
└── sync.rs        CollabDoc（Op 翻译 + CRDT 合并 + 快照/增量/版本向量）
```

## 依赖

`wiki-core` / `uwu-crdt` / `serde` / `serde_json` / `async-trait`
