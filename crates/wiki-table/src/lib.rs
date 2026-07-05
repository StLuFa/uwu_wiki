//! # wiki-table
//!
//! 智能表格：列定义 / 行 CRUD / 视图查询 / LLM 智能填充。
//! 领域实体（行）自适配为 `wiki_llm::TextUnit` 后调用 wiki-llm 端口。

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use wiki_llm::{LlmCapability, TextUnit};

// ===========================================================================
// 基础模型
// ===========================================================================

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ColumnId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RowId(pub String);

impl RowId {
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }
}

impl Default for RowId {
    fn default() -> Self {
        Self::new()
    }
}

impl ColumnId {
    pub fn new(name: &str) -> Self {
        Self(format!("col-{name}"))
    }
}

/// 列类型。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ColumnType {
    Text,
    Number,
    Checkbox,
    Url,
    Date,
    Select,
    MultiSelect,
    Relation,
    Rollup,
    Formula,
    LlmFill,
    CreatedAt,
    UpdatedAt,
    CreatedBy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Column {
    pub id: ColumnId,
    pub name: String,
    pub ty: ColumnType,
}

impl Column {
    pub fn new(name: impl Into<String>, ty: ColumnType) -> Self {
        let s = name.into();
        Self {
            id: ColumnId::new(&s),
            name: s,
            ty,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cell {
    pub column: ColumnId,
    pub value: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Row {
    pub id: RowId,
    pub cells: Vec<Cell>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Table {
    pub id: String,
    pub name: String,
    pub columns: Vec<Column>,
    pub rows: Vec<Row>,
}

impl Table {
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            columns: Vec::new(),
            rows: Vec::new(),
        }
    }

    /// 添加列（列名不可重复）。
    pub fn add_column(&mut self, col: Column) -> Result<(), TableError> {
        if self.columns.iter().any(|c| c.name == col.name) {
            return Err(TableError::DuplicateColumn(col.name));
        }
        self.columns.push(col);
        Ok(())
    }

    /// 移除列（同时从所有行中删除对应单元格）。
    pub fn remove_column(&mut self, col_id: &ColumnId) -> bool {
        let existed = self.columns.iter().any(|c| &c.id == col_id);
        self.columns.retain(|c| &c.id != col_id);
        for row in &mut self.rows {
            row.cells.retain(|c| &c.column != col_id);
        }
        existed
    }

    /// 插入行。
    pub fn insert_row(&mut self, row: Row) -> Result<(), TableError> {
        if self.rows.iter().any(|r| r.id == row.id) {
            return Err(TableError::DuplicateRow(row.id.0.clone()));
        }
        self.rows.push(row);
        Ok(())
    }

    /// 更新行（替换整行 cells）。
    pub fn update_row(&mut self, row_id: &RowId, cells: Vec<Cell>) -> Result<(), TableError> {
        let row = self
            .rows
            .iter_mut()
            .find(|r| &r.id == row_id)
            .ok_or_else(|| TableError::NotFound(format!("row {}", row_id.0)))?;
        row.cells = cells;
        Ok(())
    }

    /// 更新单个单元格。
    pub fn set_cell(&mut self, row_id: &RowId, col_id: &ColumnId, value: serde_json::Value) -> Result<(), TableError> {
        let row = self
            .rows
            .iter_mut()
            .find(|r| &r.id == row_id)
            .ok_or_else(|| TableError::NotFound(format!("row {}", row_id.0)))?;
        if let Some(cell) = row.cells.iter_mut().find(|c| c.column == *col_id) {
            cell.value = value;
        } else {
            row.cells.push(Cell {
                column: col_id.clone(),
                value,
            });
        }
        Ok(())
    }

    /// 删除行。
    pub fn delete_row(&mut self, row_id: &RowId) -> bool {
        let len = self.rows.len();
        self.rows.retain(|r| &r.id != row_id);
        self.rows.len() < len
    }

    /// 按 ID 获取行。
    pub fn get_row(&self, row_id: &RowId) -> Option<&Row> {
        self.rows.iter().find(|r| &r.id == row_id)
    }

    /// 获取单元格值。
    pub fn cell_value(&self, row_id: &RowId, col_id: &ColumnId) -> Option<&serde_json::Value> {
        self.get_row(row_id)
            .and_then(|r| r.cells.iter().find(|c| c.column == *col_id))
            .map(|c| &c.value)
    }

    /// 获取所有 LLM 填充列。
    pub fn llm_columns(&self) -> Vec<&Column> {
        self.columns.iter().filter(|c| c.ty == ColumnType::LlmFill).collect()
    }

    /// 把一行适配为领域无关的 TextUnit（供 wiki-llm 处理）。
    pub fn row_to_text_unit(&self, row: &Row) -> TextUnit {
        let text = row
            .cells
            .iter()
            .map(|c| c.value.to_string())
            .collect::<Vec<_>>()
            .join(" | ");
        TextUnit {
            id: row.id.0.clone(),
            text,
            path: vec![self.id.clone(), row.id.0.clone()],
        }
    }

    pub fn row_count(&self) -> usize {
        self.rows.len()
    }
}

// ===========================================================================
// 错误
// ===========================================================================

#[derive(Debug, Clone)]
pub enum TableError {
    NotFound(String),
    DuplicateColumn(String),
    DuplicateRow(String),
    InvalidValue(String),
}

impl std::fmt::Display for TableError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(s) => write!(f, "not found: {s}"),
            Self::DuplicateColumn(s) => write!(f, "duplicate column: {s}"),
            Self::DuplicateRow(s) => write!(f, "duplicate row: {s}"),
            Self::InvalidValue(s) => write!(f, "invalid value: {s}"),
        }
    }
}

