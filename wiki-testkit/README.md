# wiki-testkit

全部 7 个 `WikiStorage` 端口的内存参考实现，**仅用于测试/开发**。

生产环境由 `agent-context-db` 注入 PG + Qdrant 后端。

---

## 使用

```rust
use wiki_testkit::MemoryWikiStorage;
use wiki_core::{WikiStorage, WikiSpace, SpaceId};

let storage = MemoryWikiStorage::new();

// 所有 7 个端口可用
let vector  = storage.vector_store();
let docs    = storage.doc_store();
let oplog   = storage.op_log();
let text    = storage.text_index();
let links   = storage.link_store();
let blobs   = storage.blob_store();
let versions = storage.version_store();

// 传入 WikiSpace
let space = WikiSpace::new(SpaceId::default(), Arc::new(storage));
```

---

## 7 个端口实现

| 端口 | 实现 | 说明 |
|---|---|---|
| `VectorStore` | `MemVectorStore` | HashMap + 余弦相似度检索 |
| `DocStore` | `MemDocStore` | HashMap 文档 CRUD |
| `OpLog` | `MemOpLog` | Vec 追加 / 按序号回放 |
| `TextIndex` | `MemTextIndex` | HashMap 倒排 + Exact/Prefix/Fuzzy 匹配 |
| `LinkStore` | `MemLinkStore` | HashMap 出链表 + backlinks 遍历 + broken_links 过滤 |
| `BlobStore` | `MemBlobStore` | HashMap + 引用计数 GC |
| `DocVersionStore` | `MemVersionStore` | Vec 快照列表 + Block 级 diff + restore 生成新版本 |

---

## 目录

```
wiki-testkit/src/
└── lib.rs     MemoryWikiStorage + 7 个 Mem* 实现 + 测试
```

## 依赖

`wiki-core` / `parking_lot` / `serde_json` / `async-trait` / `uuid` / `chrono`
