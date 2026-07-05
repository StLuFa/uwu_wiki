# wiki-table

智能表格：列定义 / 行 CRUD / 视图查询 / LLM 智能填充。

领域实体（行）自适配为 `wiki_llm::TextUnit` 后调用 wiki-llm 端口。

---

## 快速上手

```rust
use wiki_table::{Table, TableStore, TableView, Column, ColumnType, Filter, FilterOp, Row, Cell};

// 建表
let mut t = Table::new("tasks", "任务表");
t.add_column(Column::new("title", ColumnType::Text)).unwrap();
t.add_column(Column::new("priority", ColumnType::Number)).unwrap();
t.add_column(Column::new("done", ColumnType::Checkbox)).unwrap();

// 插入行
let row = Row {
    id: RowId::new(),
    cells: vec![
        Cell { column: col_title, value: json!("learn rust") },
        Cell { column: col_priority, value: json!(5) },
        Cell { column: col_done, value: json!(false) },
    ],
};
t.insert_row(row).unwrap();

// 视图查询
let view = TableView::new(&t)
    .filter(Filter { column: col_done, op: FilterOp::Eq, value: json!(false) })
    .sort_by("priority", true);
let pending: Vec<&Row> = view.rows();
```

---

## 列类型

| ColumnType | 说明 |
|---|---|
| `Text` / `Number` / `Checkbox` / `Url` / `Date` | 基础类型 |
| `Select` / `MultiSelect` | 枚举选项 |
| `Relation` | 关联另一张表（外键语义） |
| `Rollup` | 对 Relation 列聚合（sum / count / avg） |
| `Formula` | 使用 uwu_visual_script SlotProgram |
| `LlmFill` | LLM 智能填充 |
| `CreatedAt` / `UpdatedAt` / `CreatedBy` | 系统字段 |

---

## TableView — 声明式查询

```rust
let view = TableView::new(&table)
    .filter(Filter { column, op: FilterOp::Contains, value: json!("rust") })
    .filter(Filter { column, op: FilterOp::Gt, value: json!(3) })
    .sort_by("priority", false)     // 降序
    .hide_column(ColumnId("internal".into()));

let page = view.paginate(0, 20);    // 分页
let total = view.count();           // 总数
let groups = view.group_counts(&col_id); // 分组计数
```

| FilterOp | 说明 |
|---|---|
| `Eq` / `Neq` | 等于 / 不等于 |
| `Gt` / `Lt` / `Gte` / `Lte` | 数值比较 |
| `Contains` / `StartsWith` | 字符串匹配 |

---

## TableStore — 内存存储

```rust
let mut store = TableStore::new();
store.create(table).unwrap();

let t = store.get("tasks").unwrap();
let t_mut = store.get_mut("tasks").unwrap();
store.delete("tasks");
```

生产环境由 agent-context-db 注入持久化后端。

---

## LlmFill — LLM 智能填充

```rust
use wiki_llm::LlmCapability;

// 定义 LLM 填充列
t.add_column(Column::new("summary", ColumnType::LlmFill)).unwrap();

// 插入行后触发 LLM 填充
t.insert_row(new_row).unwrap();
let filled = t.fill_row_with_llm(&*llm_engine, &row_id).await?;
// filled: Vec<(列名, 生成内容)>
```

Prompt 模板支持 `{col_name}` 占位符，触发方式：`OnCreate` / `OnUpdate` / `Manual`。

---

## 目录

```
wiki-table/src/
└── lib.rs     Table / Column / Row / Cell / TableStore / TableView / LlmFill
```

## 依赖

`wiki-core` / `wiki-llm` / `serde` / `serde_json` / `uuid`

Feature: `formula`（公式引擎）/ `llm-fill`（LLM 列）
