//! Op 队列合并 + CRDT 合并计算（uwu-crdt）。
//!
//! 把 wiki-core 的领域操作 [`Op`] 翻译为 uwu-crdt 的 [`UwuOp`]，交由
//! [`UwuCrdtDoc`] 做无冲突合并。合并后的状态与增量由调用方持久化到
//! `WikiStorage`（本模块不持有存储，DB 是唯一真相源）。
//!
//! # 翻译对照
//!
//! | wiki-core `Op` | uwu-crdt `UwuOp` |
//! |---|---|
//! | `InsertBlock { parent, after, block }` | `Insert { id, parent, after, data }` |
//! | `UpdateBlock { id, patch }` | `Update { id, patch }` |
//! | `DeleteBlock { id }` | `Delete { id }` |
//! | `MoveBlock { id, new_parent, after }` | `Move { id, new_parent, after }` |
//! | `UpdateDocMeta { .. }` | 文档级元数据，不进 Block 树 CRDT（返回 `None`） |
//!
//! `Block` 负载序列化进 CRDT 节点 `data`；`BlockId` 直接作为 CRDT 的 `NodeId`。
//! 根块的 `parent` 为 `None`（映射到 Loro 树根）。

use serde::{Deserialize, Serialize};
use uwu_crdt::{NodeId, UwuCrdtDoc, UwuOp};
use wiki_core::{Block, BlockId, Op};

/// 协作同步错误。
#[derive(Debug, Serialize, Deserialize)]
pub enum SyncError {
    /// Block 负载序列化失败。
    Serialize(String),
    /// 底层 CRDT 合并错误。
    Crdt(String),
    /// Op 无法翻译（如文档级 meta 不进 Block 树）。
    NotTranslatable(String),
}

impl std::fmt::Display for SyncError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SyncError::Serialize(s) => write!(f, "serialize: {s}"),
            SyncError::Crdt(s) => write!(f, "crdt: {s}"),
            SyncError::NotTranslatable(s) => write!(f, "not translatable: {s}"),
        }
    }
}

impl std::error::Error for SyncError {}

impl From<uwu_crdt::UwuCrdtError> for SyncError {
    fn from(e: uwu_crdt::UwuCrdtError) -> Self {
        SyncError::Crdt(e.to_string())
    }
}

type Result<T> = std::result::Result<T, SyncError>;

fn node_id(id: &BlockId) -> NodeId {
    NodeId(id.0.clone())
}

/// 把单个 wiki-core [`Op`] 翻译为一个 [`UwuOp`]。
///
/// `root` 用于识别根块——根块的 parent 在 CRDT 中映射为 `None`。
/// 返回 `Ok(None)` 表示该 Op 不进 Block 树 CRDT（如 `UpdateDocMeta`），
/// 由调用方在文档元数据层面另行处理。
pub fn translate_op(op: &Op, root: &BlockId) -> Result<Option<UwuOp>> {
    let translated = match op {
        Op::InsertBlock { parent, after, block } => {
            let data = serde_json::to_value(block)
                .map_err(|e| SyncError::Serialize(e.to_string()))?;
            UwuOp::Insert {
                id: node_id(&block.id),
                parent: block_parent(parent, root),
                after: after.as_ref().map(node_id),
                data,
            }
        }
        Op::UpdateBlock { id, patch } => UwuOp::Update {
            id: node_id(id),
            patch: patch.clone(),
        },
        Op::DeleteBlock { id } => UwuOp::Delete { id: node_id(id) },
        Op::MoveBlock { id, new_parent, after } => UwuOp::Move {
            id: node_id(id),
            new_parent: block_parent(new_parent, root),
            after: after.as_ref().map(node_id),
        },
        Op::UpdateDocMeta { .. } => {
            return Ok(None);
        }
    };
    Ok(Some(translated))
}

/// 根块的 parent 映射为 `None`（Loro 树根），其余为 `Some(NodeId)`。
fn block_parent(parent: &BlockId, root: &BlockId) -> Option<NodeId> {
    if parent == root {
        None
    } else {
        Some(node_id(parent))
    }
}

