//! # wiki-testkit
//!
//! 全部 7 个 `WikiStorage` 端口的内存参考实现，仅用于测试/开发。
//! 生产环境由 `agent-context-db` 注入真实后端。

use async_trait::async_trait;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use wiki_core::storage::{
    BlobId, BlobStore, BlockChange, ChangeKind, DocDiff, DocStore, DocVersionStore, LinkStore,
    MatchMode, OpLog, TextHit, TextIndex, TextQuery, VectorSearchResult, VectorStore, VersionEntry,
    VersionId, WikiStorage,
};
use wiki_core::{BlockId, DocId, Document, Result, WikiError, WikiLink};

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

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na * nb)
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
        _filter: Option<serde_json::Value>,
    ) -> Result<Vec<VectorSearchResult>> {
        let data = self.data.lock();
        let mut hits: Vec<VectorSearchResult> = data
            .get(collection)
            .map(|c| {
                c.iter()
                    .map(|(id, (vec, meta))| VectorSearchResult {
                        id: id.clone(),
                        score: cosine(&query, vec),
                        metadata: meta.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        hits.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap());
        hits.truncate(top_k);
        Ok(hits)
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
    // block_id -> text
    idx: Mutex<HashMap<String, String>>,
}

#[async_trait]
impl TextIndex for MemTextIndex {
    async fn index_block(
        &self,
        block_id: &str,
        text: &str,
        _meta: serde_json::Value,
    ) -> Result<()> {
        self.idx.lock().insert(block_id.to_string(), text.to_lowercase());
        Ok(())
    }

    async fn search(&self, query: &TextQuery, top_k: usize) -> Result<Vec<TextHit>> {
        let idx = self.idx.lock();
        let mut hits = Vec::new();
        for (id, text) in idx.iter() {
            let matched = match query.mode {
                MatchMode::Exact => query.terms.iter().all(|t| text.contains(&t.to_lowercase())),
                MatchMode::Prefix => query
                    .terms
                    .iter()
                    .any(|t| text.split_whitespace().any(|w| w.starts_with(&t.to_lowercase()))),
                MatchMode::Fuzzy => query.terms.iter().any(|t| text.contains(&t.to_lowercase())),
            };
            let phrase_ok = query
                .phrase
                .as_ref()
                .map(|p| text.contains(&p.to_lowercase()))
                .unwrap_or(true);
            if matched && phrase_ok {
                hits.push(TextHit {
                    block_id: id.clone(),
                    snippet: text.chars().take(80).collect(),
                    score: 1.0,
                });
            }
        }
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
        let ids_a: Vec<&BlockId> = va.blocks.iter().map(|blk| &blk.id).collect();
        let ids_b: Vec<&BlockId> = vb.blocks.iter().map(|blk| &blk.id).collect();

        let mut changes = Vec::new();
        for blk in &vb.blocks {
            if !ids_a.contains(&&blk.id) {
                changes.push(BlockChange {
                    block_id: blk.id.0.clone(),
                    kind: ChangeKind::Added,
                });
            }
        }
        for blk in &va.blocks {
            if !ids_b.contains(&&blk.id) {
                changes.push(BlockChange {
                    block_id: blk.id.0.clone(),
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
    use wiki_core::{Block, BlockContent, BlockType, SpaceId};

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

        let v1 = vs.snapshot(&doc.id, &doc, Some("v1".into())).await.unwrap();

        // 加一个 block 再快照
        let extra = Block::new(BlockType::Paragraph, BlockContent::text("more"), "a");
        doc.blocks.push(extra);
        let v2 = vs.snapshot(&doc.id, &doc, Some("v2".into())).await.unwrap();

        assert_eq!(vs.list_versions(&doc.id).await.unwrap().len(), 2);

        let diff = vs.diff(&doc.id, &v1, &v2).await.unwrap();
        assert_eq!(diff.changes.len(), 1);
        assert_eq!(diff.changes[0].kind, ChangeKind::Added);

        // restore 生成第三个版本
        vs.restore(&doc.id, &v1).await.unwrap();
        assert_eq!(vs.list_versions(&doc.id).await.unwrap().len(), 3);
    }
}
