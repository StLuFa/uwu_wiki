//! 图布局算法：树形 / 分层 / 力导向。

use std::collections::{HashMap, HashSet};

use crate::{Edge, Graph, NodeId};

/// 布局结果：节点 → 2D 坐标。
#[derive(Debug, Clone)]
pub struct LayoutResult {
    pub positions: HashMap<NodeId, (f32, f32)>,
}

// =============================================================================
// 树形布局
// =============================================================================

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
        let first_child_x = positions.get(children[0]).map(|p| p.0).unwrap_or(0.0);
        x = (first_child_x + last_x) / 2.0;
    }

    positions.insert(node_id.clone(), (x, y));
    x
}

// =============================================================================
// 分层布局
// =============================================================================

/// 分层布局：拓扑排序后按层排列。
pub fn layered_layout(graph: &Graph) -> LayoutResult {
    let mut positions = HashMap::new();

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

// =============================================================================
// 力导向布局
// =============================================================================

/// 力导向布局（Fruchterman-Reingold 算法）。
///
/// 排斥力将节点推开，吸引力沿边收缩。迭代收敛后得到自然布局。
pub fn force_directed_layout(
    graph: &Graph,
    width: f32,
    height: f32,
    iterations: usize,
) -> LayoutResult {
    use rand::Rng as _;
    let mut rng = rand::thread_rng();

    let n = graph.nodes.len();
    if n == 0 {
        return LayoutResult {
            positions: HashMap::new(),
        };
    }

    let area = width * height;
    let k = (area / n as f32).sqrt() * 0.7;

    let mut positions: HashMap<NodeId, (f32, f32)> = graph
        .nodes
        .iter()
        .map(|node| {
            (
                node.id.clone(),
                (rng.gen_range(0.0..width), rng.gen_range(0.0..height)),
            )
        })
        .collect();

    let mut t = width.min(height) / 10.0;

    for _iter in 0..iterations {
        let mut disp: HashMap<NodeId, (f32, f32)> = graph
            .nodes
            .iter()
            .map(|node| (node.id.clone(), (0.0, 0.0)))
            .collect();

        // 排斥力：每对节点互相推开。
        for i in 0..n {
            for j in (i + 1)..n {
                let ni = &graph.nodes[i];
                let nj = &graph.nodes[j];
                let (xi, yi) = positions[&ni.id];
                let (xj, yj) = positions[&nj.id];
                let dx = xi - xj;
                let dy = yi - yj;
                let dist = (dx * dx + dy * dy).sqrt().max(0.01);
                let repulsion = k * k / dist;

                let d = disp.get_mut(&ni.id).unwrap();
                d.0 += repulsion * dx / dist;
                d.1 += repulsion * dy / dist;
                let d = disp.get_mut(&nj.id).unwrap();
                d.0 -= repulsion * dx / dist;
                d.1 -= repulsion * dy / dist;
            }
        }

        // 吸引力：沿边拉近。
        for edge in &graph.edges {
            let (xi, yi) = positions[&edge.from];
            let (xj, yj) = positions[&edge.to];
            let dx = xi - xj;
            let dy = yi - yj;
            let dist = (dx * dx + dy * dy).sqrt().max(0.01);
            let attraction = dist * dist / k;

            let d = disp.get_mut(&edge.from).unwrap();
            d.0 -= attraction * dx / dist;
            d.1 -= attraction * dy / dist;
            let d = disp.get_mut(&edge.to).unwrap();
            d.0 += attraction * dx / dist;
            d.1 += attraction * dy / dist;
        }

        // 应用位移并限制范围。
        for node in &graph.nodes {
            let (mut dx, mut dy) = disp[&node.id];
            let disp_mag = (dx * dx + dy * dy).sqrt();
            if disp_mag > 0.0 {
                dx = dx / disp_mag * disp_mag.min(t);
                dy = dy / disp_mag * disp_mag.min(t);
            }
            let pos = positions.get_mut(&node.id).unwrap();
            pos.0 = (pos.0 + dx).clamp(0.0, width);
            pos.1 = (pos.1 + dy).clamp(0.0, height);
        }

        t *= 0.95;
    }

    LayoutResult { positions }
}
