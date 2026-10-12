//! Character-anchor computation; callers supply only their authorised snapshot.
//! No private anchor discovery or Features cache. Metadata is from source.characters.
use crate::{Error, Result};
use std::collections::{HashMap, HashSet};
#[derive(Clone, Default)]
pub struct Metadata {
    pub p90: f32,
    pub p50: f32,
    pub p75: f32,
    pub skin: bool,
    pub post_count: i64,
    pub gender_conf: f64,
}
pub struct AnchorIndex {
    pub entities: Vec<String>,
    pub p90: Vec<f32>,
    pub p50: Vec<f32>,
    pub p75: Vec<f32>,
}
fn invalid() -> Error {
    Error::Invalid("角色锚点向量/ID/元数据不匹配".into())
}
fn unit(row: &[f32]) -> Result<Vec<f32>> {
    if row.is_empty() || row.iter().any(|v| !v.is_finite()) {
        return Err(invalid());
    }
    let squares = crate::research_compute::contiguous_sum32(&row.iter().map(|v| v * v).collect::<Vec<_>>());
    // Finite but huge elements overflow the sum; dividing by an infinite norm would silently give an all-zero row.
    if !squares.is_finite() {
        return Err(Error::Invalid("特征向量数值过大，无法归一化".into()));
    }
    // ccip.cross has a 1e-12 denominator floor, including all-zero rows.
    let norm = squares.sqrt().max(1e-12);
    Ok(row.iter().map(|v| v / norm).collect())
}
/// Rectangular (1-cos)/2 in f32, including the Python zero-vector floor.
pub fn cross(a: &[Vec<f32>], b: &[Vec<f32>]) -> Result<Vec<Vec<f32>>> {
    cross_checked(a, b, &|| Ok(()))
}
pub(crate) fn cross_checked(a: &[Vec<f32>], b: &[Vec<f32>], check: &dyn Fn() -> Result<()>) -> Result<Vec<Vec<f32>>> {
    let dimension = a.first().or_else(|| b.first()).map_or(0, Vec::len);
    if a.iter().chain(b).any(|r| r.len() != dimension) {
        return Err(invalid());
    }
    let a = a
        .iter()
        .map(|r| {
            check()?;
            unit(r)
        })
        .collect::<Result<Vec<_>>>()?;
    let b = b
        .iter()
        .map(|r| {
            check()?;
            unit(r)
        })
        .collect::<Result<Vec<_>>>()?;
    let left = a.iter().flatten().copied().collect::<Vec<_>>();
    let right = b.iter().flatten().copied().collect::<Vec<_>>();
    check()?;
    let product = illustash_platform::platform_research_math::cross(&left, a.len(), &right, b.len(), dimension)
        .map_err(Error::Invalid)?;
    check()?;
    if b.is_empty() {
        return Ok(vec![vec![]; a.len()]);
    }
    Ok(product.chunks_exact(b.len()).map(|r| r.iter().map(|v| (1.0f32 - v) / 2.0f32).collect()).collect())
}
/// Direct neighbours choose the greatest score, then strictly uphill hops.
/// This differs from connected-component/single-linkage aliasing.
pub fn anchor_index(vectors: &[Vec<f32>], ids: &[String], metadata: &HashMap<String, Metadata>) -> Result<AnchorIndex> {
    anchor_index_checked(vectors, ids, metadata, &|| Ok(()))
}
pub(crate) fn anchor_index_checked(
    vectors: &[Vec<f32>],
    ids: &[String],
    metadata: &HashMap<String, Metadata>,
    check: &dyn Fn() -> Result<()>,
) -> Result<AnchorIndex> {
    anchor_index_blocks(vectors, ids, metadata, 1024, check)
}
/// Index scan preserves the legacy 1024-row BLAS shape at exact alias boundaries.
/// Both paths keep the legacy shape, with cooperative checks around BLAS and each alias row.
pub(crate) fn anchor_index_for_scan(
    vectors: &[Vec<f32>],
    ids: &[String],
    metadata: &HashMap<String, Metadata>,
) -> Result<AnchorIndex> {
    anchor_index_blocks(vectors, ids, metadata, 1024, &|| Ok(()))
}
fn anchor_index_blocks(
    vectors: &[Vec<f32>],
    ids: &[String],
    metadata: &HashMap<String, Metadata>,
    rows: usize,
    check: &dyn Fn() -> Result<()>,
) -> Result<AnchorIndex> {
    check()?;
    if vectors.len() != ids.len() || ids.iter().collect::<HashSet<_>>().len() != ids.len() {
        return Err(invalid());
    }
    let score = |i: usize| {
        let m = metadata.get(&ids[i]).cloned().unwrap_or_default();
        (m.gender_conf >= 0.9, !m.skin, m.post_count, ids[i].as_str())
    };
    let mut alias: Vec<usize> = (0..ids.len()).collect();
    // Bound each uncancellable BLAS call; cancellation is checked before/after
    // the call and for every normalization/alias row. No threshold tolerance.
    for start in (0..ids.len()).step_by(rows) {
        let distances = cross_checked(&vectors[start..(start + rows).min(ids.len())], vectors, check)?;
        for (r, row) in distances.iter().enumerate() {
            check()?;
            for (j, d) in row.iter().enumerate() {
                let i = start + r;
                if *d <= 0.01 && score(j) > score(alias[i]) {
                    alias[i] = j;
                }
            }
        }
    }
    let entities = alias
        .iter()
        .map(|&i| {
            check()?;
            let mut i = i;
            while alias[i] != i {
                i = alias[i];
            }
            Ok(ids[i].clone())
        })
        .collect::<Result<Vec<_>>>()?;
    let columns = |get: fn(&Metadata) -> f32| ids.iter().map(|id| metadata.get(id).map_or(0., get)).collect();
    Ok(AnchorIndex { entities, p90: columns(|m| m.p90), p50: columns(|m| m.p50), p75: columns(|m| m.p75) })
}
/// Curated p90 vs explicitly patched p75; missing threshold stays zero (rank only).
pub fn claim_thresholds(ids: &[String], index: &AnchorIndex, patched: &HashSet<String>) -> Result<Vec<f32>> {
    if ids.len() != index.p90.len() || ids.len() != index.p75.len() {
        return Err(invalid());
    }
    Ok(ids.iter().enumerate().map(|(i, id)| if patched.contains(id) { index.p75[i] } else { index.p90[i] }).collect())
}
/// Scalar NumPy aquicksort ordering (including equal-value partition swaps),
/// used by the current macOS CPU producer. Never silently substitute a stable sort.
pub fn nearest_order(values: &[f32]) -> Vec<usize> {
    fn sort(values: &[f32], indices: &mut [usize], mut depth: i32) {
        if indices.len() < 2 {
            return;
        }
        if depth < 0 {
            // NumPy's fallback is a max heap with strict comparisons.
            fn sift(values: &[f32], a: &mut [usize], mut root: usize, end: usize) {
                loop {
                    let mut child = root * 2 + 1;
                    if child >= end {
                        break;
                    }
                    if child + 1 < end && values[a[child]] < values[a[child + 1]] {
                        child += 1;
                    }
                    if values[a[root]] < values[a[child]] {
                        a.swap(root, child);
                        root = child;
                    } else {
                        break;
                    }
                }
            }
            for root in (0..indices.len() / 2).rev() {
                let n = indices.len();
                sift(values, indices, root, n);
            }
            for end in (1..indices.len()).rev() {
                indices.swap(0, end);
                sift(values, indices, 0, end);
            }
            return;
        }
        if indices.len() > 16 {
            let last = indices.len() - 1;
            let middle = last / 2;
            if values[indices[middle]] < values[indices[0]] {
                indices.swap(middle, 0);
            }
            if values[indices[last]] < values[indices[middle]] {
                indices.swap(last, middle);
            }
            if values[indices[middle]] < values[indices[0]] {
                indices.swap(middle, 0);
            }
            let pivot = values[indices[middle]];
            indices.swap(middle, last - 1);
            let (mut left, mut right) = (0, last - 1);
            loop {
                left += 1;
                while values[indices[left]] < pivot {
                    left += 1;
                }
                right -= 1;
                while pivot < values[indices[right]] {
                    right -= 1;
                }
                if left >= right {
                    break;
                }
                indices.swap(left, right);
            }
            indices.swap(left, last - 1);
            depth -= 1;
            let (low, high) = indices.split_at_mut(left);
            sort(values, low, depth);
            sort(values, &mut high[1..], depth);
        } else {
            for i in 1..indices.len() {
                let value = indices[i];
                let mut j = i;
                while j > 0 && values[value] < values[indices[j - 1]] {
                    indices[j] = indices[j - 1];
                    j -= 1;
                }
                indices[j] = value;
            }
        }
    }
    let mut indices: Vec<usize> = (0..values.len()).collect();
    let depth = if values.is_empty() { 0 } else { 2 * (usize::BITS - 1 - values.len().leading_zeros()) as i32 };
    sort(values, &mut indices, depth);
    indices
}
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Claim {
    pub asset: String,
    pub character: String,
    pub distance: f32,
    pub threshold: f32,
    pub confidence: f64,
}
pub fn claims(
    asset: &str,
    vectors: &[Vec<f32>],
    feature: &[f32],
    index: &AnchorIndex,
    limits: &[f32],
) -> Result<Vec<Claim>> {
    claims_checked(asset, vectors, feature, index, limits, &|| Ok(()))
}
pub(crate) fn claims_checked(
    asset: &str,
    vectors: &[Vec<f32>],
    feature: &[f32],
    index: &AnchorIndex,
    limits: &[f32],
    check: &dyn Fn() -> Result<()>,
) -> Result<Vec<Claim>> {
    check()?;
    if vectors.len() != limits.len() || vectors.len() != index.entities.len() {
        return Err(invalid());
    }
    let distances = cross_checked(vectors, &[feature.to_vec()], check)?.into_iter().map(|r| r[0]).collect::<Vec<_>>();
    let mut seen = HashSet::new();
    let mut claims = Vec::new();
    for a in nearest_order(&distances).into_iter().take(9) {
        check()?;
        let character = &index.entities[a];
        // Python marks an alias as seen before checking the first anchor's threshold.
        if !seen.insert(character.clone()) {
            continue;
        }
        if limits[a] > 0. && distances[a] <= limits[a] {
            claims.push(Claim {
                asset: asset.into(),
                character: character.clone(),
                distance: distances[a],
                threshold: limits[a],
                confidence: 1. - f64::from(distances[a]),
            });
        }
    }
    Ok(claims)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_alias_block_checks_cancellation_before_after_product_and_per_row() {
        let vectors = vec![vec![1., 0.]; 129];
        let ids = (0..129).map(|i| format!("synthetic-{i}")).collect::<Vec<_>>();
        // initial check, 129 left norms, 129 right norms, pre/post BLAS, alias rows.
        for stop in [260, 261, 262, 263] {
            let checks = std::cell::Cell::new(0usize);
            let result = anchor_index_checked(&vectors, &ids, &HashMap::new(), &|| {
                checks.set(checks.get() + 1);
                if checks.get() == stop {
                    return Err(Error::Invalid("synthetic cancel".into()));
                }
                Ok(())
            });
            assert!(result.err().expect("cancellation must abort").to_string().contains("synthetic cancel"));
            assert_eq!(checks.get(), stop);
        }
    }
    fn fixture(posts: [i64; 3], gender: [f64; 3]) -> (Vec<Vec<f32>>, Vec<String>, HashMap<String, Metadata>) {
        let vectors = [0f32, 0.16, 0.32].into_iter().map(|v| vec![v.cos(), v.sin()]).collect();
        let ids = ["a", "b", "c"].map(String::from).to_vec();
        let meta = ids
            .iter()
            .enumerate()
            .map(|(i, id)| {
                (
                    id.clone(),
                    Metadata {
                        post_count: posts[i],
                        gender_conf: gender[i],
                        p90: 0.08,
                        p50: 0.03,
                        p75: 0.05,
                        ..Metadata::default()
                    },
                )
            })
            .collect();
        (vectors, ids, meta)
    }
    #[test]
    fn low_hub_does_not_bridge_and_uphill_hops_do() {
        let (v, ids, m) = fixture([3351, 50, 900], [1., 0.5, 1.]);
        assert_eq!(anchor_index(&v, &ids, &m).unwrap().entities, vec!["a", "a", "c"]);
        let (v, ids, m) = fixture([9000, 516, 351], [1.; 3]);
        let index = anchor_index(&v, &ids, &m).unwrap();
        assert_eq!(index.entities, vec!["a"; 3]);
        assert_eq!(claim_thresholds(&ids, &index, &HashSet::from(["b".into()])).unwrap(), vec![0.08, 0.05, 0.08]);
    }
    #[test]
    fn cross_zero_and_opposite_vectors() {
        assert_eq!(
            cross(&[vec![0., 0.], vec![1., 0.]], &[vec![1., 0.], vec![-1., 0.]]).unwrap(),
            vec![vec![0.5, 0.5], vec![0., 1.]]
        );
        assert!(cross(&[vec![f32::NAN]], &[vec![1.]]).is_err());
        assert!(cross(&[vec![1.]], &[vec![1., 2.]]).is_err());
    }
    #[test]
    fn overflowing_norm_is_refused_while_zero_rows_keep_the_floor() {
        // #75: 2e19² overflows f32, so the old code divided by an infinite norm and returned zeros.
        let huge: Vec<f32> = vec![2e19, 0.];
        fn refused<T>(result: Result<T>) -> bool {
            matches!(result, Err(Error::Invalid(m)) if m.contains("数值过大"))
        }
        assert!(refused(unit(&huge)));
        assert!(refused(cross(std::slice::from_ref(&huge), &[vec![1., 0.]])));
        assert!(refused(cross(&[vec![1., 0.]], std::slice::from_ref(&huge))));
        // Building the anchor index and matching an image feature both fail instead of scoring 0.5 everywhere.
        let ids = vec!["a".to_string(), "b".to_string()];
        assert!(refused(anchor_index_for_scan(&[vec![1., 0.], huge.clone()], &ids, &HashMap::new())));
        let index = AnchorIndex { entities: vec!["c".into()], p90: vec![], p50: vec![], p75: vec![] };
        assert!(refused(claims("a", &[vec![1., 0.]], &huge, &index, &[1.])));
        // Largest values whose squares still sum to a finite number are normalized as before.
        let edge: Vec<f32> = vec![1.3e19, 1.3e19];
        let norm = (edge[0] * edge[0] + edge[1] * edge[1]).sqrt();
        assert_eq!(unit(&edge).unwrap(), vec![edge[0] / norm, edge[1] / norm]);
        assert_eq!(unit(&[0., 0.]).unwrap(), vec![0., 0.]);
        assert_eq!(cross(&[vec![0., 0.]], &[vec![1., 0.]]).unwrap(), vec![vec![0.5]]);
    }
    #[test]
    fn closest_alias_is_seen_before_threshold_and_only_nine_are_considered() {
        let vectors = vec![vec![1., 0.], vec![0.99, 0.1]];
        let index = AnchorIndex { entities: vec!["same".into(); 2], p90: vec![], p50: vec![], p75: vec![] };
        assert!(claims("a", &vectors, &[1., 0.], &index, &[0., 1.]).unwrap().is_empty());
        let vectors = (0..10).map(|i| vec![(i as f32 * 0.1).cos(), (i as f32 * 0.1).sin()]).collect::<Vec<_>>();
        let index =
            AnchorIndex { entities: (0..10).map(|i| format!("c{i}")).collect(), p90: vec![], p50: vec![], p75: vec![] };
        let rows = claims("a", &vectors, &[1., 0.], &index, &[1.; 10]).unwrap();
        assert_eq!(rows.len(), 9);
        assert!(rows.iter().all(|c| c.character != "c9"));
    }
}