/// 协作文档：包裹一个 [`UwuCrdtDoc`]，提供 wiki 语义的 Op 应用与快照/增量同步。
///
/// 不持有存储：`snapshot()` / `updates_since()` 产出的字节由调用方落库
/// （`WikiStorage::doc_store` / `op_log`）并经 `uwu_event_mesh` 广播。
pub struct CollabDoc {
    crdt: UwuCrdtDoc,
    root: BlockId,
}

impl CollabDoc {
    /// 以指定 `peer_id`（区分并发副本）和根块创建协作文档。
    ///
    /// 会先把根块作为 CRDT 根节点插入。
    pub fn new(peer_id: u64, root: Block) -> Result<Self> {
        let root_id = root.id.clone();
        let mut crdt = UwuCrdtDoc::new(peer_id);
        let data = serde_json::to_value(&root)
            .map_err(|e| SyncError::Serialize(e.to_string()))?;
        crdt.apply_ops(&[UwuOp::Insert {
            id: node_id(&root_id),
            parent: None,
            after: None,
            data,
        }])?;
        Ok(Self { crdt, root: root_id })
    }

    /// 从已有 CRDT 快照重建协作文档（新副本加入协作时用）。
    pub fn from_snapshot(peer_id: u64, root: BlockId, snapshot: &[u8]) -> Result<Self> {
        let mut crdt = UwuCrdtDoc::new(peer_id);
        crdt.import(snapshot)?;
        Ok(Self { crdt, root })
    }

    /// 应用一批 wiki-core [`Op`]（翻译 + CRDT 合并 + commit）。
    ///
    /// `UpdateDocMeta` 会被跳过（不进 Block 树），返回其数量供调用方在文档
    /// 元数据层面另行处理。
    pub fn apply_ops(&mut self, ops: &[Op]) -> Result<usize> {
        let mut uwu_ops = Vec::with_capacity(ops.len());
        let mut skipped = 0;
        for op in ops {
            match translate_op(op, &self.root)? {
                Some(u) => uwu_ops.push(u),
                None => skipped += 1,
            }
        }
        self.crdt.apply_ops(&uwu_ops)?;
        Ok(skipped)
    }

    /// 合并另一副本导出的字节（快照或增量）。
    pub fn merge(&mut self, bytes: &[u8]) -> Result<()> {
        self.crdt.import(bytes)?;
        Ok(())
    }

    /// 全量快照（落 doc_store / 新副本加载）。
    pub fn snapshot(&self) -> Result<Vec<u8>> {
        Ok(self.crdt.export_snapshot()?)
    }

    /// 自 `since`（对端版本向量编码）以来的增量（写 op_log / 广播）。
    pub fn updates_since(&self, since: Option<&[u8]>) -> Result<Vec<u8>> {
        Ok(self.crdt.export_updates(since)?)
    }

    /// 当前版本向量编码，供对端下次增量导出。
    pub fn version(&self) -> Vec<u8> {
        self.crdt.version()
    }

    /// 读取某块的当前负载（反序列化回 [`Block`]）。
    pub fn block(&self, id: &BlockId) -> Result<Block> {
        let val = self.crdt.get(&node_id(id))?;
        serde_json::from_value(val).map_err(|e| SyncError::Serialize(e.to_string()))
    }

    /// 当前存活块数。
    pub fn len(&self) -> usize {
        self.crdt.len()
    }

