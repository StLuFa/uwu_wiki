//! # wiki-core
//!
//! uwu_wiki 核心：Block 引擎 + Document/Op 模型 + 全部存储/LLM 端口 **trait 定义**。
//!
//! ## 设计约束（见 ARCHITECTURE.md §1.5）
//!
//! - **核心纯粹性**：除 serde/uuid/chrono 外零依赖；**不含存储/LLM 实现**。
//! - **端口/适配器**：全部存储能力以 trait（端口）暴露，实现由宿主注入。
//! - **单向依赖**：`wiki-core` 不依赖任何其他 wiki-* crate 或引擎。
//!
//! 参考实现见 `wiki-testkit`（dev-dependency）；生产由 `agent-context-db` 注入。

pub mod block;
pub mod doc;
pub mod error;
pub mod link;
pub mod registry;
pub mod storage;

pub use block::{Block, BlockContent, BlockId, BlockMeta, BlockType};
pub use doc::{DocId, Document, Op, SpaceId};
pub use error::{Result, WikiError};
pub use link::{parse_links, resolve_links, LinkGraph, LinkTarget, WikiLink};
pub use registry::{BlockTypeRegistry, MarkdownRenderer, Render};
pub use storage::{
    BlobId, BlobStore, BlockChange, ChangeKind, DocDiff, DocStore, DocVersionStore, LinkStore,
    MatchMode, OpLog, TextHit, TextIndex, TextQuery, VectorSearchResult, VectorStore, VersionEntry,
    VersionId, WikiStorage,
};

use std::sync::Arc;

/// 知识库空间 —— 注入存储后对外提供完整读写入口。
///
/// 写入统一走 [`WikiSpace::apply_ops`]：就地回放 [`Op`] 修改文档树，
/// 同步维护 op_log（离线回放）、text_index（全文）、link_store（反向链接）。
pub struct WikiSpace {
    pub id: SpaceId,
    storage: Arc<dyn WikiStorage>,
}

impl WikiSpace {
    pub fn new(id: SpaceId, storage: Arc<dyn WikiStorage>) -> Self {
        Self { id, storage }
    }

    pub fn storage(&self) -> &Arc<dyn WikiStorage> {
        &self.storage
    }

    // ---- 文档 CRUD ----

    /// 创建文档并落库（同时索引 root 块）。
    pub async fn create_doc(&self, title: impl Into<String>, root: Block) -> Result<Document> {
        let doc = Document::new(title, root, self.id.clone());
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

    // ---- 写入管线 ----

    /// 对文档回放一批 [`Op`]：改树 → 存文档 → 写 op_log → 增量重索引受影响块。
    ///
    /// 任一 Op 非法则整批中止（文档不落库），保证原子性。
    pub async fn apply_ops(&self, doc_id: &DocId, ops: Vec<Op>) -> Result<Document> {
        let mut doc = self
            .storage
            .doc_store()
            .get(doc_id)
            .await?
            .ok_or_else(|| WikiError::NotFound(format!("doc {doc_id}")))?;

        // 收集受影响块，用于回放后增量重索引。
        let mut touched: Vec<BlockId> = Vec::new();
        let mut removed: Vec<BlockId> = Vec::new();

        for op in &ops {
            match op {
                Op::InsertBlock { block, .. } => touched.push(block.id.clone()),
                Op::UpdateBlock { id, .. } => touched.push(id.clone()),
                Op::MoveBlock { id, .. } => touched.push(id.clone()),
                Op::DeleteBlock { id } => removed.extend(doc.descendants(id)),
                Op::UpdateDocMeta { .. } => {}
            }
        }

        for op in ops.iter().cloned() {
            doc.apply(op)?;
        }

        self.storage.doc_store().save(&doc).await?;

        // op_log：序列化整批。
        let serialized: Vec<serde_json::Value> = ops
            .iter()
            .map(|op| serde_json::to_value(op).map_err(|e| WikiError::Serialization(e.to_string())))
            .collect::<Result<_>>()?;
        self.storage.op_log().append(doc_id, serialized).await?;

        // 索引维护：先删已移除块，再重索引存活的受影响块。
        let text = self.storage.text_index();
        for bid in &removed {
            text.remove(&bid.0).await?;
        }
        for bid in &touched {
            if doc.block(bid).is_some() {
                self.reindex_block(&doc, bid).await?;
            }
        }

        Ok(doc)
    }

    /// 重索引单个块：写全文索引 + 解析并存其出链。
    async fn reindex_block(&self, doc: &Document, id: &BlockId) -> Result<()> {
        let block = match doc.block(id) {
            Some(b) => b,
            None => return Ok(()),
        };
        let plain = block.content.as_plain_text();

        // 全文索引。
        self.storage
            .text_index()
            .index_block(&id.0, &plain, serde_json::json!({ "doc": doc.id.0 }))
            .await?;

        // 出链解析 + 存储（悬空目标由 Lint 处理）。
        let links = parse_links(id, &plain);
        self.storage.link_store().upsert_links(id, &links).await?;
        Ok(())
    }

    // ---- 检索 ----

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

    // ---- 版本 ----

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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_version_and_staleness() {
        let mut b = Block::new(BlockType::Paragraph, BlockContent::text("hello"), "agent-1");
        assert!(!b.is_embedding_stale());

        b.embedding = Some(vec![0.1, 0.2]);
        b.embedding_version = b.version;
        assert!(!b.is_embedding_stale());

        b.bump_version();
        assert!(b.is_embedding_stale(), "内容更新后 embedding 应标记陈旧");
    }

    #[test]
    fn document_block_lookup() {
        let root = Block::new(BlockType::Paragraph, BlockContent::text("root"), "a");
        let root_id = root.id.clone();
        let doc = Document::new("Test", root, SpaceId::default());
        assert!(doc.block(&root_id).is_some());
        assert_eq!(doc.root, root_id);
    }
}
