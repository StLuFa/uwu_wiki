---

# uwu_wiki 项目增强建议

## 一、架构与设计层面

### 1. 统一配置系统（优先级：高）

当前 `wiki-workflow` 中大量硬编码值分散在各处：

| 位置 | 硬编码值 |
|---|---|
| `ingest.rs` | `take(10)` 搜索上限 |
| `query.rs` | RAG top-8 |
| `lint.rs` | `take(200)` 扫描上限 |
| `rag.rs` | RRF k=60 |

**建议**：引入一个 `WikiConfig` 结构体，统一管理所有可调参数，支持从文件/环境变量加载。

### 2. 持久化调度器（优先级：高）

`lint.rs` 中配置了 `schedule = "0 3 * * *"`，但没有实际的 cron 调度实现。

**建议**：引入 `tokio-cron-scheduler` 或类似库，实现真正可运行的定时 Lint 扫描 + Ingest 调度。

### 3. 消除重复代码（优先级：中）

`cosine_similarity` 在 `wiki-testkit/src/lib.rs` 和 `wiki-workflow/src/lint.rs` 中各有一份独立实现。

**建议**：将通用工具函数下沉到 `wiki-core` 的 `utils` 模块，由各 crate 引用。

---

## 二、功能补全

### 4. 矛盾检测实现（优先级：高）

`WikiLinter::check_contradictions` 当前是空实现（TODO）：

```rust
// wiki-workflow/src/lint.rs
fn check_contradictions(&self) -> Vec<LintIssue> {
    // TODO: LLM 驱动的语义矛盾检测
    vec![]
}
```

**建议**：利用已有的 LLM 能力，实现以下逻辑：
- 对内容相似度高的文档对（已有余弦相似度计算），用 LLM 做语义级别的矛盾检测
- 检测到矛盾时生成 `LintIssue` 并标记严重级别

### 5. LLM 语义过时检测（优先级：中）

`LintConfig::stale_check_llm` 已定义但未使用：

```rust
pub struct LintConfig {
    pub stale_check_llm: bool,  // 存在但未实现
    // ...
}
```

**建议**：当 `stale_check_llm = true` 时，对超过阈值天数的文档，调用 LLM 判断内容是否真的过时（而非仅靠时间戳）。

### 6. 全文搜索增强（优先级：中）

当前 `TextIndex` 仅支持 `Exact`/`Prefix`/`Fuzzy` 三种模式：

```rust
pub enum SearchMode {
    Exact,
    Prefix,
    Fuzzy,
}
```

**建议**：
- 添加 **短语搜索** (Phrase) 和 **布尔查询** (Boolean: AND/OR/NOT)
- 添加 **搜索结果高亮** 返回匹配位置
- 支持 **BM25 相关性评分** 而非简单的命中/未命中

### 7. Block 操作历史与撤销/重做（优先级：中）

当前 Op 日志只支持追加和回放，没有撤销机制。

**建议**：基于 OpLog 实现：
- `undo()` / `redo()` 操作栈
- 生成逆 Op（InsertBlock ↔ DeleteBlock，MoveBlock 保存旧 parent）
- 前端可展示操作历史时间线

---

## 三、性能优化

### 8. 增量索引优化（优先级：中）

当前 `apply_ops` 中增量重索引的逻辑：

```rust
// wiki-core/src/lib.rs
for op in &ops {
    match op {
        Op::UpdateBlock { block_id, .. } => {
            // 全文重索引
            self.text_index.index_block(block_id, &block.content)?;
        }
        // ...
    }
}
```

**建议**：
- 批量索引：收集所有变更 block，一次性批量提交给索引
- 异步索引：将索引操作放入后台队列，不阻塞写入响应
- 对 `UpdateBlock` 仅更新变化的字段，而非全量重建

### 9. Embedding 缓存持久化（优先级：低）

当前 `EmbeddingCache` 是纯内存结构，进程重启后全部丢失需要全量重算。

**建议**：将缓存持久化到磁盘（如 sled/SQLite），启动时加载，减少冷启动成本。

### 10. VectorStore 批量操作（优先级：中）

当前 `VectorStore` trait 只有单条 upsert：

```rust
fn upsert(&self, id: &BlockId, vector: Vec<f32>, metadata: serde_json::Value);
```

**建议**：添加 `batch_upsert` 方法，减少 LLM embedding 批处理后的存储开销。

---

## 四、可靠性增强