// ===========================================================================
// TableStore
// ===========================================================================

/// 内存表格存储（生产环境由 agent-context-db 注入持久化后端）。
#[derive(Default)]
pub struct TableStore {
    tables: HashMap<String, Table>,
}

impl TableStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn create(&mut self, table: Table) -> Result<(), TableError> {
        if self.tables.contains_key(&table.id) {
            return Err(TableError::DuplicateColumn(format!("table {}", table.id)));
        }
        self.tables.insert(table.id.clone(), table);
        Ok(())
    }

    pub fn get(&self, id: &str) -> Option<&Table> {
        self.tables.get(id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut Table> {
        self.tables.get_mut(id)
    }

    pub fn delete(&mut self, id: &str) -> bool {
        self.tables.remove(id).is_some()
    }

    pub fn list(&self) -> Vec<&Table> {
        self.tables.values().collect()
    }

    pub fn len(&self) -> usize {
        self.tables.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tables.is_empty()
    }
}

// ===========================================================================
// TableView — 声明式查询
// ===========================================================================

/// 过滤操作。
#[derive(Debug, Clone)]
pub enum FilterOp {
    Eq,
    Neq,
    Gt,
    Lt,
    Gte,
    Lte,
    Contains,
    StartsWith,
}

/// 过滤条件。
#[derive(Debug, Clone)]
pub struct Filter {
    pub column: ColumnId,
    pub op: FilterOp,
    pub value: serde_json::Value,
}

/// 排序配置。
#[derive(Debug, Clone)]
pub struct SortConfig {
    pub column: ColumnId,
    pub ascending: bool,
}

/// 表格视图：对 Table 的行进行过滤/排序/分页/分组。
pub struct TableView<'a> {
    table: &'a Table,
    filters: Vec<Filter>,
    sort: Option<SortConfig>,
    hidden_columns: HashSet<ColumnId>,
}

impl<'a> TableView<'a> {
    pub fn new(table: &'a Table) -> Self {
        Self {
            table,
            filters: Vec::new(),
            sort: None,
            hidden_columns: HashSet::new(),
        }
    }

    pub fn filter(mut self, filter: Filter) -> Self {
        self.filters.push(filter);
        self
    }

