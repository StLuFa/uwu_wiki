//! # wiki-graph
//!
//! 流程图 / 思维导图：节点/边模型 + 索引化存储 + 遍历 + 导出 + 布局。
//! 图节点自适配为 `wiki_llm::TextUnit`，不反向依赖横切层。

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use wiki_llm::TextUnit;

// ===========================================================================
// 基础模型
// ===========================================================================

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NodeId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EdgeId(pub String);

impl EdgeId {
    pub fn new(from: &NodeId, to: &NodeId) -> Self {
        Self(format!("{}-{}", from.0, to.0))
    }
}

/// 图节点类型。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum NodeKind {
    Process,
    Decision,
    Start,
    End,
    Idea,
    Note,
    /// 思维导图分支起点。
    Branch,
    /// 注释/标注。
    Annotation,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphNode {
    pub id: NodeId,
    pub kind: NodeKind,
    pub label: String,
    /// 额外标签（用于分类/搜索）。
    pub tags: Vec<String>,
    /// 颜色（hex，可选）。
    pub color: Option<String>,
    pub embedding: Option<Vec<f32>>,
}

impl GraphNode {
    pub fn new(id: impl Into<String>, kind: NodeKind, label: impl Into<String>) -> Self {
        Self {
            id: NodeId(id.into()),
            kind,
            label: label.into(),
            tags: Vec::new(),
            color: None,
            embedding: None,
        }
    }

    /// 节点的出度（在给定图中）。
    pub fn out_degree(&self, graph: &Graph) -> usize {
        graph.edges.iter().filter(|e| e.from == self.id).count()
    }

    /// 节点的入度。
    pub fn in_degree(&self, graph: &Graph) -> usize {
        graph.edges.iter().filter(|e| e.to == self.id).count()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    pub from: NodeId,
    pub to: NodeId,
    pub label: Option<String>,
}

impl Edge {
    pub fn new(from: NodeId, to: NodeId) -> Self {
        Self {
            from,
            to,
            label: None,
        }
    }

    pub fn id(&self) -> EdgeId {
        EdgeId::new(&self.from, &self.to)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Graph {
    pub id: String,
    pub title: Option<String>,
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<Edge>,
}

impl Graph {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            title: None,
            nodes: Vec::new(),
            edges: Vec::new(),
        }
    }

    // ---- Node CRUD ----

    pub fn add_node(&mut self, node: GraphNode) -> Result<(), GraphError> {
        if self.nodes.iter().any(|n| n.id == node.id) {
            return Err(GraphError::DuplicateNode(node.id.0.clone()));
        }
        self.nodes.push(node);
        Ok(())
    }

    pub fn remove_node(&mut self, node_id: &NodeId) -> Result<(), GraphError> {
        let existed = self.nodes.iter().any(|n| &n.id == node_id);
        if !existed {
            return Err(GraphError::NotFound(format!("node {}", node_id.0)));
        }
        self.nodes.retain(|n| &n.id != node_id);
        self.edges.retain(|e| &e.from != node_id && &e.to != node_id);
        Ok(())
    }

    pub fn update_node(&mut self, node_id: &NodeId, label: impl Into<String>) -> Result<(), GraphError> {
        let node = self
            .nodes
            .iter_mut()
            .find(|n| &n.id == node_id)
            .ok_or_else(|| GraphError::NotFound(format!("node {}", node_id.0)))?;
        node.label = label.into();
        Ok(())
    }

    pub fn get_node(&self, node_id: &NodeId) -> Option<&GraphNode> {
        self.nodes.iter().find(|n| &n.id == node_id)
    }

    // ---- Edge CRUD ----

    pub fn add_edge(&mut self, edge: Edge) -> Result<(), GraphError> {
        if !self.nodes.iter().any(|n| n.id == edge.from) {
            return Err(GraphError::NotFound(format!("source node {}", edge.from.0)));
        }
        if !self.nodes.iter().any(|n| n.id == edge.to) {
            return Err(GraphError::NotFound(format!("target node {}", edge.to.0)));
        }
        self.edges.push(edge);
        Ok(())
    }

    pub fn remove_edge(&mut self, from: &NodeId, to: &NodeId) -> bool {
        let len = self.edges.len();
        self.edges.retain(|e| &e.from != from || &e.to != to);
        self.edges.len() < len
    }

