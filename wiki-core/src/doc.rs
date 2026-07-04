//! Document 模型 + Op 操作枚举。

use crate::block::{Block, BlockId};
use crate::error::{Result, WikiError};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 文档唯一标识。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DocId(pub String);

impl DocId {
    pub fn new() -> Self {
        Self(Uuid::now_v7().to_string())
    }
}

impl Default for DocId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for DocId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// 空间（多租户/多知识库隔离单元）。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SpaceId(pub String);

impl Default for SpaceId {
    fn default() -> Self {
        Self("default".into())
    }
}

/// 结构化文档 —— Block 树。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Document {
    pub id: DocId,
    pub title: String,
    pub root: BlockId,
    pub version: u64,
    pub space_id: SpaceId,
    pub tags: Vec<String>,
    pub icon: Option<String>,
    pub cover: Option<String>,
    /// 文档内全部 Block（以 `root` 为树根）。
    pub blocks: Vec<Block>,
}

impl Document {
    pub fn new(title: impl Into<String>, root: Block, space_id: SpaceId) -> Self {
        let root_id = root.id.clone();
        Self {
            id: DocId::new(),
            title: title.into(),
            root: root_id,
            version: 0,
            space_id,
            tags: Vec::new(),
            icon: None,
            cover: None,
            blocks: vec![root],
        }
    }

    pub fn block(&self, id: &BlockId) -> Option<&Block> {
        self.blocks.iter().find(|b| &b.id == id)
    }

    pub fn block_mut(&mut self, id: &BlockId) -> Option<&mut Block> {
        self.blocks.iter_mut().find(|b| &b.id == id)
    }

    /// 直接子块（按 `children` 顺序）。
    pub fn children(&self, parent: &BlockId) -> Vec<&Block> {
        match self.block(parent) {
            Some(p) => p
                .children
                .iter()
                .filter_map(|cid| self.block(cid))
                .collect(),
            None => Vec::new(),
        }
    }

    /// 前序遍历子树（含 `root`），返回 BlockId 顺序。
    pub fn descendants(&self, root: &BlockId) -> Vec<BlockId> {
        let mut out = Vec::new();
        self.walk(root, &mut |b| out.push(b.id.clone()));
        out
    }

    /// 最后一个子块 ID（InsertBlock after 用）。
    pub fn last_child(&self, parent: &BlockId) -> Option<BlockId> {
        self.block(parent)
            .and_then(|p| p.children.last().cloned())
    }

    /// 文档的最后更新时间（取所有 Block 中最新的 `updated_at`）。
    pub fn updated_at(&self) -> chrono::DateTime<chrono::Utc> {
        self.blocks
            .iter()
            .map(|b| b.meta.updated_at)
            .max()
            .unwrap_or_else(chrono::Utc::now)
    }

    /// 前序遍历子树（含 `root`），对每个 Block 调用 `f`。
    pub fn walk(&self, root: &BlockId, f: &mut impl FnMut(&Block)) {
        if let Some(b) = self.block(root) {
            f(b);
            for child in b.children.clone() {
                self.walk(&child, f);
            }
        }
    }

    /// 该块是否为 `ancestor` 的后代（用于 Move 防环）。
    fn is_descendant_of(&self, node: &BlockId, ancestor: &BlockId) -> bool {
        if node == ancestor {
            return true;
        }
        match self.block(ancestor) {
            Some(a) => a.children.iter().any(|c| self.is_descendant_of(node, c)),
            None => false,
        }
    }

    /// 在 `parent.children` 中把 `child` 放到 `after` 之后（`after=None` 放最前）。
    fn place_child(&mut self, parent: &BlockId, child: &BlockId, after: Option<&BlockId>) -> Result<()> {
        let p = self
            .block_mut(parent)
            .ok_or_else(|| WikiError::NotFound(format!("parent block {parent}")))?;
        p.children.retain(|c| c != child);
        let pos = match after {
            None => 0,
            Some(a) => p
                .children
                .iter()
                .position(|c| c == a)
                .map(|i| i + 1)
                .ok_or_else(|| WikiError::Invalid(format!("after block {a} not under parent")))?,
        };
        p.children.insert(pos, child.clone());
        Ok(())
    }

