# wiki-llm

LLM 横切层 —— **领域无关**。只认 `TextUnit`，不 `use` 文档/表格/图具体类型。

LLM 后端由注入的 `LlmClient` 提供，不依赖 agent-core 或任何 LLM 引擎。

---

## 快速上手

```rust
use wiki_llm::{DefaultLlmEngine, LlmCapability, MockLlmClient};
use wiki_testkit::MemoryWikiStorage;
use wiki_llm::mock::AllowAllPermissionFilter;

let storage = MemoryWikiStorage::new();
let engine = DefaultLlmEngine::new(
    Arc::new(MockLlmClient::new()),
    storage.vector_store(),
    storage.text_index(),
    storage.doc_store(),
    Arc::new(AllowAllPermissionFilter),
);

// 语义搜索
let results = engine.search("rust async", 5).await?;

// RAG 问答
let answer = engine.qa("什么是 async/await?", None).await?;

// 摘要
let summary = engine.summarize(&text_units).await?;
```

---

## 核心抽象

### TextUnit — 领域无关文本单元

```rust
pub struct TextUnit {
    pub id: String,        // BlockId / RowId / NodeId 的字符串化
    pub text: String,      // 待处理文本
    pub path: Vec<String>, // 溯源路径（doc→block / table→row / graph→node）
}
```

三类实体（Block / 表格行 / 图节点）统一适配为 `TextUnit` 后调 LLM 端口。

### LlmClient — 注入的 LLM 后端

```rust
#[async_trait]
pub trait LlmClient: Send + Sync {
    async fn complete(&self, prompt: &str, opts: &LlmOpts) -> Result<String>;
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
}
```

由宿主在构造期注入（复用 agent-context-db 同名抽象）。

### LlmCapability — 领域无关能力端口

```rust
#[async_trait]
pub trait LlmCapability: Send + Sync {
    async fn embed(&self, units: &[TextUnit]) -> Result<Vec<Vec<f32>>>;
    async fn search(&self, query: &str, top_k: usize) -> Result<Vec<(TextUnit, f32)>>;
    async fn complete(&self, unit: &TextUnit, partial: &str) -> Result<String>;
    async fn qa(&self, question: &str, scope_root: Option<&str>) -> Result<QaAnswer>;
    async fn summarize(&self, units: &[TextUnit]) -> Result<String>;
}
```

默认实现在 `DefaultLlmEngine`。

---

## DefaultLlmEngine — 依赖注入 5 个端口

```rust
pub struct DefaultLlmEngine {
    llm: Arc<dyn LlmClient>,            // LLM 推理
    vector: Arc<dyn VectorStore>,       // 语义检索
    text: Arc<dyn TextIndex>,           // 全文精确检索
    docs: Arc<dyn DocStore>,            // Block 全文读取
    permission: Arc<dyn PermissionFilter>, // 权限过滤
}
```

### 权限模型

- **`search()` / `qa()`**：系统级权限（Owner 角色）—— 适合 lint/ingest 等后台任务
- **`search_with_permission(ctx)` / `qa_with_permission(ctx)`**：用户级权限过滤 —— 适合面向用户的查询

权限过滤发生在 prompt 构建**之前**，无权 Block 绝不进 LLM 上下文。

---

## RAG 检索管线

```
用户查询
  ├─ 语义路 → VectorStore（向量）
  └─ 精确路 → TextIndex（倒排）
  → RRF（Reciprocal Rank Fusion）融合
  → 权限过滤（PermissionFilter）
  → 返回 top-k
```

- RRF 参数 k=60，避免向量分与 BM25 分量纲不可比
- 全文支持 Exact / Prefix / Fuzzy 三种匹配模式

---

## 增量 Embedding

配合 wiki-core 的 embedding 陈旧检测：

```rust
pub struct Block {
    pub version: u64,               // 内容版本
    pub embedding_version: u64,     // ★ 生成该 embedding 时的 version
}

// 陈旧判断
pub fn is_embedding_stale(&self) -> bool { ... }
```

- **写入**：Block 更新 `version += 1`，`embedding_version` 保持旧值
- **检索**：命中 Block 若 `embedding_version < version`，标记 stale
- **补算**：`diff_embed()` 仅重算 stale 单元，其余复用 `EmbeddingCache`

---

## 测试 Mock

```rust
use wiki_llm::mock::{MockLlmClient, AllowAllPermissionFilter, DenyListPermissionFilter};

// 可配置的 LLM 桩
let mock = MockLlmClient::new()
    .with_fixed_complete("固定回答")
    .with_deterministic_embed(8);

// 放行全部权限
let filter = AllowAllPermissionFilter;

// 拒绝特定 Block
let filter = DenyListPermissionFilter::new(["secret-block"]);
```

---

## 目录

```
wiki-llm/src/
├── lib.rs         LlmClient / LlmCapability / TextUnit / QaAnswer 定义
├── engine.rs      DefaultLlmEngine（注入 5 端口，完整实现）
├── rag.rs         RAG 检索管线（hybrid search + RRF + context builder + QA prompt）
├── embed.rs       EmbeddingCache + diff_embed（增量 embedding）
└── mock.rs        MockLlmClient + AllowAll/DenyList PermissionFilter
```

## 依赖

`wiki-core` / `wiki-collab` / `serde` / `serde_json` / `async-trait`

**不依赖** agent-core 或任何 LLM 引擎具体类型。