    pub fn edges_from(&self, node_id: &NodeId) -> Vec<&Edge> {
        self.edges.iter().filter(|e| &e.from == node_id).collect()
    }

    pub fn edges_to(&self, node_id: &NodeId) -> Vec<&Edge> {
        self.edges.iter().filter(|e| &e.to == node_id).collect()
    }

    // ---- Traversal ----

    /// 命中节点的一跳邻居（RAG context 扩展用）。
    pub fn neighbors(&self, node: &NodeId) -> Vec<&GraphNode> {
        let neighbor_ids: Vec<&NodeId> = self
            .edges
            .iter()
            .filter_map(|e| {
                if &e.from == node {
                    Some(&e.to)
                } else if &e.to == node {
                    Some(&e.from)
                } else {
                    None
                }
            })
            .collect();
        self.nodes
            .iter()
            .filter(|n| neighbor_ids.contains(&&n.id))
            .collect()
    }

    /// BFS 遍历（从 start 出发的有向可达节点）。
    pub fn bfs(&self, start: &NodeId) -> Vec<&GraphNode> {
        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        let mut result = Vec::new();

        if self.get_node(start).is_some() {
            queue.push_back(start.clone());
            visited.insert(start.clone());
        }

        while let Some(current) = queue.pop_front() {
            if let Some(node) = self.get_node(&current) {
                result.push(node);
                for edge in self.edges_from(&current) {
                    if visited.insert(edge.to.clone()) {
                        queue.push_back(edge.to.clone());
                    }
                }
            }
        }
        result
    }

    /// DFS 遍历（从 start 出发的有向可达节点）。
    pub fn dfs(&self, start: &NodeId) -> Vec<&GraphNode> {
        let mut visited = HashSet::new();
        let mut result = Vec::new();
        self.dfs_visit(start, &mut visited, &mut result);
        result
    }

    fn dfs_visit<'a>(&'a self, node_id: &NodeId, visited: &mut HashSet<NodeId>, result: &mut Vec<&'a GraphNode>) {
        if let Some(node) = self.get_node(node_id)
            && visited.insert(node_id.clone())
        {
            result.push(node);
            let neighbors: Vec<NodeId> = self
                .edges_from(node_id)
                .iter()
                .map(|e| e.to.clone())
                .collect();
            for next in neighbors {
                self.dfs_visit(&next, visited, result);
            }
        }
    }

    /// 拓扑排序（有向无环图）。Kahn 算法。
    pub fn topological_sort(&self) -> Result<Vec<&GraphNode>, GraphError> {
        let mut in_degree: HashMap<&NodeId, usize> = HashMap::new();
        for node in &self.nodes {
            in_degree.entry(&node.id).or_insert(0);
        }
        for edge in &self.edges {
            *in_degree.entry(&edge.to).or_insert(0) += 1;
        }

        let mut queue: VecDeque<&NodeId> = in_degree
            .iter()
            .filter_map(|(id, deg)| if *deg == 0 { Some(*id) } else { None })
            .collect();

        let mut result = Vec::new();
        while let Some(id) = queue.pop_front() {
            if let Some(node) = self.get_node(id) {
                result.push(node);
            }
            for edge in self.edges_from(id) {
                let deg = in_degree.get_mut(&edge.to).unwrap();
                *deg -= 1;
                if *deg == 0 {
                    queue.push_back(&edge.to);
                }
            }
        }

        if result.len() != self.nodes.len() {
            return Err(GraphError::CycleDetected);
        }
        Ok(result)
    }

    // 节点数。
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    pub fn edge_count(&self) -> usize {
        self.edges.len()
    }

    /// 节点适配为领域无关 TextUnit。
    pub fn node_to_text_unit(&self, node: &GraphNode) -> TextUnit {
        TextUnit {
            id: node.id.0.clone(),
            text: node.label.clone(),
            path: vec![self.id.clone(), node.id.0.clone()],
        }
    }
}

// ===========================================================================
// 错误
// ===========================================================================

#[derive(Debug, Clone)]
pub enum GraphError {
    NotFound(String),
    DuplicateNode(String),
    DuplicateGraph(String),
    CycleDetected,
}