    pub fn sort_by(mut self, column: impl Into<String>, ascending: bool) -> Self {
        let col_name = column.into();
        if let Some(col) = self.table.columns.iter().find(|c| c.name == col_name) {
            self.sort = Some(SortConfig {
                column: col.id.clone(),
                ascending,
            });
        }
        self
    }

    pub fn hide_column(mut self, col_id: ColumnId) -> Self {
        self.hidden_columns.insert(col_id);
        self
    }

    /// 获取满足所有过滤条件的行。
    pub fn rows(&self) -> Vec<&Row> {
        let mut result: Vec<&Row> = self
            .table
            .rows
            .iter()
            .filter(|row| self.filters.iter().all(|f| Self::match_filter(row, f)))
            .collect();

        if let Some(ref sort) = self.sort {
            result.sort_by(|a, b| {
                let va = self.table.cell_value(&a.id, &sort.column);
                let vb = self.table.cell_value(&b.id, &sort.column);
                let cmp = Self::cmp_values(va, vb);
                if sort.ascending {
                    cmp
                } else {
                    cmp.reverse()
                }
            });
        }

        result
    }

    /// 分页查询。
    pub fn paginate(&self, offset: usize, limit: usize) -> Vec<&Row> {
        self.rows().into_iter().skip(offset).take(limit).collect()
    }

    /// 总行数。
    pub fn count(&self) -> usize {
        self.rows().len()
    }

    /// 分组聚合（按某列分组，返回每组行数）。
    pub fn group_counts(&self, col_id: &ColumnId) -> Vec<(serde_json::Value, usize)> {
        let mut groups: HashMap<String, (serde_json::Value, usize)> = HashMap::new();
        for row in self.rows() {
            if let Some(val) = self.table.cell_value(&row.id, col_id) {
                let key = val.to_string();
                groups
                    .entry(key)
                    .or_insert_with(|| (val.clone(), 0))
                    .1 += 1;
            }
        }
        groups.into_values().collect()
    }

    fn match_filter(row: &Row, filter: &Filter) -> bool {
        let cell_val = row
            .cells
            .iter()
            .find(|c| c.column == filter.column)
            .map(|c| &c.value);
        match cell_val {
            None => false,
            Some(v) => match filter.op {
                FilterOp::Eq => v == &filter.value,
                FilterOp::Neq => v != &filter.value,
                FilterOp::Contains => v
                    .as_str()
                    .map(|s| s.contains(filter.value.as_str().unwrap_or("")))
                    .unwrap_or(false),
                FilterOp::StartsWith => v
                    .as_str()
                    .map(|s| s.starts_with(filter.value.as_str().unwrap_or("")))
                    .unwrap_or(false),
                FilterOp::Gt | FilterOp::Lt | FilterOp::Gte | FilterOp::Lte => {
                    let a = v.as_f64();
                    let b = filter.value.as_f64();
                    match (a, b) {
                        (Some(a), Some(b)) => match filter.op {
                            FilterOp::Gt => a > b,
                            FilterOp::Lt => a < b,
                            FilterOp::Gte => a >= b,
                            FilterOp::Lte => a <= b,
                            _ => false,
                        },
                        _ => false,
                    }
                }
            },
        }
    }

    fn cmp_values(a: Option<&serde_json::Value>, b: Option<&serde_json::Value>) -> std::cmp::Ordering {
        match (a.and_then(|v| v.as_str()), b.and_then(|v| v.as_str())) {
            (Some(a), Some(b)) => a.cmp(b),
            _ => match (a.and_then(|v| v.as_f64()), b.and_then(|v| v.as_f64())) {
                (Some(a), Some(b)) => a.partial_cmp(&b).unwrap_or(std::cmp::Ordering::Equal),
                _ => std::cmp::Ordering::Equal,
            },
        }
    }
}

// ===========================================================================
// LlmFill — LLM 智能填充
// ===========================================================================

/// LLM 填充配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmFillConfig {
    /// 目标列 ID。
    pub column: ColumnId,
    /// Prompt 模板，支持 `{col_name}` 占位符。
    pub prompt_template: String,
    /// 触发方式。
    pub trigger: FillTrigger,
}

