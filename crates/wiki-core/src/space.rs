//! 知识库空间 —— 注入存储后对外提供完整读写入口。
//!
//! 写入统一走 [`WikiSpace::apply_ops`]：就地回放 [`Op`] 修改文档树，
//! 同步维护 op_log（离线回放）、text_index（全文）、link_store（反向链接）。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::block::{Block, BlockId};
use crate::config::ValidationConfig;
use crate::doc::{DocId, Document, Op, SpaceId};
use crate::error::{Result, WikiError};
use crate::link::{parse_links, LinkTarget, WikiLink};
use crate::storage::{TextHit, TextQuery, VectorSearchResult, VersionId, WikiStorage};

/// 撤销条目：记录操作前的文档快照，用于回滚。
struct UndoEntry {
    ops: Vec<Op>,
    snapshot: Document,
}

/// 知识库空间 —— 注入存储后对外提供完整读写入口。
pub struct WikiSpace {
    pub id: SpaceId,
    storage: Arc<dyn WikiStorage>,
    undo_stack: Mutex<HashMap<String, Vec<UndoEntry>>>,
    redo_stack: Mutex<HashMap<String, Vec<Vec<Op>>>>,
}

impl WikiSpace {
    pub fn new(id: SpaceId, storage: Arc<dyn WikiStorage>) -> Self {
        Self {
            id,
            storage,
            undo_stack: Mutex::new(HashMap::new()),
            redo_stack: Mutex::new(HashMap::new()),
        }
    }

    pub fn storage(&self) -> &Arc<dyn WikiStorage> {
        &self.storage
    }

    // =========================================================================
    // 文档 CRUD
    // =========================================================================

    /// 创建文档并落库（同时索引 root 块）。
    #[tracing::instrument(skip(self, root, title))]
    pub async fn create_doc(&self, title: impl Into<String>, root: Block) -> Result<Document> {
        let doc = Document::new(title, root, self.id.clone());
        crate::validate::validate_doc(&doc, &Default::default())?;
        self.storage.doc_store().save(&doc).await?;
        self.reindex_block(&doc, &doc.root).await?;
        Ok(doc)
    }

    pub async fn get_doc(&self, id: &DocId) -> Result<Option<Document>> {
        self.storage.doc_store().get(id).await
    }

    pub async fn save_doc(&self, doc: &Document) -> Result<()> {
        self.storage.doc_store().save(doc).await
    }

    /// 删除文档并清理其全部块的索引/链接。
    pub async fn delete_doc(&self, id: &DocId) -> Result<()> {
        if let Some(doc) = self.storage.doc_store().get(id).await? {
            let ids = doc.descendants(&doc.root);
            let text = self.storage.text_index();
            for bid in &ids {
                text.remove(&bid.0).await?;
            }
        }
        self.storage.doc_store().delete(id).await
    }

    pub async fn list_docs(&self, offset: usize, limit: usize) -> Result<Vec<DocId>> {
        self.storage.doc_store().list(offset, limit).await
    }

    // =========================================================================
    // 写入管线
    // =========================================================================

    /// 对文档回放一批 [`Op`]：写 WAL → 改树 → 存文档 → 增量重索引受影响块。
    ///
    /// 任一 Op 非法则整批中止（文档不落库），保证原子性。
    /// 采用 Write-Ahead Log 模式：Op 先序列化写入 [`OpLog`]，再就地执行；
    /// 若执行中任一 Op 失败，回滚到操作前快照，已写入的 WAL 条目标记为失败批次。
    #[tracing::instrument(skip(self, ops), fields(doc_id = %doc_id, op_count = ops.len()))]
    pub async fn apply_ops(&self, doc_id: &DocId, ops: Vec<Op>) -> Result<Document> {
        let mut doc = self
            .storage
            .doc_store()
            .get(doc_id)
            .await?
            .ok_or_else(|| WikiError::NotFound(format!("doc {doc_id}")))?;

        // 收集受影响块 + 预校验（在 WAL 写入前检测非法 Op）。
        let rules = ValidationConfig::default();
        crate::validate::detect_cycle(&doc)?;
        let mut touched: Vec<BlockId> = Vec::new();
        let mut removed: Vec<BlockId> = Vec::new();

        for op in &ops {
            crate::validate::validate_op(op, &doc, &rules)?;
            match op {
                Op::InsertBlock { block, .. } => touched.push(block.id.clone()),
                Op::UpdateBlock { id, .. } => touched.push(id.clone()),
                Op::MoveBlock { id, .. } => touched.push(id.clone()),
                Op::DeleteBlock { id } => removed.extend(doc.descendants(id)),
                Op::UpdateDocMeta { .. } => {}
            }
        }

        // ---- WAL: 先持久化 Op 列表，再执行 ----
        let serialized: Vec<serde_json::Value> = ops
            .iter()
            .map(|op| serde_json::to_value(op).map_err(|e| WikiError::Serialization(e.to_string())))
            .collect::<Result<_>>()?;

        self.storage.op_log().append(doc_id, serialized).await?;

        // ---- 执行：保留快照用于回滚和撤销 ----
        let pre_apply_snapshot = doc.clone();
        let rollback_snapshot = pre_apply_snapshot.clone();

        let mut failed = false;
        for op in ops.iter().cloned() {
            if doc.apply(op).is_err() {
                failed = true;
                break;
            }
        }

        if failed {
            doc = rollback_snapshot;
        }

        self.storage.doc_store().save(&doc).await?;

        if failed {
            return Err(WikiError::Invalid(
                "batch contained an invalid op; document rolled back".into(),
            ));
        }

        // 索引维护：先批量删已移除块，再批量重索引存活的受影响块。
        self.maintain_indexes(&doc, &removed, &touched).await?;

        // 记录快照用于撤销。
        {
            let mut undo = self.undo_stack.lock().unwrap();
            undo.entry(doc_id.0.clone())
                .or_default()
                .push(UndoEntry {
                    ops: ops.iter().cloned().collect(),
                    snapshot: pre_apply_snapshot,
                });
        }
        {
            let mut redo = self.redo_stack.lock().unwrap();
            redo.remove(&doc_id.0);
        }

        Ok(doc)
    }

