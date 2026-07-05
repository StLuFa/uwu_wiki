//! 存储端口 —— 全部 trait 定义在核心，实现由宿主注入（端口/适配器模式）。
//!
//! `wiki-core` 不含任何实现（`MemoryWikiStorage` 在 `wiki-testkit`）。
//! 生产环境由 `agent-context-db` 注入 PG + Qdrant 后端。

use crate::block::BlockId;
use crate::doc::{DocId, Document};
use crate::error::Result;
use crate::link::WikiLink;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

// ===========================================================================
// 存储门面 —— 聚合全部端口，宿主实现并注入
// ===========================================================================

/// 调用方实现此 trait 并在初始化时传入。
pub trait WikiStorage: Send + Sync + 'static {
    /// Block 向量存储（embedding upsert / 语义检索）。
    fn vector_store(&self) -> Arc<dyn VectorStore>;
    /// 文档/Block 持久化。
    fn doc_store(&self) -> Arc<dyn DocStore>;
    /// Op 日志持久化（CRDT 回放）。
    fn op_log(&self) -> Arc<dyn OpLog>;
    /// 全文倒排索引（精确关键词检索，#1）。
    fn text_index(&self) -> Arc<dyn TextIndex>;
    /// 引用图持久化（backlinks，#2）。
    fn link_store(&self) -> Arc<dyn LinkStore>;
    /// 二进制附件存储（#4）。
    fn blob_store(&self) -> Arc<dyn BlobStore>;
    /// 文档版本快照（历史浏览 / diff / 回滚，#5）。
    fn version_store(&self) -> Arc<dyn DocVersionStore>;
}

// ===========================================================================
// 端口：向量存储
// ===========================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VectorSearchResult {
    pub id: String,
    pub score: f32,
    pub metadata: Value,
}

#[async_trait]
pub trait VectorStore: Send + Sync {
    async fn upsert(&self, collection: &str, id: &str, vector: Vec<f32>, metadata: Value)
        -> Result<()>;
    async fn search(
        &self,
        collection: &str,
        query: Vec<f32>,
        top_k: usize,
        filter: Option<Value>,
    ) -> Result<Vec<VectorSearchResult>>;
    async fn delete(&self, collection: &str, id: &str) -> Result<()>;
}

// ===========================================================================
// 端口：文档/Block 持久化
// ===========================================================================

#[async_trait]
pub trait DocStore: Send + Sync {
    async fn get(&self, id: &DocId) -> Result<Option<Document>>;
    async fn save(&self, doc: &Document) -> Result<()>;
    async fn delete(&self, id: &DocId) -> Result<()>;
    async fn list(&self, offset: usize, limit: usize) -> Result<Vec<DocId>>;
}

// ===========================================================================
// 端口：Op 日志（CRDT 离线回放）
// ===========================================================================

#[async_trait]
pub trait OpLog: Send + Sync {
    /// 追加一批 Op（序列化后的），返回单调递增序号。
    async fn append(&self, doc_id: &DocId, ops: Vec<Value>) -> Result<u64>;
    /// 从指定序号回放 Op。
    async fn replay(&self, doc_id: &DocId, from_seq: u64) -> Result<Vec<Value>>;
}

// ===========================================================================
// 端口：全文倒排索引（#1）
// ===========================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MatchMode {
    Exact,
    Prefix,
    Fuzzy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextQuery {
    /// 精确词。
    pub terms: Vec<String>,
    /// 短语精确匹配。
    pub phrase: Option<String>,
    /// tag/status/space 过滤。
    pub filter: Option<Value>,
    pub mode: MatchMode,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextHit {
    pub block_id: String,
    pub snippet: String,
    pub score: f32,
}

#[async_trait]
pub trait TextIndex: Send + Sync {
    async fn index_block(&self, block_id: &str, text: &str, meta: Value) -> Result<()>;
    async fn search(&self, query: &TextQuery, top_k: usize) -> Result<Vec<TextHit>>;
    async fn remove(&self, block_id: &str) -> Result<()>;
}

// ===========================================================================
// 端口：引用图持久化（#2）
// ===========================================================================

#[async_trait]
pub trait LinkStore: Send + Sync {
    async fn upsert_links(&self, from: &BlockId, links: &[WikiLink]) -> Result<()>;
    async fn backlinks(&self, target_key: &str) -> Result<Vec<WikiLink>>;
    async fn broken_links(&self) -> Result<Vec<WikiLink>>;
}

// ===========================================================================
// 端口：二进制附件（#4）
// ===========================================================================

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BlobId(pub String);

#[async_trait]
pub trait BlobStore: Send + Sync {
    async fn put(&self, bytes: Vec<u8>, content_type: &str) -> Result<BlobId>;
    async fn get(&self, id: &BlobId) -> Result<(Vec<u8>, String)>;
    /// Block 引用变化时更新引用计数，返回新计数。归零由 GC 回收。
    async fn ref_delta(&self, id: &BlobId, delta: i32) -> Result<u64>;
    /// 回收 refcount=0 的孤儿 blob，返回回收数。
    async fn gc(&self) -> Result<usize>;
}

// ===========================================================================
// 端口：文档版本快照（#5）
// ===========================================================================

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionId(pub String);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionEntry {
    pub id: VersionId,
    pub label: Option<String>,
    pub author: String,
    pub ts: chrono::DateTime<chrono::Utc>,
}

/// Block 级结构化差异。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockChange {
    pub block_id: String,
    pub kind: ChangeKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChangeKind {
    Added,
    Removed,
    Modified,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DocDiff {
    pub changes: Vec<BlockChange>,
}

/// 用户级版本快照。与 CRDT OpLog 区分：OpLog 是操作流，此处是可读版本快照。
///
/// 挂 agent-context-db 时，由适配器把这些操作翻译成 context-db 的 commit/DAG，
/// 真值源唯一。独立部署时用 wiki-testkit 的线性快照实现。
#[async_trait]
pub trait DocVersionStore: Send + Sync {
    async fn snapshot(&self, doc_id: &DocId, doc: &Document, label: Option<String>)
        -> Result<VersionId>;
    async fn list_versions(&self, doc_id: &DocId) -> Result<Vec<VersionEntry>>;
    async fn get_version(&self, doc_id: &DocId, v: &VersionId) -> Result<Document>;
    async fn diff(&self, doc_id: &DocId, a: &VersionId, b: &VersionId) -> Result<DocDiff>;
    /// 回滚 = 以旧版为内容提交新版（不物理删除中间版本）。
    async fn restore(&self, doc_id: &DocId, v: &VersionId) -> Result<()>;
}