/// 触发方式。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FillTrigger {
    /// 行创建时填充。
    OnCreate,
    /// 指定列更新后填充。
    OnUpdate(Vec<ColumnId>),
    /// 手动触发。
    Manual,
}

impl Table {
    /// 对单行执行 LLM 填充（针对所有 LlmFill 列）。
    ///
    /// 返回被填充的 (列名, 生成内容) 列表。
    pub async fn fill_row_with_llm(
        &mut self,
        llm: &dyn LlmCapability,
        row_id: &RowId,
    ) -> Result<Vec<(String, String)>, TableError> {
        let llm_cols: Vec<Column> = self.columns.iter()
            .filter(|c| c.ty == ColumnType::LlmFill)
            .cloned()
            .collect();

        let mut filled = Vec::new();
        for col in &llm_cols {
            let unit = self.row_to_text_unit(
                self.get_row(row_id)
                    .ok_or_else(|| TableError::NotFound(format!("row {}", row_id.0)))?,
            );

            let completion = llm
                .complete(&unit, &format!("[{}]: ", col.name))
                .await
                .unwrap_or_else(|_| String::new());

            if !completion.is_empty() {
                self.set_cell(row_id, &col.id, serde_json::json!(completion.trim()))?;
                filled.push((col.name.clone(), completion));
            }
        }
        Ok(filled)
    }

    /// 对新插入的行列表触发 OnCreate 填充。
    pub async fn fill_on_create(
        &mut self,
        llm: &dyn LlmCapability,
        new_row_ids: &[RowId],
    ) -> Result<Vec<(String, Vec<(String, String)>)>, TableError> {
        let mut results = Vec::new();
        for row_id in new_row_ids {
            let filled = self.fill_row_with_llm(llm, row_id).await?;
            if !filled.is_empty() {
                results.push((row_id.0.clone(), filled));
            }
        }
        Ok(results)
    }
}

