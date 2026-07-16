//! # wiki-testkit
//!
//! 全部 7 个 `WikiStorage` 端口的内存参考实现，仅用于测试/开发。
//! 生产环境由 `agent-context-db` 注入真实后端。

use async_trait::async_trait;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use wiki_core::storage::{
    DocEvent, EventBus, EventStream, HybridHit, Permission, PermissionStore,
    BlobId, BlobStore, BlockChange, BoolOp, ChangeKind, DocDiff, DocStore, DocVersionStore,
    LinkStore, MatchMode, OpLog, TextHit, TextIndex, TextQuery, VectorSearchResult, VectorStore,
    DocSummary, MergeResult, VersionEntry, VersionId, WikiStorage,
};
use wiki_core::{BlockId, DocId, Document, Op, Result, SpaceId, WikiError, WikiLink};

// ===========================================================================
// 顶层门面
// ===========================================================================

#[derive(Default)]
pub struct MemoryWikiStorage {
    vector: Arc<MemVectorStore>,
    docs: Arc<MemDocStore>,
    oplog: Arc<MemOpLog>,
    text: Arc<MemTextIndex>,
    links: Arc<MemLinkStore>,
    blobs: Arc<MemBlobStore>,
    versions: Arc<MemVersionStore>,
    permissions: Arc<MemPermissionStore>,
    events: Arc<MemEventBus>,
}

impl MemoryWikiStorage {
    pub fn new() -> Self {
        Self::default()
    }
}

impl WikiStorage for MemoryWikiStorage {
    fn vector_store(&self) -> Arc<dyn VectorStore> {
        self.vector.clone()
    }
    fn doc_store(&self) -> Arc<dyn DocStore> {
        self.docs.clone()
    }
    fn op_log(&self) -> Arc<dyn OpLog> {
        self.oplog.clone()
    }
    fn text_index(&self) -> Arc<dyn TextIndex> {
        self.text.clone()
    }
    fn link_store(&self) -> Arc<dyn LinkStore> {
        self.links.clone()
    }
    fn blob_store(&self) -> Arc<dyn BlobStore> {
        self.blobs.clone()
    }
    fn version_store(&self) -> Arc<dyn DocVersionStore> {
        self.versions.clone()
    }
    fn permission_store(&self) -> Arc<dyn PermissionStore> {
        self.permissions.clone()
    }
    fn event_bus(&self) -> Arc<dyn EventBus> {
        self.events.clone()
    }
}

// ===========================================================================
// VectorStore
// ===========================================================================

#[derive(Default)]
struct MemVectorStore {
    // collection -> id -> (vector, metadata)
    data: Mutex<HashMap<String, HashMap<String, VectorRecord>>>,
}

/// 向量记录：(向量, 元数据)。
type VectorRecord = (Vec<f32>, serde_json::Value);

/// 简单的 JSON 字段匹配：filter 中的每个 key-value 须在 meta 中完全匹配。
fn filter_matches(filter: &serde_json::Value, meta: &serde_json::Value) -> bool {
    match (filter, meta) {
        (serde_json::Value::Object(f), serde_json::Value::Object(m)) => {
            f.iter().all(|(k, v)| m.get(k) == Some(v))
        }
        _ => filter == meta,
    }
}

#[async_trait]
impl VectorStore for MemVectorStore {
    async fn upsert(
        &self,
        collection: &str,
        id: &str,
        vector: Vec<f32>,
        metadata: serde_json::Value,
    ) -> Result<()> {
        self.data
            .lock()
            .entry(collection.to_string())
            .or_default()
            .insert(id.to_string(), (vector, metadata));
        Ok(())
    }