### 11. 写入事务保证（优先级：高）

当前 `apply_ops` 的"原子性"只是内存级校验：

```rust
// 先校验所有 Op 合法性，再执行
for op in &ops {
    self.validate_op(op)?; // 任一失败则返回错误
}
for op in ops {
    self.execute_op(op)?;  // 但执行过程中可能失败
}
```

**建议**：
- 引入写前日志（WAL）：先持久化 Op 列表，再逐条执行
- 执行失败时回滚已执行的 Op
- 或采用 Copy-on-Write：在副本上执行，成功后原子替换

### 12. 错误恢复与重试（优先级：中）

LLM 调用、存储操作均无重试机制。

**建议**：
- 对 `LlmClient::complete()` 和 `LlmClient::embed()` 添加指数退避重试
- 对存储操作添加可配置的重试策略

### 13. 数据校验层（优先级：中）

当前缺少对输入数据的完整性校验。

**建议**：
- Block content 的 JSON Schema 校验
- Document title 长度/字符限制
- 防止超大文档导致 OOM（设置 block 数量/深度上限）
- 循环引用检测（不仅限于 MoveBlock，也应检测 Block content 中的引用）

---

## 五、开发者体验

### 14. 测试覆盖补充（优先级：中）

当前测试覆盖了核心路径，但缺少：

| 缺失场景 | 说明 |
|---|---|
| 边界条件 | 空文档、超长文本、特殊字符 |
| 并发测试 | 多线程同时写入 |
| 模糊测试 | 随机 Op 序列 + 不变量检查 |
| 性能基准 | RAG 检索延迟、文档树遍历性能 |

**建议**：
- 使用 `proptest` 做基于属性的模糊测试
- 使用 `criterion` 建立性能基准测试
- 添加并发压力测试

### 15. 可观测性（优先级：中）

当前完全没有日志/指标/追踪。

**建议**：
- 引入 `tracing` crate 添加结构化日志
- 关键路径（ingest/query/lint）添加 span 追踪
- 暴露 Prometheus 指标：文档数、检索延迟、LLM 调用次数/延迟
- 事件总线已有 9 个 topic 定义，应接入实际的事件发布

### 16. 错误信息国际化（优先级：低）

当前 `WikiError` 的错误信息是英文硬编码。

**建议**：使用错误码 + 消息模板，支持多语言。

---

## 六、功能扩展建议

### 17. 插件系统（优先级：低）

项目架构文档中提到了 WASM 插件 (`uwu_wasm`)，但当前无实现。

**建议**：
- 利用 `BlockTypeRegistry` 的 Custom 类型，通过 WASM 插件注册自定义 Block 渲染器
- 允许插件 hook Ingest/Query/Lint 管线

### 18. Markdown 导入/导出增强（优先级：低）

当前 `MarkdownRenderer` 只支持输出，且仅覆盖部分 BlockType。

**建议**：
- 实现 Markdown → Block 树的解析器（双向转换）
- 支持更多 BlockType 的渲染（Table、Graph、Image、Embed）

### 19. Graph 可视化增强（优先级：低）

当前 Graph 支持 Mermaid/PlantUML 导出和基础布局，但：

**建议**：
- 添加力导向布局 (Force-directed layout)
- 支持子图/分组 (Subgraph)
- 节点支持自定义样式（颜色、大小、图标）

---

## 总结优先级排序

| 优先级 | 类别 | 项目 |
|---|---|---|
| 🔴 高 | 功能补全 | 矛盾检测实现 |
| 🔴 高 | 可靠性 | 写入事务保证 (WAL) |
| 🔴 高 | 架构 | 统一配置系统 |
| 🔴 高 | 架构 | 持久化调度器 |
| 🟡 中 | 功能补全 | LLM 语义过时检测 |
| 🟡 中 | 功能补全 | 全文搜索增强 (BM25/布尔查询) |
| 🟡 中 | 功能补全 | 撤销/重做 |
| 🟡 中 | 性能 | 批量索引 + VectorStore 批量操作 |
| 🟡 中 | 可靠性 | 错误恢复与重试 |
| 🟡 中 | 可靠性 | 数据校验层 |
| 🟡 中 | 架构 | 消除重复代码 |
| 🟡 中 | 开发者体验 | 测试补充 + 可观测性 |
| 🟢 低 | 功能扩展 | 插件系统、Markdown 导入、Graph 可视化增强 |

---