// ===========================================================================
// 测试
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_table() -> Table {
        let mut t = Table::new("t1", "tasks");
        t.add_column(Column::new("title", ColumnType::Text)).unwrap();
        t.add_column(Column::new("priority", ColumnType::Number)).unwrap();
        t.add_column(Column::new("done", ColumnType::Checkbox)).unwrap();
        t
    }

    fn sample_row(id: &str, title: &str, priority: i32, done: bool) -> Row {
        let t = sample_table();
        Row {
            id: RowId(id.into()),
            cells: vec![
                Cell { column: t.columns[0].id.clone(), value: serde_json::json!(title) },
                Cell { column: t.columns[1].id.clone(), value: serde_json::json!(priority) },
                Cell { column: t.columns[2].id.clone(), value: serde_json::json!(done) },
            ],
        }
    }

    #[test]
    fn table_insert_and_get_row() {
        let mut t = sample_table();
        let row = sample_row("r1", "learn rust", 5, false);
        t.insert_row(row).unwrap();
        assert_eq!(t.row_count(), 1);
        assert!(t.get_row(&RowId("r1".into())).is_some());
    }

    #[test]
    fn table_duplicate_row_rejected() {
        let mut t = sample_table();
        let r = sample_row("r1", "x", 1, false);
        t.insert_row(r.clone()).unwrap();
        assert!(t.insert_row(r).is_err());
    }

    #[test]
    fn table_update_row() {
        let mut t = sample_table();
        t.insert_row(sample_row("r1", "old", 1, false)).unwrap();
        let col_id = t.columns[0].id.clone();
        t.update_row(&RowId("r1".into()), vec![Cell {
            column: col_id.clone(),
            value: serde_json::json!("new"),
        }])
        .unwrap();
        assert_eq!(
            t.cell_value(&RowId("r1".into()), &col_id).unwrap().as_str().unwrap(),
            "new"
        );
    }

    #[test]
    fn table_delete_row() {
        let mut t = sample_table();
        t.insert_row(sample_row("r1", "x", 1, false)).unwrap();
        assert!(t.delete_row(&RowId("r1".into())));
        assert!(t.get_row(&RowId("r1".into())).is_none());
    }

    #[test]
    fn table_add_remove_column() {
        let mut t = sample_table();
        let col = Column::new("notes", ColumnType::Text);
        let col_id = col.id.clone();
        assert!(t.add_column(col).is_ok());
        assert_eq!(t.columns.len(), 4);
        assert!(t.remove_column(&col_id));
        assert_eq!(t.columns.len(), 3);
    }

    #[test]
    fn duplicate_column_name_rejected() {
        let mut t = sample_table();
        let col = Column::new("title", ColumnType::Select);
        assert!(t.add_column(col).is_err());
    }

    #[test]
    fn table_store_crud() {
        let mut store = TableStore::new();
        let t = sample_table();
        store.create(t).unwrap();
        assert_eq!(store.len(), 1);
        assert!(store.get("t1").is_some());
        assert!(store.delete("t1"));
        assert!(store.is_empty());
    }

    #[test]
    fn view_filter_eq() {
        let mut t = sample_table();
        t.insert_row(sample_row("r1", "alpha", 5, false)).unwrap();
        t.insert_row(sample_row("r2", "beta", 3, true)).unwrap();

        let view = TableView::new(&t).filter(Filter {
            column: t.columns[2].id.clone(),
            op: FilterOp::Eq,
            value: serde_json::json!(true),
        });
        assert_eq!(view.count(), 1);
        assert_eq!(view.rows()[0].id.0, "r2");
    }

    #[test]
    fn view_sort_numeric() {
        let mut t = sample_table();
        t.insert_row(sample_row("r1", "alpha", 5, false)).unwrap();
        t.insert_row(sample_row("r2", "beta", 3, true)).unwrap();

        let view = TableView::new(&t).sort_by("priority", true);
        let rows = view.rows();
        assert_eq!(rows[0].id.0, "r2"); // 3 < 5
        assert_eq!(rows[1].id.0, "r1");
    }

    #[test]
    fn view_paginate() {
        let mut t = sample_table();
        for i in 0..10 {
            t.insert_row(sample_row(&format!("r{i}"), &format!("task{i}"), i, false))
                .unwrap();
        }
        let view = TableView::new(&t);
        assert_eq!(view.count(), 10);
        assert_eq!(view.paginate(0, 3).len(), 3);
        assert_eq!(view.paginate(9, 5).len(), 1);
    }

    #[test]
    fn view_group_counts() {
        let mut t = sample_table();
        t.insert_row(sample_row("r1", "a", 5, true)).unwrap();
        t.insert_row(sample_row("r2", "b", 3, false)).unwrap();
        t.insert_row(sample_row("r3", "c", 5, true)).unwrap();

        let view = TableView::new(&t);
        let groups = view.group_counts(&t.columns[2].id);
        assert_eq!(groups.len(), 2);
    }

    #[test]
    fn row_adapts_to_text_unit() {
        let t = sample_table();
        let row = sample_row("r1", "hello", 1, true);
        let unit = t.row_to_text_unit(&row);
        assert_eq!(unit.id, "r1");
        assert_eq!(unit.path, vec!["t1".to_string(), "r1".to_string()]);
    }

    #[test]
    fn llm_columns_detection() {
        let mut t = sample_table();
        t.add_column(Column::new("summary", ColumnType::LlmFill)).unwrap();
        assert_eq!(t.llm_columns().len(), 1);
    }

    #[test]
    fn set_cell_creates_new_cell() {
        let mut t = sample_table();
        let col = Column::new("extra", ColumnType::Text);
        let col_id = col.id.clone();
        t.add_column(col).unwrap();
        t.insert_row(sample_row("r1", "x", 1, false)).unwrap();

        t.set_cell(&RowId("r1".into()), &col_id, serde_json::json!("value")).unwrap();
        assert_eq!(
            t.cell_value(&RowId("r1".into()), &col_id).unwrap().as_str().unwrap(),
            "value"
        );
    }
}
