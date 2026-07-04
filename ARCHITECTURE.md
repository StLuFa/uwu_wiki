# uwu_wiki — 架构概览

---

## 1. 设计原则

| 原则 | 说明 |
|---|---|
| **Block 第一** | 文档、表格、图均由 Block 树组成；Block 是检索、合并、事件的最小单元 |
| **索引先行** | 热路径走倒排索引（tag/category/status/title/graph_id），零全表扫描 |
| **并发安全** | 所有 Store 使用 `Arc<RwLock<Inner>>`，`.clone()` 即可跨任务共享 |
| **统一 LLM 接口** | `LlmCapability` 领域无关端口，同时覆盖文档、表格、图 |
| **增量 Embedding** | 只重算变更节点及其一跳邻居 |
| **可插拔存储** | `WikiStorage` 7 端口 trait 注入，核心不含存储实现 |
| **Op 日志驱动** | 所有写操作产生 Op：CRDT 合并输入 / 事件消息体 / 审计日志三合一 |
| **LLM 全职编辑** | Ingest（原料→知识）/ Query（问答→反写）/ Lint（定期审计），知识复利增长 |

### 四条硬约束

1. **端口/适配器**：每个 crate 对外只暴露 trait；后端/引擎是适配器，构造期注入
2. **单向依赖**：`wiki-core` ← 各能力 crate ← 调用方；横切层不反向依赖领域层
3. **依赖倒置**：`LlmClient` / `WikiStorage` 全 trait 注入，不 `use` 具体实现
4. **核心纯粹性**：`wiki-core` 除 serde/uuid/chrono 零依赖

---

## 2. 架构全景

```
┌────────────────────────────────────────────────────────────────────────┐
│                              uwu_wiki                                  │
│                                                                        │
│  ┌─────────────┐   ┌──────────────┐   ┌─────────────────────────────┐  │
│  │  wiki-core  │   │  wiki-table  │   │        wiki-graph           │  │
│  │  Block 引擎  │   │   智能表格    │   │  流程图 / 思维导图 + LLM    │  │
│  └──────┬──────┘   └──────┬───────┘   └─────────────┬───────────────┘  │
│         │                 │                         │                  │
│  ┌──────┴─────────────────┴─────────────────────────┴──────────────┐   │
│  │                         wiki-llm                                │   │
│  │   LlmCapability 端口  ·  Embedding  ·  RAG  ·  搜索  ·  补全    │   │
│  │                                                                  │   │
│  │  ┌──────────────────────────────────────────────────────────┐   │   │
│  │  │              wiki-workflow                               │   │   │
│  │  │   Ingest（原料→知识）· Query（问答→反写）· Lint（审计）    │   │   │
│  │  └──────────────────────────────────────────────────────────┘   │   │
│  └────────────────────────────┬────────────────────────────────────┘   │
│                               │                                        │
│  ┌────────────────────────────┴────────────────────────────────────┐   │
│  │                        wiki-collab                              │   │
│  │              CRDT 协作 · 权限控制 · Op 广播 · 离线队列            │   │
│  └────────────────────────────┬────────────────────────────────────┘   │
└───────────────────────────────┼────────────────────────────────────────┘
                                │
          ┌─────────────────────┼─────────────────────┐
          ▼                     ▼                     ▼
   VectorStore trait      uwu_event_mesh          uwu_wasm
  (外部注入)             (跨进程事件总线)       (WASM 插件沙箱)
```

---

## 3. Crate 拆分

```
pkg/uwu_wiki/
├── wiki-core/       Block / Document / Op / 存储端口 trait（零依赖，无实现）
├── wiki-testkit/    MemoryWikiStorage 等参考实现（dev-dependency）
├── wiki-table/      智能表格（依赖 wiki-core；自适配 TextUnit 调 wiki-llm）
├── wiki-graph/      流程图 / 思维导图（依赖 wiki-core；自适配 TextUnit 调 wiki-llm）
├── wiki-llm/        LLM 横切层（领域无关 LlmCapability + TextUnit；LlmClient 注入）
├── wiki-workflow/   工作流 Ingest/Query/Lint（依赖 wiki-core + wiki-llm 端口）
└── wiki-collab/     CRDT 协作（依赖 wiki-core, uwu-crdt, uwu_event_mesh）
```

各模块详情见对应 README：