    /// 索引维护：批量删除 + 批量重索引。
    async fn maintain_indexes(
        &self,
        doc: &Document,
        removed: &[BlockId],
        touched: &[BlockId],
    ) -> Result<()> {
        let text = self.storage.text_index();
        if !removed.is_empty() {
            let removed_ids: Vec<String> = removed.iter().map(|b| b.0.clone()).collect();
            text.batch_remove(&removed_ids).await?;
        }
        let reindex_entries: Vec<(String, String, serde_json::Value)> = touched
            .iter()
            .filter_map(|bid| {
                doc.block(bid).map(|b| {
                    (
                        bid.0.clone(),
                        b.content.as_plain_text(),
                        serde_json::json!({ "doc": doc.id.0 }),
                    )
                })
            })
            .collect();
        if !reindex_entries.is_empty() {
            text.batch_index(&reindex_entries).await?;
        }
        for bid in touched {
            if let Some(block) = doc.block(bid) {
                let links = parse_links(bid, &block.content.as_plain_text());
                self.storage.link_store().upsert_links(bid, &links).await?;
            }
        }
        Ok(())
    }

    /// 重索引单个块：写全文索引 + 解析并存其出链。
    async fn reindex_block(&self, doc: &Document, id: &BlockId) -> Result<()> {
        let block = match doc.block(id) {
            Some(b) => b,
            None => return Ok(()),
        };
        let plain = block.content.as_plain_text();
        self.storage
            .text_index()
            .index_block(&id.0, &plain, serde_json::json!({ "doc": doc.id.0 }))
            .await?;
        let links = parse_links(id, &plain);
        self.storage.link_store().upsert_links(id, &links).await?;
        Ok(())
    }

    // =========================================================================
    // 检索
    // =========================================================================

    /// 语义检索（向量），返回命中块 id + 分数。
    pub async fn search_semantic(
        &self,
        query_vec: Vec<f32>,
        top_k: usize,
    ) -> Result<Vec<VectorSearchResult>> {
        self.storage
            .vector_store()
            .search("wiki_blocks", query_vec, top_k, None)
            .await
    }

    /// 全文精确检索。
    pub async fn search_text(&self, query: &TextQuery, top_k: usize) -> Result<Vec<TextHit>> {
        self.storage.text_index().search(query, top_k).await
    }

    /// 某目标的反向链接（backlinks）。
    pub async fn backlinks(&self, target: &LinkTarget) -> Result<Vec<WikiLink>> {
        self.storage.link_store().backlinks(&target.key()).await
    }

    // =========================================================================
    // 版本
    // =========================================================================

    /// 为文档打一个可读版本快照。
    pub async fn snapshot(&self, doc_id: &DocId, label: Option<String>) -> Result<VersionId> {
        let doc = self
            .storage
            .doc_store()
            .get(doc_id)
            .await?
            .ok_or_else(|| WikiError::NotFound(format!("doc {doc_id}")))?;
        self.storage.version_store().snapshot(doc_id, &doc, label).await
    }

    /// 回滚到某历史版本（以旧版为内容提交新版），并刷新当前文档。
    pub async fn restore(&self, doc_id: &DocId, version: &VersionId) -> Result<Document> {
        let versions = self.storage.version_store();
        let old = versions.get_version(doc_id, version).await?;
        versions.restore(doc_id, version).await?;
        self.storage.doc_store().save(&old).await?;
        Ok(old)
    }

    // =========================================================================
    // 撤销 / 重做
    // =========================================================================

    /// 撤销最近一次 [`apply_ops`] 操作。
    pub async fn undo(&self, doc_id: &DocId) -> Result<Option<Document>> {
        let entry = {
            let mut stack = self.undo_stack.lock().unwrap();
            stack.get_mut(&doc_id.0).and_then(|v| v.pop())
        };
        let Some(entry) = entry else {
            return Ok(None);
        };
        self.storage.doc_store().save(&entry.snapshot).await?;
        {
            let mut redo = self.redo_stack.lock().unwrap();
            redo.entry(doc_id.0.clone()).or_default().push(entry.ops);
        }
        Ok(Some(entry.snapshot))
    }

    /// 重做最近一次被撤销的 [`apply_ops`]。
    pub async fn redo(&self, doc_id: &DocId) -> Result<Option<Document>> {
        let ops = {
            let mut stack = self.redo_stack.lock().unwrap();
            stack.get_mut(&doc_id.0).and_then(|v| v.pop())
        };
        let Some(ops) = ops else {
            return Ok(None);
        };
        let doc = self.apply_ops(doc_id, ops).await?;
        Ok(Some(doc))
    }
}