    /// 应用一个 [`Op`]，就地修改文档树并推进版本。
    ///
    /// 这是 wiki-core 的写入真相：CRDT 合并后由宿主回放 Op 得到相同状态。
    pub fn apply(&mut self, op: Op) -> Result<()> {
        match op {
            Op::InsertBlock { parent, after, block } => {
                if self.block(&parent).is_none() {
                    return Err(WikiError::NotFound(format!("parent block {parent}")));
                }
                if self.block(&block.id).is_some() {
                    return Err(WikiError::Invalid(format!("block {} already exists", block.id)));
                }
                let mut block = block;
                block.parent = Some(parent.clone());
                let child_id = block.id.clone();
                self.blocks.push(block);
                self.place_child(&parent, &child_id, after.as_ref())?;
            }
            Op::UpdateBlock { id, patch } => {
                let block = self
                    .block_mut(&id)
                    .ok_or_else(|| WikiError::NotFound(format!("block {id}")))?;
                merge_content(&mut block.content.0, &patch);
                block.bump_version();
            }
            Op::DeleteBlock { id } => {
                if id == self.root {
                    return Err(WikiError::Invalid("cannot delete root block".into()));
                }
                if self.block(&id).is_none() {
                    return Err(WikiError::NotFound(format!("block {id}")));
                }
                // 收集整棵子树后统一删除。
                let to_remove = self.descendants(&id);
                if let Some(parent_id) = self.block(&id).and_then(|b| b.parent.clone())
                    && let Some(p) = self.block_mut(&parent_id)
                {
                    p.children.retain(|c| c != &id);
                }
                self.blocks.retain(|b| !to_remove.contains(&b.id));
            }
            Op::MoveBlock { id, new_parent, after } => {
                if id == self.root {
                    return Err(WikiError::Invalid("cannot move root block".into()));
                }
                if self.block(&id).is_none() {
                    return Err(WikiError::NotFound(format!("block {id}")));
                }
                if self.block(&new_parent).is_none() {
                    return Err(WikiError::NotFound(format!("new parent {new_parent}")));
                }
                // 防止把节点移进自己的子树造成环。
                if self.is_descendant_of(&new_parent, &id) {
                    return Err(WikiError::Invalid(
                        "cannot move a block into its own subtree".into(),
                    ));
                }
                // 从旧父摘除。
                if let Some(old_parent) = self.block(&id).and_then(|b| b.parent.clone())
                    && let Some(p) = self.block_mut(&old_parent)
                {
                    p.children.retain(|c| c != &id);
                }
                if let Some(b) = self.block_mut(&id) {
                    b.parent = Some(new_parent.clone());
                }
                self.place_child(&new_parent, &id, after.as_ref())?;
            }
            Op::UpdateDocMeta { doc_id, patch } => {
                if doc_id != self.id {
                    return Err(WikiError::Invalid(format!(
                        "doc meta op targets {doc_id}, not {}",
                        self.id
                    )));
                }
                apply_doc_meta(self, &patch);
            }
        }
        self.version += 1;
        Ok(())
    }
}

/// 顶层字段浅合并到 Block 内容 JSON。
fn merge_content(target: &mut serde_json::Value, patch: &serde_json::Value) {
    match (target, patch) {
        (serde_json::Value::Object(t), serde_json::Value::Object(p)) => {
            for (k, v) in p {
                t.insert(k.clone(), v.clone());
            }
        }
        (t, p) => *t = p.clone(),
    }
}

/// 应用文档级 meta patch（title / tags / icon / cover）。
fn apply_doc_meta(doc: &mut Document, patch: &serde_json::Value) {
    if let Some(t) = patch.get("title").and_then(|v| v.as_str()) {
        doc.title = t.to_string();
    }
    if let Some(icon) = patch.get("icon").and_then(|v| v.as_str()) {
        doc.icon = Some(icon.to_string());
    }
    if let Some(cover) = patch.get("cover").and_then(|v| v.as_str()) {
        doc.cover = Some(cover.to_string());
    }
    if let Some(tags) = patch.get("tags").and_then(|v| v.as_array()) {
        doc.tags = tags
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect();
    }
}

