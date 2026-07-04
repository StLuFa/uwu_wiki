# wiki-core

uwu_wiki 核心：**Block 引擎 + Document/Op 模型 + 全部存储/LLM 端口 trait 定义**。

## 设计约束

| 原则 | 说明 |
|---|---|
| **核心纯粹性** | 除 serde/uuid/chrono 外零依赖；不含存储/LLM 实现 |
| **端口/适配器** | 全部存储能力以 trait（端口）暴露，实现由宿主注入 |
| **单向依赖** | 不依赖任何其他 wiki-* crate 或引擎 |

参考实现见 `wiki-testkit`（dev-dependency）；生产由 `agent-context-db` 注入。

---

## Block 模型

文档、表格、图的最小单元。

```rust
pub struct Block {
    pub id: BlockId,                   // UUID v7
    pub ty: BlockType,
    pub content: BlockContent,         // serde_json::Value
    pub children: Vec<BlockId>,        // 有序子块
    pub parent: Option<BlockId>,
    pub version: u64,                  // 乐观并发版本号
    pub embedding: Option<Vec<f32>>,   // 懒生成
    pub embedding_version: u64,        // 陈旧检测
    pub meta: BlockMeta,               // created_at / updated_at / created_by
}
```

**内置 BlockType**：`paragraph` / `heading` / `bulleted_list` / `numbered_list` / `toggle` / `quote` / `callout` / `code` / `divider` / `image` / `embed` / `table_ref` / `graph_ref` / `database_view`

新类型通过 `BlockTypeRegistry::register` 注册。

---

## Document 模型

```rust
pub struct Document {
    pub id: DocId,
    pub title: String,
    pub root: BlockId,         // 树根
    pub version: u64,
    pub space_id: SpaceId,
    pub tags: Vec<String>,
    pub blocks: Vec<Block>,    // 全部 Block（以 root 为根）
}
```

**方法**：`block(id)` / `children(parent)` / `descendants(root)` / `walk(root, f)` / `last_child(parent)` / `updated_at()` / `apply(op)`

---

## Op 写入管线

```rust
pub enum Op {
    InsertBlock { parent, after, block },
    UpdateBlock { id, patch },
    DeleteBlock { id },
    MoveBlock   { id, new_parent, after },
    UpdateDocMeta { doc_id, patch },
}
```

所有写操作产生 Op；Op 既是 CRDT 合并输入，也是事件总线消息体，也是审计日志。

---

## 双向链接（backlinks）

Block 正文可含 `[[wiki-link]]` 引用。核心维护引用图，支撑 backlinks：

```rust
pub struct WikiLink {
    pub from: BlockId,
    pub to: LinkTarget,         // Doc(DocId) | Block(DocId, BlockId) | Broken(String)
    pub anchor_text: String,
}

pub trait LinkGraph: Send + Sync {
    fn outbound(&self, from: BlockId) -> Vec<WikiLink>;
    fn backlinks(&self, target: &LinkTarget) -> Vec<WikiLink>;
    fn broken_links(&self) -> Vec<WikiLink>;
}
```

---

## 存储端口（全部 7 个）

```rust
pub trait WikiStorage: Send + Sync + 'static {
    fn vector_store(&self)  -> Arc<dyn VectorStore>;
    fn doc_store(&self)     -> Arc<dyn DocStore>;
    fn op_log(&self)        -> Arc<dyn OpLog>;
    fn text_index(&self)    -> Arc<dyn TextIndex>;      // #1 全文精确检索
    fn link_store(&self)    -> Arc<dyn LinkStore>;       // #2 引用图持久化
    fn blob_store(&self)    -> Arc<dyn BlobStore>;       // #4 二进制附件
    fn version_store(&self) -> Arc<dyn DocVersionStore>; // #5 文档版本快照
}
```

| 端口 | 用途 |
|---|---|
| `VectorStore` | 向量 upsert/search/delete |
| `DocStore` | 文档 CRUD |
| `OpLog` | Op 日志 append/replay（CRDT 离线回放） |
| `TextIndex` | 全文倒排索引（精确/前缀/模糊查询） |
| `LinkStore` | 引用图 upsert/backlinks/broken_links |
| `BlobStore` | 二进制附件 + 引用计数 GC |
| `DocVersionStore` | 版本快照 snapshot/diff/restore |

---

## WikiSpace — 写入入口

```rust
pub struct WikiSpace { id: SpaceId, storage: Arc<dyn WikiStorage> }

impl WikiSpace {
    // 文档 CRUD
    pub async fn create_doc(&self, title, root) -> Result<Document>;
    pub async fn get_doc(&self, id) -> Result<Option<Document>>;
    pub async fn save_doc(&self, doc) -> Result<()>;
    pub async fn delete_doc(&self, id) -> Result<()>;
    pub async fn list_docs(&self, offset, limit) -> Result<Vec<DocId>>;

    // 写入管线
    pub async fn apply_ops(&self, doc_id, ops: Vec<Op>) -> Result<Document>;

    // 检索
    pub async fn search_semantic(&self, query_vec, top_k) -> Result<Vec<VectorSearchResult>>;
    pub async fn search_text(&self, query: &TextQuery, top_k) -> Result<Vec<TextHit>>;
    pub async fn backlinks(&self, target) -> Result<Vec<WikiLink>>;

    // 版本
    pub async fn snapshot(&self, doc_id, label) -> Result<VersionId>;
    pub async fn restore(&self, doc_id, version) -> Result<Document>;
}
```

`apply_ops` 保证原子性：任一 Op 非法则整批中止。同时维护 op_log + text_index + link_store。

---

## 目录

```
wiki-core/src/
├── block.rs       Block + BlockId + BlockContent + BlockMeta
├── doc.rs         Document + DocId + SpaceId + Op
├── error.rs       Result / WikiError
├── link.rs        WikiLink / LinkTarget / LinkGraph / 链接解析
├── registry.rs    BlockTypeRegistry + MarkdownRenderer
├── storage.rs     全部 7 个存储端口 trait 定义
└── lib.rs         WikiSpace + re-exports
```

## 依赖

`serde` / `serde_json` / `uuid` / `chrono` / `async-trait` / `thiserror`

零外部引擎依赖，零存储实现。