    async fn search(
        &self,
        collection: &str,
        query: Vec<f32>,
        top_k: usize,
        filter: Option<serde_json::Value>,
    ) -> Result<Vec<VectorSearchResult>> {
        let data = self.data.lock();
        let mut hits: Vec<VectorSearchResult> = data
            .get(collection)
            .map(|c| {
                c.iter()
                    .filter(|(_, (_, meta))| match &filter {
                        Some(f) => filter_matches(f, meta),
                        None => true,
                    })
                    .map(|(id, (vec, meta))| VectorSearchResult {
                        id: id.clone(),
                        score: wiki_core::cosine_similarity(&query, vec),
                        metadata: meta.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        hits.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap());
        hits.truncate(top_k);
        Ok(hits)
    }

    async fn batch_upsert(
        &self,
        collection: &str,
        items: &[(String, Vec<f32>, serde_json::Value)],
    ) -> Result<()> {
        let mut data = self.data.lock();
        let col = data.entry(collection.to_string()).or_default();
        for (id, vec, meta) in items {
            col.insert(id.clone(), (vec.clone(), meta.clone()));
        }
        Ok(())
    }

    async fn delete(&self, collection: &str, id: &str) -> Result<()> {
        if let Some(c) = self.data.lock().get_mut(collection) {
            c.remove(id);
        }
        Ok(())
    }
}

// ===========================================================================
// DocStore
// ===========================================================================

#[derive(Default)]
struct MemDocStore {
    docs: Mutex<HashMap<String, Document>>,
}

#[async_trait]
impl DocStore for MemDocStore {
    async fn get(&self, id: &DocId) -> Result<Option<Document>> {
        Ok(self.docs.lock().get(&id.0).cloned())
    }

    async fn save(&self, doc: &Document) -> Result<()> {
        self.docs.lock().insert(doc.id.0.clone(), doc.clone());
        Ok(())
    }

    async fn delete(&self, id: &DocId) -> Result<()> {
        self.docs.lock().remove(&id.0);
        Ok(())
    }

    async fn list(&self, offset: usize, limit: usize) -> Result<Vec<DocId>> {
        Ok(self
            .docs
            .lock()
            .keys()
            .skip(offset)
            .take(limit)
            .map(|k| DocId(k.clone()))
            .collect())
    }
}

// ===========================================================================
// OpLog
// ===========================================================================

#[derive(Default)]
struct MemOpLog {
    // doc -> ordered ops
    logs: Mutex<HashMap<String, Vec<serde_json::Value>>>,
}

#[async_trait]
impl OpLog for MemOpLog {
    async fn append(&self, doc_id: &DocId, ops: Vec<serde_json::Value>) -> Result<u64> {
        let mut logs = self.logs.lock();
        let entry = logs.entry(doc_id.0.clone()).or_default();
        entry.extend(ops);
        Ok(entry.len() as u64)
    }

    async fn replay(&self, doc_id: &DocId, from_seq: u64) -> Result<Vec<serde_json::Value>> {
        Ok(self
            .logs
            .lock()
            .get(&doc_id.0)
            .map(|v| v.iter().skip(from_seq as usize).cloned().collect())
            .unwrap_or_default())
    }
}

// ===========================================================================
// TextIndex
// ===========================================================================

#[derive(Default)]
struct MemTextIndex {
    // block_id -> (text, metadata)
    idx: Mutex<HashMap<String, (String, serde_json::Value)>>,
}

#[async_trait]
impl TextIndex for MemTextIndex {
    async fn index_block(
        &self,
        block_id: &str,
        text: &str,
        meta: serde_json::Value,
    ) -> Result<()> {
        self.idx.lock().insert(block_id.to_string(), (text.to_lowercase(), meta));
        Ok(())
    }

    async fn search(&self, query: &TextQuery, top_k: usize) -> Result<Vec<TextHit>> {
        let idx = self.idx.lock();
        let total_docs = idx.len().max(1) as f32;
        // 计算 IDF（为 BM25 评分用）
        let mut doc_freq: HashMap<String, f32> = HashMap::new();
        for term in &query.terms {
            let t = term.to_lowercase();
            let df = idx.values().filter(|(text, _)| text.contains(&t)).count() as f32;
            let idf = ((total_docs - df + 0.5) / (df + 0.5) + 1.0).ln().max(0.0);
            doc_freq.insert(t, idf);
        }

        let mut hits = Vec::new();
        for (id, (text, meta)) in idx.iter() {
            let text_len = text.split_whitespace().count().max(1) as f32;
            let avg_len = total_docs.max(1.0);

            // BM25 评分：k1=1.2, b=0.75
            let bm25_score: f32 = query
                .terms
                .iter()
                .map(|term| {
                    let t = term.to_lowercase();
                    let idf = doc_freq.get(&t).copied().unwrap_or(0.0);
                    let tf = text.matches(&t).count() as f32;
                    let k1 = 1.2;
                    let b = 0.75;
                    idf * (tf * (k1 + 1.0))
                        / (tf + k1 * (1.0 - b + b * text_len / avg_len))
                })
                .sum();

            let matched = match query.mode {
                MatchMode::Exact => query.terms.iter().all(|t| text.contains(&t.to_lowercase())),
                MatchMode::Prefix => query
                    .terms
                    .iter()
                    .any(|t| text.split_whitespace().any(|w| w.starts_with(&t.to_lowercase()))),
                MatchMode::Fuzzy => query.terms.iter().any(|t| text.contains(&t.to_lowercase())),
                MatchMode::Phrase => query
                    .phrase
                    .as_ref()
                    .map(|p| text.contains(&p.to_lowercase()))
                    .unwrap_or(false),
                MatchMode::Boolean => {
                    match query.bool_op.unwrap_or(BoolOp::And) {
                        BoolOp::And => query.terms.iter().all(|t| text.contains(&t.to_lowercase())),
                        BoolOp::Or => query.terms.iter().any(|t| text.contains(&t.to_lowercase())),
                        BoolOp::Not => !query.terms.iter().any(|t| text.contains(&t.to_lowercase())),
                    }
                }
            };
            let phrase_ok = query
                .phrase
                .as_ref()
                .map(|p| text.contains(&p.to_lowercase()))
                .unwrap_or(true);

            // 应用 filter：查询中的 filter 字段需与索引时的 meta 匹配。
            let filter_ok = match &query.filter {
                Some(filter) => filter_matches(filter, meta),
                None => true,
            };

            if matched && phrase_ok && filter_ok {
                hits.push(TextHit {
                    block_id: id.clone(),
                    snippet: text.chars().take(80).collect(),
                    score: bm25_score,
                });
            }
        }
        // 按 BM25 评分降序排列。
        hits.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap());
        hits.truncate(top_k);
        Ok(hits)
    }

    async fn remove(&self, block_id: &str) -> Result<()> {
        self.idx.lock().remove(block_id);
        Ok(())
    }
}

// ===========================================================================
// LinkStore
// ===========================================================================

#[derive(Default)]
struct MemLinkStore {
    // from block -> outbound links
    outbound: Mutex<HashMap<String, Vec<WikiLink>>>,
}

#[async_trait]
impl LinkStore for MemLinkStore {
    async fn resolve_cross_doc(&self, _link: &WikiLink) -> Result<Option<DocSummary>> { Ok(None) }
    async fn incoming_count(&self, _doc_id: &DocId) -> Result<usize> { Ok(0) }
    async fn outgoing_count(&self, _doc_id: &DocId) -> Result<usize> { Ok(0) }
    async fn find_orphans(&self, _space_id: &SpaceId) -> Result<Vec<DocId>> { Ok(Vec::new()) }
    async fn get_citation_graph(&self, _doc_id: &DocId, _depth: usize) -> Result<Vec<(DocId, DocId, String)>> { Ok(Vec::new()) }

    async fn upsert_links(&self, from: &BlockId, links: &[WikiLink]) -> Result<()> {
        self.outbound.lock().insert(from.0.clone(), links.to_vec());
        Ok(())
    }

    async fn backlinks(&self, target_key: &str) -> Result<Vec<WikiLink>> {
        let outbound = self.outbound.lock();
        Ok(outbound
            .values()
            .flatten()
            .filter(|l| l.to.key() == target_key)
            .cloned()
            .collect())
    }

    async fn broken_links(&self) -> Result<Vec<WikiLink>> {
        let outbound = self.outbound.lock();
        Ok(outbound
            .values()
            .flatten()
            .filter(|l| matches!(l.to, wiki_core::LinkTarget::Broken(_)))
            .cloned()
            .collect())
    }
}

// ===========================================================================
// BlobStore
// ===========================================================================

#[derive(Default)]
struct MemBlobStore {
    // id -> (bytes, content_type, refcount)
    blobs: Mutex<HashMap<String, BlobRecord>>,
}

/// blob 存储记录：(bytes, content_type, refcount)。
type BlobRecord = (Vec<u8>, String, i64);

#[async_trait]
impl BlobStore for MemBlobStore {
    async fn put(&self, bytes: Vec<u8>, content_type: &str) -> Result<BlobId> {
        let id = uuid::Uuid::new_v4().to_string();
        self.blobs
            .lock()
            .insert(id.clone(), (bytes, content_type.to_string(), 0));
        Ok(BlobId(id))
    }

    async fn get(&self, id: &BlobId) -> Result<(Vec<u8>, String)> {
        self.blobs
            .lock()
            .get(&id.0)
            .map(|(b, ct, _)| (b.clone(), ct.clone()))
            .ok_or_else(|| WikiError::NotFound(format!("blob {}", id.0)))
    }

    async fn ref_delta(&self, id: &BlobId, delta: i32) -> Result<u64> {
        let mut blobs = self.blobs.lock();
        let entry = blobs
            .get_mut(&id.0)
            .ok_or_else(|| WikiError::NotFound(format!("blob {}", id.0)))?;
        entry.2 += delta as i64;
        Ok(entry.2.max(0) as u64)
    }

    async fn gc(&self) -> Result<usize> {
        let mut blobs = self.blobs.lock();
        let before = blobs.len();
        blobs.retain(|_, (_, _, rc)| *rc > 0);
        Ok(before - blobs.len())
    }
}

// ===========================================================================
// DocVersionStore（线性快照实现）
// ===========================================================================

#[derive(Default)]
struct MemVersionStore {
    // doc -> ordered (VersionEntry, Document)
    versions: Mutex<HashMap<String, Vec<VersionRecord>>>,
}

/// 版本快照记录：(元信息, 文档全量)。
type VersionRecord = (VersionEntry, Document);

#[async_trait]
impl DocVersionStore for MemVersionStore {
    async fn create_branch(&self, _doc_id: &DocId, _name: &str, _from: &VersionId) -> Result<()> { Ok(()) }
    async fn merge(&self, _doc_id: &DocId, _source: &str, _target: &str) -> Result<MergeResult> {
        Ok(MergeResult { success: true, conflicts: Vec::new(), merged_version: None, strategy: "fast-forward".into() })
    }
    async fn log(&self, _doc_id: &DocId, _branch: &str, _offset: usize, _limit: usize) -> Result<Vec<VersionEntry>> { Ok(Vec::new()) }
    async fn list_branches(&self, _doc_id: &DocId) -> Result<Vec<String>> { Ok(vec!["main".into()]) }
    async fn cherry_pick(&self, _doc_id: &DocId, _commit: &VersionId, _onto: &str) -> Result<VersionId> { Ok(VersionId("cherry-picked".into())) }
    async fn create_tag(&self, _doc_id: &DocId, _name: &str, _target: &VersionId) -> Result<()> { Ok(()) }
    async fn list_tags(&self, _doc_id: &DocId) -> Result<Vec<(String, VersionId)>> { Ok(Vec::new()) }

    async fn snapshot(
        &self,
        doc_id: &DocId,
        doc: &Document,
        label: Option<String>,
    ) -> Result<VersionId> {
        let vid = VersionId(uuid::Uuid::new_v4().to_string());
        let entry = VersionEntry {
            id: vid.clone(),
            label,
            author: "system".into(),
            ts: chrono::Utc::now(),
        };
        self.versions
            .lock()
            .entry(doc_id.0.clone())
            .or_default()
            .push((entry, doc.clone()));
        Ok(vid)
    }

    async fn list_versions(&self, doc_id: &DocId) -> Result<Vec<VersionEntry>> {
        Ok(self
            .versions
            .lock()
            .get(&doc_id.0)
            .map(|v| v.iter().map(|(e, _)| e.clone()).collect())
            .unwrap_or_default())
    }

    async fn get_version(&self, doc_id: &DocId, v: &VersionId) -> Result<Document> {
        self.versions
            .lock()
            .get(&doc_id.0)
            .and_then(|list| list.iter().find(|(e, _)| &e.id == v).map(|(_, d)| d.clone()))
            .ok_or_else(|| WikiError::NotFound(format!("version {}", v.0)))
    }

    async fn diff(&self, doc_id: &DocId, a: &VersionId, b: &VersionId) -> Result<DocDiff> {
        let va = self.get_version(doc_id, a).await?;
        let vb = self.get_version(doc_id, b).await?;
        let ids_a: Vec<&BlockId> = va.blocks.iter().map(|blk| blk.id()).collect();
        let ids_b: Vec<&BlockId> = vb.blocks.iter().map(|blk| blk.id()).collect();

        let mut changes = Vec::new();
        for blk in &vb.blocks {
            if !ids_a.contains(&&blk.id()) {
                changes.push(BlockChange {
                    block_id: blk.id().0.clone(),
                    kind: ChangeKind::Added,
                });
            }
        }
        for blk in &va.blocks {
            if !ids_b.contains(&&blk.id()) {
                changes.push(BlockChange {
                    block_id: blk.id().0.clone(),
                    kind: ChangeKind::Removed,
                });
            }
        }
        Ok(DocDiff { changes })
    }

    async fn restore(&self, doc_id: &DocId, v: &VersionId) -> Result<()> {
        // 以旧版为内容提交新版（不物理删除中间版本）。
        let doc = self.get_version(doc_id, v).await?;
        self.snapshot(doc_id, &doc, Some(format!("restore of {}", v.0)))
            .await?;
        Ok(())
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use wiki_core::{Block, BlockContent, BlockId, BlockType, Op, SpaceId, WikiSpace};

    fn sample_doc() -> Document {
        let root = Block::new(BlockType::Paragraph, BlockContent::text("hello world"), "a");
        Document::new("Doc", root, SpaceId::default())
    }

    #[tokio::test]
    async fn full_storage_roundtrip() {
        let storage = MemoryWikiStorage::new();
        let doc = sample_doc();

        // DocStore
        storage.doc_store().save(&doc).await.unwrap();
        assert!(storage.doc_store().get(&doc.id).await.unwrap().is_some());

        // VectorStore
        storage
            .vector_store()
            .upsert("wiki_blocks", "b1", vec![1.0, 0.0], serde_json::json!({}))
            .await
            .unwrap();
        let hits = storage
            .vector_store()
            .search("wiki_blocks", vec![1.0, 0.0], 5, None)
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);

        // TextIndex
        storage
            .text_index()
            .index_block("b1", "Rust async tokio", serde_json::json!({}))
            .await
            .unwrap();
        let tq = TextQuery {
            terms: vec!["tokio".into()],
            phrase: None,
            filter: None,
            mode: MatchMode::Exact,
            bool_op: None,
        };
        assert_eq!(storage.text_index().search(&tq, 5).await.unwrap().len(), 1);

        // BlobStore + GC
        let bid = storage
            .blob_store()
            .put(vec![1, 2, 3], "image/png")
            .await
            .unwrap();
        assert_eq!(storage.blob_store().gc().await.unwrap(), 1); // refcount 0 → 回收
        storage.blob_store().put(vec![4], "text/plain").await.unwrap();
        // 不再断言具体 blob 内容（已回收）
        let _ = bid;
    }

    #[tokio::test]
    async fn version_snapshot_and_diff() {
        let storage = MemoryWikiStorage::new();
        let mut doc = sample_doc();
        let vs = storage.version_store();

        let v1 = vs.snapshot(&doc.id(), &doc, Some("v1".into())).await.unwrap();

        // 加一个 block 再快照
        let extra = Block::new(BlockType::Paragraph, BlockContent::text("more"), "a");
        doc.blocks.push(extra);
        let v2 = vs.snapshot(&doc.id(), &doc, Some("v2".into())).await.unwrap();

        assert_eq!(vs.list_versions(&doc.id).await.unwrap().len(), 2);

        let diff = vs.diff(&doc.id(), &v1, &v2).await.unwrap();
        assert_eq!(diff.changes.len(), 1);
        assert_eq!(diff.changes[0].kind, ChangeKind::Added);

        // restore 生成第三个版本
        vs.restore(&doc.id(), &v1).await.unwrap();
        assert_eq!(vs.list_versions(&doc.id).await.unwrap().len(), 3);
    }

    /// 验证 WAL 回滚：批次中间有无效 Op 时，文档状态不变。
    #[tokio::test]
    async fn wal_rollback_on_invalid_op() {
        let storage = MemoryWikiStorage::new();
        let space = WikiSpace::new(SpaceId::default(), Arc::new(storage));

        // 创建一个文档。
        let doc = space
            .create_doc("Test", Block::new(
                BlockType::Paragraph,
                BlockContent::text("root content"),
                "tester",
            ))
            .await
            .unwrap();

        let root_id = doc.root.clone();
        let initial_block_count = doc.blocks.len();

        // 构造一批 Op：第一个有效，第二个无效（引用不存在的 block）。
        let valid_block = Block::new(
            BlockType::Paragraph,
            BlockContent::text("valid child"),
            "tester",
        );
        let ops = vec![
            Op::InsertBlock {
                parent: root_id.clone(),
                after: None,
                block: valid_block,
            },
            // 无效 Op：UpdateBlock 引用不存在的 block id。
            Op::UpdateBlock {
                id: BlockId::new(), // 新 ID，不存在于文档中
                patch: serde_json::json!({"text": "should fail"}),
            },
        ];

        let result = space.apply_ops(&doc.id(), ops).await;
        assert!(result.is_err(), "包含无效 Op 的批次应返回错误");

        // 验证文档未被修改。
        let reloaded = space.get_doc(&doc.id).await.unwrap().unwrap();
        assert_eq!(
            reloaded.blocks.len(),
            initial_block_count,
            "回滚后 Block 数量应与初始一致"
        );
        assert_eq!(reloaded.version, doc.version, "文档版本不应增加");
    }

    /// 验证有效批次正常提交。
    #[tokio::test]
    async fn wal_commit_valid_batch() {
        let storage = MemoryWikiStorage::new();
        let space = WikiSpace::new(SpaceId::default(), Arc::new(storage));

        let doc = space
            .create_doc("Test", Block::new(
                BlockType::Paragraph,
                BlockContent::text("root content"),
                "tester",
            ))
            .await
            .unwrap();

        let root_id = doc.root.clone();
        let child = Block::new(
            BlockType::Paragraph,
            BlockContent::text("child"),
            "tester",
        );
        let child_id = child.id().clone();

        let ops = vec![Op::InsertBlock {
            parent: root_id.clone(),
            after: None,
            block: child,
        }];

        let result = space.apply_ops(&doc.id(), ops).await;
        assert!(result.is_ok(), "有效批次应成功提交");
        let updated = result.unwrap();
        assert!(updated.block(&child_id).is_some(), "新 Block 应已添加到文档");
        assert!(updated.version > doc.version, "版本应递增");
    }

    /// 验证撤销/重做流程。
    #[tokio::test]
    async fn undo_redo_roundtrip() {
        let storage = MemoryWikiStorage::new();
        let space = WikiSpace::new(SpaceId::default(), Arc::new(storage));

        let doc = space
            .create_doc("UndoTest", Block::new(
                BlockType::Paragraph,
                BlockContent::text("original"),
                "tester",
            ))
            .await
            .unwrap();
        let original_version = doc.version;

        // 执行一次修改。
        let root_id = doc.root.clone();
        let child = Block::new(
            BlockType::Paragraph,
            BlockContent::text("added child"),
            "tester",
        );
        let child_id = child.id().clone();
        let ops = vec![Op::InsertBlock {
            parent: root_id.clone(),
            after: None,
            block: child,
        }];
        let updated = space.apply_ops(&doc.id(), ops).await.unwrap();
        assert!(updated.block(&child_id).is_some());
        assert!(updated.version > original_version);

        // 撤销。
        let undone = space.undo(&doc.id).await.unwrap().unwrap();
        assert!(undone.block(&child_id).is_none(), "撤销后子块应不存在");
        assert_eq!(undone.version, original_version, "版本应回退");

        // 重做。
        let redone = space.redo(&doc.id).await.unwrap().unwrap();
        assert!(redone.block(&child_id).is_some(), "重做后子块应恢复");
        assert!(redone.version > original_version);

        // 再次撤销确认。
        let undone2 = space.undo(&doc.id).await.unwrap().unwrap();
        assert!(undone2.block(&child_id).is_none());

        // 无可重做时返回 None。
        let no_redo = space.undo(&doc.id).await.unwrap();
        assert!(no_redo.is_none(), "无可撤销内容时应返回 None");
    }

    /// 并发写入不产生数据损坏。
    #[tokio::test]
    async fn concurrent_writes_no_corruption() {
        let storage = MemoryWikiStorage::new();
        let space = Arc::new(WikiSpace::new(SpaceId::default(), Arc::new(storage)));

        let doc = space
            .create_doc("Concurrent", Block::new(
                BlockType::Paragraph,
                BlockContent::text("root"),
                "tester",
            ))
            .await
            .unwrap();
        let root = doc.root.clone();

        // 并发插入 N 个不同的子块。
        let space = Arc::new(space);
        let mut handles = vec![];
        for i in 0..10 {
            let s = space.clone();
            let r = root.clone();
            let did = doc.id.clone();
            handles.push(tokio::spawn(async move {
                let child = Block::new(
                    BlockType::Paragraph,
                    BlockContent::text(format!("concurrent-{i}")),
                    "tester",
                );
                let ops = vec![Op::InsertBlock {
                    parent: r,
                    after: None,
                    block: child,
                }];
                s.apply_ops(&did, ops).await
            }));
        }

        let mut inserted = 0;
        let mut errors = 0;
        for h in handles {
            match h.await.unwrap() {
                Ok(_) => inserted += 1,
                Err(_) => errors += 1,
            }
        }

        // 每个并发操作都处理了。
        assert!(inserted + errors > 0);

        // 重新加载文档验证完整性。
        let reloaded = space.get_doc(&doc.id).await.unwrap().unwrap();
        // 所有块的 parent 引用都有效。
        for block in &reloaded.blocks {
            if let Some(ref parent) = block.parent {
                assert!(
                    reloaded.block(parent).is_some(),
                    "并发写入后块的 parent 引用无效"
                );
            }
        }
    }
}

// ===========================================================================
// 权限存储（新增）
// ===========================================================================

#[derive(Default)]
struct MemPermissionStore {
    grants: Mutex<HashMap<(String, String), String>>,
}

#[async_trait]
impl PermissionStore for MemPermissionStore {
    async fn check(&self, _user: &str, _doc_id: &DocId, _action: Permission) -> Result<bool> {
        Ok(true) // testkit: allow all
    }
    async fn grant(&self, user: &str, doc_id: &DocId, role: &str) -> Result<()> {
        self.grants.lock().insert((user.to_string(), doc_id.0.clone()), role.to_string());
        Ok(())
    }
    async fn revoke(&self, user: &str, doc_id: &DocId) -> Result<()> {
        self.grants.lock().remove(&(user.to_string(), doc_id.0.clone()));
        Ok(())
    }
}

// ===========================================================================
// 事件总线（新增）
// ===========================================================================

#[derive(Default)]
struct MemEventBus {
    events: Mutex<Vec<DocEvent>>,
}

#[async_trait]
impl EventBus for MemEventBus {
    async fn publish(&self, event: DocEvent) -> Result<()> {
        self.events.lock().push(event);
        Ok(())
    }
    async fn subscribe(&self, _event_types: &[&str]) -> Result<Vec<Box<dyn EventStream>>> {
        Ok(Vec::new()) // testkit: no-op subscription
    }
}