impl std::fmt::Display for GraphError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(s) => write!(f, "not found: {s}"),
            Self::DuplicateNode(s) => write!(f, "duplicate node: {s}"),
            Self::DuplicateGraph(s) => write!(f, "duplicate graph: {s}"),
            Self::CycleDetected => write!(f, "cycle detected (DAG required)"),
        }
    }
}

// ===========================================================================
// GraphStore — 索引化内存存储
// ===========================================================================

/// 图存储：按 ARCHITECTURE.md §6 设计的索引化内存存储。
///
/// 倒排索引支持 O(1) 按图/标签/类型查节点。
#[derive(Default)]
pub struct GraphStore {
    graphs: HashMap<String, Graph>,
    /// node_id → graph_id
    node_to_graph: HashMap<NodeId, String>,
    /// tag → node_id 集合
    tag_index: HashMap<String, HashSet<NodeId>>,
    /// NodeKind → node_id 集合
    type_index: HashMap<NodeKind, HashSet<NodeId>>,
}

impl GraphStore {
    pub fn new() -> Self {
        Self::default()
    }

    // ---- Graph CRUD ----

    pub fn create(&mut self, graph: Graph) -> Result<(), GraphError> {
        if self.graphs.contains_key(&graph.id) {
            return Err(GraphError::DuplicateGraph(graph.id.clone()));
        }
        // 建立索引。
        for node in &graph.nodes {
            self.index_node(&graph.id, node);
        }
        self.graphs.insert(graph.id.clone(), graph);
        Ok(())
    }

