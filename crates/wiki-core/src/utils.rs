//! 通用数学工具函数。

/// 余弦相似度。
///
/// 返回两个向量的余弦相似度，范围 `[-1.0, 1.0]`。
/// 任一向量为零向量时返回 `0.0`。
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na * nb)
    }
}