| 模块 | README | 说明 |
|---|---|---|
| wiki-core | [README](wiki-core/README.md) | Block 引擎 + Document/Op 模型 + 存储端口 |
| wiki-llm | [README](wiki-llm/README.md) | DefaultLlmEngine + RAG 管线 + 增量 Embedding |
| wiki-table | [README](wiki-table/README.md) | TableStore + TableView + LlmFill |
| wiki-graph | [README](wiki-graph/README.md) | GraphStore + 遍历 + Mermaid/PlantUML 导出 |
| wiki-collab | [README](wiki-collab/README.md) | CollabDoc CRDT 同步 + PermissionFilter |
| wiki-workflow | [README](wiki-workflow/README.md) | Ingest/Query/Lint 三管线 |
| wiki-testkit | [README](wiki-testkit/README.md) | 7 端口内存参考实现 |

### Feature 矩阵

| Crate | Feature | 说明 |
|---|---|---|
| `wiki-core` | `backlinks` / `attachments` / `versioning` | 双向链接 / 附件 / 版本快照 |
| `wiki-table` | `formula` / `llm-fill` | 公式引擎 / LLM 列 |
| `wiki-graph` | `llm` / `llm-qa` / `export` | 图 LLM / 图 RAG / 导出 |
| `wiki-llm` | `hybrid-search` / `full-text` | 混合检索 / 全文倒排 |
| `wiki-workflow` | `default` | Ingest/Query/Lint 管线 |

---

## 4. 依赖关系图

```
                      wiki-core
              (零外部依赖 + 全部 trait 定义)
             /      |        |        \        \
            /       |        |         \        \
     wiki-table  wiki-graph  wiki-llm  wiki-collab  wiki-workflow
         |          |         |          |            |
   uwu_visual   (适配      LlmClient   uwu-crdt   依赖 wiki-llm
     _script    TextUnit   (注入)     uwu_event     端口 + wiki-core
                调 wiki-llm  ↑          _mesh
                端口)     不依赖 agent-core

  wiki-testkit（dev-dependency，不进生产）

  单向依赖：wiki-core ← 各能力 crate ← 调用方
  WikiStorage 7 端口 + LlmClient trait 均在 wiki-core/wiki-llm 定义，外部注入
```

---

## 5. 配置参考

```toml
[wiki]
space_id = "default"

[wiki.llm]
model            = "gpt-4o-mini"
embed_model      = "text-embedding-3-small"
search_top_k     = 8
search_hybrid    = true

[wiki.collab]
op_queue_max  = 10000

[wiki.storage]
vector_collection = "wiki_blocks"
vector_dim        = 1536
graph_node_collection = "wiki_graph_nodes"

[wiki.graph]
default_layout        = "dagre"
search_top_k          = 8
search_context_hops   = 1

[wiki.workflow.ingest]
max_touched_docs = 20

[wiki.workflow.query]
write_back_policy     = "ask_first"
write_back_confidence = 0.85

[wiki.workflow.lint]
schedule                 = "0 3 * * *"
duplicate_threshold      = 0.92
auto_fix_broken_links    = true

[wiki.versioning]
snapshot_op_threshold = 50
max_versions_per_doc  = 200
```

---

## 6. 事件总线

| 事件主题 | 发布方 | 订阅方 | 触发条件 |
|---|---|---|---|
| `wiki.block.updated` | wiki-core | wiki-llm | Block 创建/更新 → 增量 embedding |
| `wiki.graph.node.updated` | wiki-graph | wiki-llm | 节点创建/更新 → 增量 embedding |
| `wiki.llm.fill_request` | wiki-table | LLM Worker | LLM 列行写入 → 异步填充 |
| `wiki.collab.op` | wiki-collab | 在线 Client | Op 实时广播 |
| `wiki.ingest.completed` | IngestPipeline | 调用方 | Ingest 完成 |
| `wiki.ingest.contradiction` | IngestPipeline | 调用方 | 发现矛盾 |
| `wiki.query.write_back` | QueryPipeline | wiki-core | 答案反写 |
| `wiki.lint.completed` | WikiLinter | 调用方 | Lint 扫描完成 |
| `wiki.block.linked` | wiki-core | LinkStore | 引用变化 → 更新引用图 |
| `wiki.embed.stale` | wiki-llm | LLM Worker | 检索命中陈旧 embedding |