    pub fn get(&self, id: &str) -> Option<&Graph> {
        self.graphs.get(id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut Graph> {
        self.graphs.get_mut(id)
    }

    pub fn delete(&mut self, id: &str) -> bool {
        if let Some(graph) = self.graphs.remove(id) {
            for node in &graph.nodes {
                self.deindex_node(node);
            }
            true
        } else {
            false
        }
    }

    pub fn list(&self) -> Vec<&Graph> {
        self.graphs.values().collect()
    }

    pub fn len(&self) -> usize {
        self.graphs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.graphs.is_empty()
    }

    // ---- Node CRUD (带索引维护) ----

    /// 向图添加节点并索引。
    pub fn add_node(&mut self, graph_id: &str, node: GraphNode) -> Result<(), GraphError> {
        let graph = self
            .graphs
            .get_mut(graph_id)
            .ok_or_else(|| GraphError::NotFound(format!("graph {graph_id}")))?;
        graph.add_node(node.clone())?;
        self.index_node(graph_id, &node);
        Ok(())
    }

    /// 从图移除节点并清理索引。
    pub fn remove_node(&mut self, graph_id: &str, node_id: &NodeId) -> Result<(), GraphError> {
        let graph = self
            .graphs
            .get_mut(graph_id)
            .ok_or_else(|| GraphError::NotFound(format!("graph {graph_id}")))?;
        if let Some(node) = graph.get_node(node_id) {
            let n = node.clone();
            graph.remove_node(node_id)?;
            self.deindex_node(&n);
        }
        Ok(())
    }

    // ---- 索引查询 ----

    /// 按标签查节点（跨图）。
    pub fn by_tag(&self, tag: &str) -> Vec<&GraphNode> {
        self.tag_index
            .get(tag)
            .map(|ids| {
                ids.iter()
                    .filter_map(|nid| self.find_node(nid))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// 按类型查节点（跨图）。
    pub fn by_type(&self, kind: &NodeKind) -> Vec<&GraphNode> {
        self.type_index
            .get(kind)
            .map(|ids| {
                ids.iter()
                    .filter_map(|nid| self.find_node(nid))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// 查某图的所有节点。
    pub fn nodes_of_graph(&self, graph_id: &str) -> Vec<&GraphNode> {
        match self.graphs.get(graph_id) {
            Some(g) => g.nodes.iter().collect(),
            None => Vec::new(),
        }
    }

    /// 按 ID 查找节点（跨图）。
    pub fn find_node(&self, node_id: &NodeId) -> Option<&GraphNode> {
        let graph_id = self.node_to_graph.get(node_id)?;
        self.graphs.get(graph_id)?.get_node(node_id)
    }

    // ---- 内部索引 ----

    fn index_node(&mut self, graph_id: &str, node: &GraphNode) {
        self.node_to_graph.insert(node.id.clone(), graph_id.into());
        for tag in &node.tags {
            self.tag_index
                .entry(tag.clone())
                .or_default()
                .insert(node.id.clone());
        }
        self.type_index
            .entry(node.kind.clone())
            .or_default()
            .insert(node.id.clone());
    }

    fn deindex_node(&mut self, node: &GraphNode) {
        self.node_to_graph.remove(&node.id);
        for tag in &node.tags {
            if let Some(set) = self.tag_index.get_mut(tag) {
                set.remove(&node.id);
            }
        }
        if let Some(set) = self.type_index.get_mut(&node.kind) {
            set.remove(&node.id);
        }
    }
}

// ===========================================================================
// 导出（Mermaid / PlantUML）
// ===========================================================================

/// 节点形状 → Mermaid 括号语法映射。
fn mermaid_shape(kind: &NodeKind) -> (&str, &str) {
    match kind {
        NodeKind::Start | NodeKind::End => ("([", "])"),
        NodeKind::Decision => ("{", "}"),
        NodeKind::Process => ("[", "]"),
        NodeKind::Idea | NodeKind::Branch => ("[", "]"),
        NodeKind::Note => ("[", "]"),
        NodeKind::Annotation => ("[", "]"),
    }
}

impl Graph {
    /// 导出为 Mermaid flowchart。
    ///
    /// 输出示例：
    /// ```mermaid
    /// flowchart TD
    ///   n1[开始]
    ///   n2{判断}
    ///   n1 --> n2
    /// ```
    pub fn to_mermaid(&self) -> String {
        let mut out = String::from("flowchart TD\n");
        for node in &self.nodes {
            let (open, close) = mermaid_shape(&node.kind);
            out.push_str(&format!("  {}{}{}{}\n", node.id.0, open, node.label, close));
        }

        // 边。
        for edge in &self.edges {
            let label = edge
                .label
                .as_ref()
                .map(|l| format!("|{l}|"))
                .unwrap_or_default();
            out.push_str(&format!("  {} -->{label} {}\n", edge.from.0, edge.to.0));
        }
        out
    }

    /// 导出为 PlantUML 活动图。
    pub fn to_plantuml(&self) -> String {
        let mut out = String::from("@startuml\n");
        for node in &self.nodes {
            out.push_str(&format!("  :{}: as {}\n", node.label, node.id.0));
        }
        for edge in &self.edges {
            let label = edge
                .label
                .as_ref()
                .map(|l| format!(" : {l}"))
                .unwrap_or_default();
            out.push_str(&format!("  {} -->{label} {}\n", edge.from.0, edge.to.0));
        }
        out.push_str("@enduml\n");
        out
    }
}

// ===========================================================================
// 布局算法
// ===========================================================================

/// 布局结果：节点 → 2D 坐标。
#[derive(Debug, Clone)]
pub struct LayoutResult {
    pub positions: HashMap<NodeId, (f32, f32)>,
}

/// 简单树形布局：从 root 出发，每层向下排列子节点。
pub fn tree_layout(graph: &Graph, root: &NodeId) -> LayoutResult {
    let mut positions = HashMap::new();
    let mut visited = HashSet::new();
    tree_layout_dfs(graph, root, 0, 0, &mut positions, &mut visited);
    LayoutResult { positions }
}

fn tree_layout_dfs(
    graph: &Graph,
    node_id: &NodeId,
    depth: usize,
    sibling_index: usize,
    positions: &mut HashMap<NodeId, (f32, f32)>,
    visited: &mut HashSet<NodeId>,
) -> f32 {
    if !visited.insert(node_id.clone()) {
        return sibling_index as f32;
    }

    let y = depth as f32 * 80.0;
    let children: Vec<&Edge> = graph.edges_from(node_id);
    let children: Vec<&NodeId> = children.iter().map(|e| &e.to).collect();

    let x: f32;
    if children.is_empty() {
        x = sibling_index as f32 * 120.0;
    } else {
        let mut last_x = sibling_index as f32;
        for (i, child) in children.iter().enumerate() {
            last_x = tree_layout_dfs(graph, child, depth + 1, i, positions, visited);
        }
        // 父节点居中。
        let first_child_x = positions.get(children[0]).map(|p| p.0).unwrap_or(0.0);
        x = (first_child_x + last_x) / 2.0;
    }

    positions.insert(node_id.clone(), (x, y));
    x
}

/// 分层布局：拓扑排序后按层排列。
pub fn layered_layout(graph: &Graph) -> LayoutResult {
    let mut positions = HashMap::new();

    // 按拓扑排序分到各层。
    let sorted = graph.topological_sort().unwrap_or_else(|_| graph.nodes.iter().collect());
    let mut layer: HashMap<&NodeId, usize> = HashMap::new();

    for node in &sorted {
        let max_parent_layer = graph
            .edges_to(&node.id)
            .iter()
            .filter_map(|e| layer.get(&&e.from))
            .max()
            .copied()
            .unwrap_or(0);
        layer.insert(&node.id, max_parent_layer + 1);
    }

    // 每层水平排列。
    let mut layer_counts: HashMap<usize, usize> = HashMap::new();
    for node in &sorted {
        let l = layer[&node.id];
        let count = layer_counts.entry(l).or_insert(0);
        positions.insert(
            node.id.clone(),
            (*count as f32 * 140.0 + 20.0, l as f32 * 100.0),
        );
        *count += 1;
    }

    LayoutResult { positions }
}

// ===========================================================================
// 测试
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_graph() -> Graph {
        let mut g = Graph::new("g1");
        g.add_node(GraphNode::new("a", NodeKind::Start, "开始")).unwrap();
        g.add_node(GraphNode::new("b", NodeKind::Decision, "判断")).unwrap();
        g.add_node(GraphNode::new("c", NodeKind::Process, "处理")).unwrap();
        g.add_node(GraphNode::new("d", NodeKind::End, "结束")).unwrap();
        g.add_edge(Edge::new(NodeId("a".into()), NodeId("b".into()))).unwrap();
        g.add_edge(Edge::new(NodeId("b".into()), NodeId("c".into()))).unwrap();
        g.add_edge(Edge::new(NodeId("c".into()), NodeId("d".into()))).unwrap();
        g
    }

    fn tagged_graph() -> Graph {
        let mut g = Graph::new("g2");
        let mut n1 = GraphNode::new("x", NodeKind::Process, "登录");
        n1.tags.push("auth".into());
        let mut n2 = GraphNode::new("y", NodeKind::Decision, "校验");
        n2.tags.push("auth".into());
        g.add_node(n1).unwrap();
        g.add_node(n2).unwrap();
        g.add_edge(Edge::new(NodeId("x".into()), NodeId("y".into()))).unwrap();
        g
    }

    // ---- Node/Edge CRUD ----

    #[test]
    fn graph_add_and_remove_node() {
        let mut g = Graph::new("g");
        g.add_node(GraphNode::new("n1", NodeKind::Process, "task")).unwrap();
        assert_eq!(g.node_count(), 1);
        g.remove_node(&NodeId("n1".into())).unwrap();
        assert_eq!(g.node_count(), 0);
    }

    #[test]
    fn graph_duplicate_node_rejected() {
        let mut g = Graph::new("g");
        g.add_node(GraphNode::new("n1", NodeKind::Process, "a")).unwrap();
        assert!(g.add_node(GraphNode::new("n1", NodeKind::Process, "b")).is_err());
    }

    #[test]
    fn remove_node_cleans_edges() {
        let mut g = sample_graph();
        g.remove_node(&NodeId("b".into())).unwrap();
        // 所有经过 b 的边应被清除。
        assert!(g.edges.iter().all(|e| e.from.0 != "b" && e.to.0 != "b"));
    }

    #[test]
    fn edge_crud() {
        let mut g = Graph::new("g");
        g.add_node(GraphNode::new("a", NodeKind::Start, "A")).unwrap();
        g.add_node(GraphNode::new("b", NodeKind::End, "B")).unwrap();
        g.add_edge(Edge::new(NodeId("a".into()), NodeId("b".into()))).unwrap();
        assert_eq!(g.edge_count(), 1);
        assert!(g.remove_edge(&NodeId("a".into()), &NodeId("b".into())));
        assert_eq!(g.edge_count(), 0);
    }

    #[test]
    fn edge_to_missing_node_rejected() {
        let mut g = Graph::new("g");
        g.add_node(GraphNode::new("a", NodeKind::Start, "A")).unwrap();
        assert!(g.add_edge(Edge::new(NodeId("a".into()), NodeId("z".into()))).is_err());
    }

    // ---- Traversal ----

    #[test]
    fn bfs_order() {
        let g = sample_graph();
        let order: Vec<&str> = g.bfs(&NodeId("a".into())).iter().map(|n| n.id.0.as_str()).collect();
        assert_eq!(order, vec!["a", "b", "c", "d"]);
    }

    #[test]
    fn dfs_order() {
        let g = sample_graph();
        let order: Vec<&str> = g.dfs(&NodeId("a".into())).iter().map(|n| n.id.0.as_str()).collect();
        // DFS: a → b → c → d
        assert_eq!(order.len(), 4);
        assert_eq!(order[0], "a");
        assert_eq!(order[3], "d");
    }

    #[test]
    fn topological_sort_linear_graph() {
        let g = sample_graph();
        let sorted = g.topological_sort().unwrap();
        let ids: Vec<&str> = sorted.iter().map(|n| n.id.0.as_str()).collect();
        assert_eq!(ids, vec!["a", "b", "c", "d"]);
    }

    #[test]
    fn neighbors_one_hop() {
        let g = sample_graph();
        let ns = g.neighbors(&NodeId("b".into()));
        assert_eq!(ns.len(), 2); // a (incoming) + c (outgoing)
    }

    // ---- GraphStore ----

    #[test]
    fn store_crud() {
        let mut store = GraphStore::new();
        store.create(sample_graph()).unwrap();
        assert_eq!(store.len(), 1);
        assert!(store.get("g1").is_some());
        assert!(store.delete("g1"));
        assert!(store.is_empty());
    }

    #[test]
    fn store_tag_index() {
        let mut store = GraphStore::new();
        store.create(tagged_graph()).unwrap();

        let auth_nodes = store.by_tag("auth");
        assert_eq!(auth_nodes.len(), 2);
    }

    #[test]
    fn store_type_index() {
        let mut store = GraphStore::new();
        store.create(sample_graph()).unwrap();

        let decisions = store.by_type(&NodeKind::Decision);
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].id.0, "b");

        let processes = store.by_type(&NodeKind::Process);
        assert_eq!(processes.len(), 1);
    }

    #[test]
    fn store_find_node_cross_graph() {
        let mut store = GraphStore::new();
        store.create(sample_graph()).unwrap();

        let node = store.find_node(&NodeId("c".into()));
        assert!(node.is_some());
        assert_eq!(node.unwrap().label, "处理");
    }

    // ---- Export ----

    #[test]
    fn mermaid_export_contains_nodes_and_edges() {
        let g = sample_graph();
        let md = g.to_mermaid();
        assert!(md.starts_with("flowchart"));
        assert!(md.contains("a"));
        assert!(md.contains("-->"));
    }

    #[test]
    fn plantuml_export_contains_tags() {
        let g = sample_graph();
        let puml = g.to_plantuml();
        assert!(puml.starts_with("@startuml"));
        assert!(puml.ends_with("@enduml\n"));
        assert!(puml.contains("-->"));
    }

    // ---- Layout ----

    #[test]
    fn tree_layout_covers_all_nodes() {
        let g = sample_graph();
        let layout = tree_layout(&g, &NodeId("a".into()));
        assert_eq!(layout.positions.len(), 4);
        // 根在顶部（y 最小）。
        let root_y = layout.positions[&NodeId("a".into())].1;
        let leaf_y = layout.positions[&NodeId("d".into())].1;
        assert!(root_y < leaf_y);
    }

    #[test]
    fn layered_layout_positions_all_nodes() {
        let g = sample_graph();
        let layout = layered_layout(&g);
        assert_eq!(layout.positions.len(), 4);
    }

    #[test]
    fn node_adapts_to_text_unit() {
        let g = sample_graph();
        let n = g.get_node(&NodeId("a".into())).unwrap();
        let unit = g.node_to_text_unit(n);
        assert_eq!(unit.text, "开始");
        assert_eq!(unit.id, "a");
    }

    #[test]
    fn node_degree() {
        let g = sample_graph();
        let b = g.get_node(&NodeId("b".into())).unwrap();
        assert_eq!(b.in_degree(&g), 1);
        assert_eq!(b.out_degree(&g), 1);
    }
}
