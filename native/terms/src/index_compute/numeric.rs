//! 既有的索引数值契约：不新增下限，也不放宽阈值容差。
use crate::recognition_compute::anchors;
use crate::{Error, Result};

pub fn merge(rows: &[Vec<f32>]) -> Result<Vec<f32>> {
    merge32(rows, false)
}
pub fn percentiles(rows: &[Vec<f32>], anchor: &[f32]) -> Result<[f64; 3]> {
    stats32(rows, false, anchor)
}
pub fn core(rows: &[Vec<f32>]) -> Result<Option<Vec<usize>>> {
    NpyMatrix {
        dimension: rows.first().map_or(0, Vec::len),
        fortran_order: false,
        values: NpyValues::Float32(rows.to_vec()),
    }
    .core()
}
// NumPy v1.26.4 的标量 argintroselect（selection.cpp）；调用方只请求一个 kth。
// 用户 2026-10-04 批准：各操作系统都保持这种并列顺序，包括 Windows 上 SIMD 带来的差异。
fn partition(v: &[f32], ids: &mut [usize], kth: usize) {
    let (mut low, mut high) = (0, ids.len() - 1);
    if kth < 3 {
        for i in 0..=kth {
            let mut best = i;
            for j in i + 1..ids.len() {
                if v[ids[j]] < v[ids[best]] {
                    best = j;
                }
            }
            ids.swap(i, best);
        }
        return;
    }
    if kth == ids.len() - 1 {
        let mut best = 0;
        for j in 1..ids.len() {
            if v[ids[j]] >= v[ids[best]] {
                best = j;
            }
        }
        ids.swap(kth, best);
        return;
    }
    let mut depth = ids.len().ilog2() as i32 * 2;
    while low + 1 < high {
        let (mut left, mut right) = (low + 1, high);
        if depth > 0 || right - left < 5 {
            let mid = low + (high - low) / 2;
            if v[ids[high]] < v[ids[mid]] {
                ids.swap(high, mid);
            }
            if v[ids[high]] < v[ids[low]] {
                ids.swap(high, low);
            }
            if v[ids[low]] < v[ids[mid]] {
                ids.swap(low, mid);
            }
            ids.swap(mid, low + 1);
        } else {
            let start = left;
            let n = (right - left) / 5;
            for i in 0..n {
                let s = start + i * 5;
                for (a, b) in [(1, 0), (4, 3), (3, 0), (4, 1), (2, 1)] {
                    if v[ids[s + a]] < v[ids[s + b]] {
                        ids.swap(s + a, s + b);
                    }
                }
                let med = if v[ids[s + 3]] < v[ids[s + 2]] {
                    if v[ids[s + 3]] < v[ids[s + 1]] {
                        1
                    } else {
                        3
                    }
                } else {
                    2
                };
                ids.swap(s + med, start + i);
            }
            if n > 2 {
                partition(v, &mut ids[start..start + n], n / 2);
            }
            ids.swap(start + n / 2, low);
            left -= 1;
            right += 1;
        }
        depth -= 1;
        let pivot = v[ids[low]];
        loop {
            left += 1;
            while v[ids[left]] < pivot {
                left += 1;
            }
            right -= 1;
            while pivot < v[ids[right]] {
                right -= 1;
            }
            if right < left {
                break;
            }
            ids.swap(left, right);
        }
        ids.swap(low, right);
        if right >= kth {
            high = right.saturating_sub(1);
        }
        if right <= kth {
            low = left;
        }
    }
    if high == low + 1 && v[ids[high]] < v[ids[low]] {
        ids.swap(high, low);
    }
}
pub fn nearest_nine(values: &[f32]) -> Vec<usize> {
    if values.is_empty() {
        return vec![];
    }
    let k = 9.min(values.len());
    let mut ids: Vec<_> = (0..values.len()).collect();
    partition(values, &mut ids, k - 1);
    ids.truncate(k);
    let subset: Vec<_> = ids.iter().map(|&i| values[i]).collect();
    anchors::nearest_order(&subset).into_iter().map(|i| ids[i]).collect()
}
use crate::research_compute::features::{NpyMatrix, NpyValues};
macro_rules! reference_math {
    ($merge:ident,$stats:ident,$ty:ty,$sum:path,$dot:path,$product:path) => {
        fn $merge(rows: &[Vec<$ty>], fortran: bool) -> Result<Vec<$ty>> {
            if rows.is_empty()
                || rows[0].is_empty()
                || rows.iter().any(|r| r.len() != rows[0].len() || r.iter().any(|x| !x.is_finite()))
            {
                return Err(Error::Invalid("无效索引参考向量".into()));
            }
            let contiguous = !fortran || rows.len() == 1 || rows[0].len() == 1;
            let lengths: Vec<$ty> = rows
                .iter()
                .map(|r| {
                    let squares: Vec<_> = r.iter().map(|x| x * x).collect();
                    if contiguous { $sum(&squares) } else { squares.iter().fold(-0.0, |s, x| s + x) }.sqrt()
                })
                .collect();
            if lengths.iter().any(|&n| n == 0. || !n.is_finite()) {
                return Err(Error::Invalid("无效索引参考模长".into()));
            }
            let mut centre = vec![0.; rows[0].len()];
            for (j, c) in centre.iter_mut().enumerate() {
                let column: Vec<_> = rows.iter().zip(&lengths).map(|(r, l)| r[j] / l).collect();
                *c = if fortran { $sum(&column) } else { column.iter().fold(0., |s, x| s + x) } / rows.len() as $ty;
            }
            let norm = $dot(&centre, &centre).map_err(Error::Invalid)?.sqrt();
            if norm == 0. || !norm.is_finite() {
                return Err(Error::Invalid("无效索引锚点模长".into()));
            }
            let mean = $sum(&lengths) / lengths.len() as $ty;
            for c in &mut centre {
                *c = *c / norm * mean;
            }
            Ok(centre)
        }
        fn $stats(rows: &[Vec<$ty>], fortran: bool, anchor: &[$ty]) -> Result<[f64; 3]> {
            let contiguous = !fortran || rows.len() == 1 || rows[0].len() == 1;
            let mut units = vec![];
            for r in rows {
                let squares: Vec<_> = r.iter().map(|x| x * x).collect();
                let norm = if contiguous { $sum(&squares) } else { squares.iter().fold(-0.0, |s, x| s + x) }.sqrt();
                units.push(r.iter().map(|x| x / norm).collect::<Vec<_>>());
            }
            let norm = $dot(anchor, anchor).map_err(Error::Invalid)?.sqrt();
            let a: Vec<_> = anchor.iter().map(|x| x / norm).collect();
            let values: Vec<_> = if fortran {
                (0..anchor.len()).flat_map(|j| units.iter().map(move |r| r[j])).collect()
            } else {
                units.into_iter().flatten().collect()
            };
            let mut d: Vec<$ty> = $product(&values, rows.len(), anchor.len(), fortran, &a)
                .map_err(Error::Invalid)?
                .into_iter()
                .map(|x| (1. - x) / 2.)
                .collect();
            d.sort_by(<$ty>::total_cmp);
            Ok([0.5, 0.75, 0.9].map(|q| {
                let at = (d.len() - 1) as f64 * q;
                let lo = at.floor() as usize;
                let hi = at.ceil() as usize;
                let t = at - lo as f64;
                // 录制的索引算法使用标量 q：NumPy 的弱类型提升保留输入的 dtype。
                // 学习另有一份单独批准的 float64 百分位契约。
                let diff = d[hi] - d[lo];
                if t >= 0.5 {
                    (d[hi] - diff * ((1.0 - t) as $ty)) as f64
                } else {
                    (d[lo] + diff * (t as $ty)) as f64
                }
            }))
        }
    };
}
reference_math!(
    merge32,
    stats32,
    f32,
    crate::research_compute::contiguous_sum32,
    illustash_platform::platform_research_math::dot,
    illustash_platform::platform_research_math::index_product32
);
reference_math!(
    merge64,
    stats64,
    f64,
    crate::research_compute::contiguous_sum64,
    illustash_platform::platform_research_math::index_dot64,
    illustash_platform::platform_research_math::index_product64
);
impl NpyMatrix {
    pub(super) fn raw32(&self) -> Vec<Vec<f32>> {
        match &self.values {
            NpyValues::Float32(r) => r.clone(),
            NpyValues::Float64(r) => r.iter().map(|r| r.iter().map(|&x| x as f32).collect()).collect(),
        }
    }
    pub(super) fn selected(&self, ids: &[usize]) -> Self {
        Self {
            dimension: self.dimension,
            fortran_order: false,
            values: match &self.values {
                NpyValues::Float32(r) => NpyValues::Float32(ids.iter().map(|&i| r[i].clone()).collect()),
                NpyValues::Float64(r) => NpyValues::Float64(ids.iter().map(|&i| r[i].clone()).collect()),
            },
        }
    }
    pub(super) fn merged(&self) -> Result<Vec<f32>> {
        match &self.values {
            NpyValues::Float32(r) => merge32(r, self.fortran_order),
            NpyValues::Float64(r) => Ok(merge64(r, self.fortran_order)?.into_iter().map(|x| x as f32).collect()),
        }
    }
    pub(super) fn built(&self) -> Result<(Vec<f32>, [f64; 3])> {
        match &self.values {
            NpyValues::Float32(r) => {
                let a = merge32(r, self.fortran_order)?;
                let p = stats32(r, self.fortran_order, &a)?;
                Ok((a, p))
            }
            NpyValues::Float64(r) => {
                let a = merge64(r, self.fortran_order)?;
                let p = stats64(r, self.fortran_order, &a)?;
                Ok((a.into_iter().map(|x| x as f32).collect(), p))
            }
        }
    }
    pub(super) fn core(&self) -> Result<Option<Vec<usize>>> {
        let rows = self.raw32();
        if rows.is_empty() {
            return Ok(None);
        }
        let d = anchors::cross(&rows, &rows)?;
        let seed = (0..rows.len())
            .max_by_key(|&i| (d[i].iter().filter(|&&x| x <= 0.1).count(), std::cmp::Reverse(i)))
            .expect("nonempty");
        let mut keep: Vec<_> = d[seed].iter().enumerate().filter(|(_, d)| **d <= 0.12).map(|(i, _)| i).collect();
        for _ in 0..5 {
            if keep.len() < 2 {
                return Ok(None);
            }
            let anchor = self.selected(&keep).merged()?;
            let d = anchors::cross(&[anchor], &rows)?.remove(0);
            let next: Vec<_> = d.iter().enumerate().filter(|(_, d)| **d <= 0.1).map(|(i, _)| i).collect();
            if next == keep {
                break;
            }
            keep = next;
        }
        Ok((keep.len() >= 5 && keep.len() as f64 >= 0.4 * rows.len() as f64).then_some(keep))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn merge_scale_and_core_majority() {
        let rows = vec![vec![2., 0.]; 6];
        assert_eq!(merge(&rows).unwrap(), vec![2., 0.]);
        assert_eq!(percentiles(&rows, &[2., 0.]).unwrap(), [0.; 3]);
        assert_eq!(core(&rows).unwrap(), Some((0..6).collect()));
        assert_eq!(core(&rows[..4]).unwrap(), None);
        assert!(merge(&[vec![0., 0.]]).is_err());
    }
    #[test]
    fn nearest_ties_preserve_partition_budget() {
        assert_eq!(nearest_nine(&[1.; 10]), vec![6, 4, 8, 7, 0, 5, 3, 1, 2]);
        assert!(nearest_nine(&[]).is_empty());
    }
}