/// 写操作 —— CRDT 合并输入、事件消息体、审计日志三合一。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Op {
    InsertBlock {
        parent: BlockId,
        after: Option<BlockId>,
        block: Block,
    },
    UpdateBlock {
        id: BlockId,
        patch: serde_json::Value,
    },
    DeleteBlock {
        id: BlockId,
    },
    MoveBlock {
        id: BlockId,
        new_parent: BlockId,
        after: Option<BlockId>,
    },
    UpdateDocMeta {
        doc_id: DocId,
        patch: serde_json::Value,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::{BlockContent, BlockType};

    fn para(text: &str) -> Block {
        Block::new(BlockType::Paragraph, BlockContent::text(text), "tester")
    }

    fn doc_with_root() -> (Document, BlockId) {
        let root = para("root");
        let root_id = root.id.clone();
        (Document::new("T", root, SpaceId::default()), root_id)
    }

    #[test]
    fn insert_block_appends_child() {
        let (mut doc, root) = doc_with_root();
        let child = para("c1");
        let cid = child.id.clone();
        doc.apply(Op::InsertBlock { parent: root.clone(), after: None, block: child }).unwrap();
        assert_eq!(doc.children(&root).len(), 1);
        assert_eq!(doc.block(&cid).unwrap().parent, Some(root));
    }

    #[test]
    fn insert_after_orders_siblings() {
        let (mut doc, root) = doc_with_root();
        let a = para("a");
        let b = para("b");
        let (aid, bid) = (a.id.clone(), b.id.clone());
        doc.apply(Op::InsertBlock { parent: root.clone(), after: None, block: a }).unwrap();
        doc.apply(Op::InsertBlock { parent: root.clone(), after: Some(aid.clone()), block: b }).unwrap();
        let order: Vec<_> = doc.children(&root).iter().map(|b| b.id.clone()).collect();
        assert_eq!(order, vec![aid, bid]);
    }

    #[test]
    fn update_block_merges_content_and_bumps_version() {
        let (mut doc, root) = doc_with_root();
        let v0 = doc.block(&root).unwrap().version;
        doc.apply(Op::UpdateBlock {
            id: root.clone(),
            patch: serde_json::json!({ "bold": true }),
        })
        .unwrap();
        let b = doc.block(&root).unwrap();
        assert_eq!(b.content.0.get("text").unwrap(), "root");
        assert_eq!(b.content.0.get("bold").unwrap(), true);
        assert_eq!(b.version, v0 + 1);
    }

    #[test]
    fn delete_removes_whole_subtree() {
        let (mut doc, root) = doc_with_root();
        let parent = para("p");
        let pid = parent.id.clone();
        let child = para("c");
        let cid = child.id.clone();
        doc.apply(Op::InsertBlock { parent: root.clone(), after: None, block: parent }).unwrap();
        doc.apply(Op::InsertBlock { parent: pid.clone(), after: None, block: child }).unwrap();
        doc.apply(Op::DeleteBlock { id: pid.clone() }).unwrap();
        assert!(doc.block(&pid).is_none());
        assert!(doc.block(&cid).is_none(), "子块应随父块删除");
        assert!(doc.children(&root).is_empty());
    }

    #[test]
    fn cannot_delete_root() {
        let (mut doc, root) = doc_with_root();
        assert!(doc.apply(Op::DeleteBlock { id: root }).is_err());
    }

    #[test]
    fn move_block_reparents() {
        let (mut doc, root) = doc_with_root();
        let p1 = para("p1");
        let p2 = para("p2");
        let (p1id, p2id) = (p1.id.clone(), p2.id.clone());
        let c = para("c");
        let cid = c.id.clone();
        doc.apply(Op::InsertBlock { parent: root.clone(), after: None, block: p1 }).unwrap();
        doc.apply(Op::InsertBlock { parent: root.clone(), after: None, block: p2 }).unwrap();
        doc.apply(Op::InsertBlock { parent: p1id.clone(), after: None, block: c }).unwrap();

        doc.apply(Op::MoveBlock { id: cid.clone(), new_parent: p2id.clone(), after: None }).unwrap();
        assert!(doc.children(&p1id).is_empty());
        assert_eq!(doc.children(&p2id)[0].id, cid);
        assert_eq!(doc.block(&cid).unwrap().parent, Some(p2id));
    }

    #[test]
    fn move_into_own_subtree_rejected() {
        let (mut doc, root) = doc_with_root();
        let p = para("p");
        let pid = p.id.clone();
        let c = para("c");
        let cid = c.id.clone();
        doc.apply(Op::InsertBlock { parent: root, after: None, block: p }).unwrap();
        doc.apply(Op::InsertBlock { parent: pid.clone(), after: None, block: c }).unwrap();
        // 把 p 移到它的子 c 下 → 应拒绝（环）。
        assert!(doc.apply(Op::MoveBlock { id: pid, new_parent: cid, after: None }).is_err());
    }

    #[test]
    fn update_doc_meta() {
        let (mut doc, _) = doc_with_root();
        doc.apply(Op::UpdateDocMeta {
            doc_id: doc.id.clone(),
            patch: serde_json::json!({ "title": "New", "tags": ["a", "b"] }),
        })
        .unwrap();
        assert_eq!(doc.title, "New");
        assert_eq!(doc.tags, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn descendants_preorder() {
        let (mut doc, root) = doc_with_root();
        let a = para("a");
        let aid = a.id.clone();
        let b = para("b");
        let bid = b.id.clone();
        doc.apply(Op::InsertBlock { parent: root.clone(), after: None, block: a }).unwrap();
        doc.apply(Op::InsertBlock { parent: aid.clone(), after: None, block: b }).unwrap();
        assert_eq!(doc.descendants(&root), vec![root, aid, bid]);
    }
}