    pub fn is_empty(&self) -> bool {
        self.crdt.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiki_core::{BlockContent, BlockType};

    fn para(text: &str) -> Block {
        Block::new(BlockType::Paragraph, BlockContent::text(text), "tester")
    }

    fn insert(parent: &BlockId, after: Option<&BlockId>, block: Block) -> Op {
        Op::InsertBlock {
            parent: parent.clone(),
            after: after.cloned(),
            block,
        }
    }

    #[test]
    fn translate_covers_block_ops() {
        let root = BlockId::new();
        let child = para("c");
        let ins = insert(&root, None, child.clone());
        let u = translate_op(&ins, &root).unwrap().unwrap();
        matches!(u, UwuOp::Insert { .. });

        let upd = Op::UpdateBlock { id: child.id.clone(), patch: serde_json::json!({"x":1}) };
        matches!(translate_op(&upd, &root).unwrap().unwrap(), UwuOp::Update { .. });

        let del = Op::DeleteBlock { id: child.id.clone() };
        matches!(translate_op(&del, &root).unwrap().unwrap(), UwuOp::Delete { .. });
    }

    #[test]
    fn doc_meta_op_is_skipped() {
        let root = BlockId::new();
        let op = Op::UpdateDocMeta {
            doc_id: wiki_core::DocId("d".into()),
            patch: serde_json::json!({"title":"x"}),
        };
        assert!(translate_op(&op, &root).unwrap().is_none());
    }

    #[test]
    fn create_and_apply_reads_back_block() {
        let root = para("root");
        let root_id = root.id.clone();
        let mut doc = CollabDoc::new(1, root).unwrap();
        assert_eq!(doc.len(), 1);

        let child = para("hello");
        let cid = child.id.clone();
        let skipped = doc.apply_ops(&[insert(&root_id, None, child)]).unwrap();
        assert_eq!(skipped, 0);
        assert_eq!(doc.len(), 2);

        let read = doc.block(&cid).unwrap();
        assert_eq!(read.content.as_plain_text(), "hello");
    }

    #[test]
    fn doc_meta_op_skipped_and_counted() {
        let root = para("root");
        let root_id = root.id.clone();
        let mut doc = CollabDoc::new(1, root).unwrap();
        let ops = vec![
            insert(&root_id, None, para("a")),
            Op::UpdateDocMeta {
                doc_id: wiki_core::DocId("d".into()),
                patch: serde_json::json!({"title":"X"}),
            },
        ];
        let skipped = doc.apply_ops(&ops).unwrap();
        assert_eq!(skipped, 1);
        assert_eq!(doc.len(), 2); // root + a
    }

    #[test]
    fn two_peers_converge() {
        // peer A 建文档并加子块，B 从快照加入，各自并发插入，交换增量后收敛。
        let root = para("root");
        let root_id = root.id.clone();
        let mut a = CollabDoc::new(1, root).unwrap();

        let snap = a.snapshot().unwrap();
        let mut b = CollabDoc::from_snapshot(2, root_id.clone(), &snap).unwrap();

        let ca = para("from-a");
        let ca_id = ca.id.clone();
        a.apply_ops(&[insert(&root_id, None, ca)]).unwrap();

        let cb = para("from-b");
        let cb_id = cb.id.clone();
        b.apply_ops(&[insert(&root_id, None, cb)]).unwrap();

        // 交换全量增量。
        let a_up = a.updates_since(None).unwrap();
        let b_up = b.updates_since(None).unwrap();
        a.merge(&b_up).unwrap();
        b.merge(&a_up).unwrap();

        // 收敛：root + from-a + from-b。
        assert_eq!(a.len(), 3);
        assert_eq!(b.len(), 3);
        assert_eq!(a.block(&cb_id).unwrap().content.as_plain_text(), "from-b");
        assert_eq!(b.block(&ca_id).unwrap().content.as_plain_text(), "from-a");
    }

    #[test]
    fn incremental_updates_since_version() {
        let root = para("root");
        let root_id = root.id.clone();
        let mut a = CollabDoc::new(1, root).unwrap();
        let v1 = a.version();

        let child = para("later");
        a.apply_ops(&[insert(&root_id, None, child)]).unwrap();
        let delta = a.updates_since(Some(&v1)).unwrap();

        // 另一副本先拿全量再拿增量，收敛到 2 块。
        let mut b = CollabDoc::from_snapshot(2, root_id, &a.snapshot().unwrap()).unwrap();
        b.merge(&delta).unwrap();
        assert_eq!(b.len(), 2);
    }
}
