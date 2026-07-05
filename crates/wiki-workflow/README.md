# wiki-workflow

LLM Wiki 工作流：Ingest（原料→知识）/ Query（问答→反写）/ Lint（审计）。

核心理念：LLM 是 wiki 的**全职编辑**，不是临时检索器。每次 Ingest 让 wiki 更丰富，每次 Query 的好答案反写回 wiki，Lint 保持 wiki 健康——知识随时间复利增长。

---

## 三管线总览

```
raw/（原始资料目录）
  │
  ▼  Ingest
wiki/（结构化知识库）
  │
  ▼  Query ──→ 答案 ──→ 反写 wiki（可选）
  │
  ▼  Lint（定期）
     修复矛盾 / 孤页 / 缺页
```

---

## 快速上手

```rust
use wiki_workflow::{WikiDomain, IngestSource, WriteBackPolicy, LintConfig};

let domain = WikiDomain::new(llm_capability, wiki_space);

// 1. Ingest — 消化原料
let result = domain.ingest(&IngestSource {
    content: "Rust 的 async/await 基于 Future trait...".into(),
    title: Some("Rust Async 介绍".into()),
    source_url: Some("https://example.com".into()),
}).await?;
// result.summary_doc_id, result.touched_docs, result.created_docs

// 2. Query — RAG 问答
let answer = domain.query("什么是 async?").await?;
let answer = domain.query_with_write_back(
    "什么是 Future?",
    &WriteBackPolicy::Auto { confidence_threshold: 0.85 },
).await?;

// 3. Lint — 全量审计
let report = domain.lint().await?;
// report.orphan_pages, report.broken_links_fixed, report.duplicate_candidates
```

---

## Ingest — 原料转知识

5 步管线：

```
1. 分析原料 → LLM 提取 entity / concept / claim
2. 搜索已有 wiki → 找相关页面
3. LLM 对比 → 生成 PageUpdatePlan（新增/更新/矛盾标注）
4. 执行 → 生成 Op 批量写入 wiki
5. 创建摘要文档 → 记录来源与触动页面
```

```rust
pub struct IngestResult {
    pub summary_doc_id: Option<DocId>,
    pub touched_docs: Vec<DocId>,        // 被更新的已有页面
    pub created_docs: Vec<DocId>,        // 新建页面
    pub contradictions: Vec<Contradiction>, // 发现的矛盾
}
```

### 矛盾处理

| ContradictionSeverity | 处理策略 |
|---|---|
| `Minor` | 标注，不覆盖 |
| `Major` | 标注 + 记录到 LintReport |
| `Critical` | 标注 + 触发人工决策事件 |

---

## Query — 问答与反写

```rust
// 仅问答（不反写）
let result = domain.query("什么是 async?").await?;

// 问答 + 反写
let result = domain.query_with_write_back(question, &policy).await?;
```

### WriteBackPolicy

| 策略 | 行为 |
|---|---|
| `Never` | 永不自动写回 |
| `Auto { confidence_threshold }` | LLM 判断置信度 ≥ 阈值 → 自动写回 |
| `AskFirst` | 评估后返回 write_back 字段，由调用方决定 |

反写流程：
1. RAG 问答 → 获取答案 + 引用
2. LLM 评估：答案是否含 wiki 中未有的新知识
3. 若策略允许 → 生成 Block 追加到目标页面或创建新页面

---

## Lint — 定期知识审计

6 项检查：

| 检查项 | 检测方式 | 自动修复 |
|---|---|---|
| **孤页** | `backlinks` 为空 | 否（建议删除或合并） |
| **断链** | `references` 中 DocId 不存在 | ✅ 自动清理 |
| **缺页** | `[[entity]]` 提及但无对应页面 | 可选（创建占位页） |
| **过时** | `updated_at` 超过阈值天数 | 否（标记 stale） |
| **重复** | 嵌入余弦相似度 ≥ 阈值 | 建议合并 |
| **矛盾** | LLM 抽样检查 | 否（人工决策） |

```rust
pub struct LintReport {
    pub space_id: SpaceId,
    pub ran_at: DateTime<Utc>,
    pub orphan_pages: Vec<DocId>,
    pub missing_pages: Vec<String>,
    pub contradictions: Vec<String>,
    pub stale_pages: Vec<(DocId, String)>,
    pub duplicate_candidates: Vec<(DocId, DocId, f32)>,
    pub broken_links_fixed: usize,
    pub total_pages: usize,
}
```

### LintConfig

```rust
LintConfig {
    duplicate_threshold: 0.92,       // 余弦相似度阈值
    stale_check_llm: false,          // 是否 LLM 语义过时检查
    auto_fix_broken_links: true,     // 自动修复断链
    auto_create_missing_pages: false, // 自动创建缺页占位
    stale_days_threshold: 90,        // 过时天数阈值
}
```

---

## 工作流事件

| 事件主题 | 触发条件 |
|---|---|
| `wiki.ingest.completed` | Ingest 完成 |
| `wiki.ingest.contradiction` | 发现矛盾 |
| `wiki.query.write_back` | 答案反写 |
| `wiki.lint.completed` | Lint 扫描完成 |
| `wiki.lint.missing_page` | 发现缺页 |

---

## 目录

```
wiki-workflow/src/
├── lib.rs       WikiDomain 三管线统一入口 + re-exports
├── ingest.rs    IngestPipeline（5 步管线 + 原料分析 + 执行计划）
├── query.rs     QueryPipeline（问答 + WriteBackEval + 反写执行）
└── lint.rs      WikiLinter（6 项检查 + 自动修复 + LintConfig）
```

## 依赖

`wiki-core` / `wiki-llm` / `serde` / `serde_json` / `chrono` / `async-trait`
