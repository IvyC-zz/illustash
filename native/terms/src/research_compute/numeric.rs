//! 研究计算程序用到的窄范围数值契约，不是模型推断。
//! 保持 float32 中间值、经批准的 float64 线性百分位和标量 introsort。
use std::collections::HashSet;

pub const PAIR_BLOCK_BYTES: usize = 16 * 1024 * 1024;
pub fn pair_block_rows(columns: usize) -> usize {
    (PAIR_BLOCK_BYTES / columns.saturating_mul(4).max(1)).clamp(1, 512)
}

// NumPy 的连续归约用八路并行，两两求和的分段阈值是 128 个元素。
pub fn sum32(v: &[f32]) -> f32 {
    if v.len() < 8 {
        return v.iter().fold(-0.0, |s, x| s + x);
    }
    if v.len() > 128 {
        let mid = (v.len() / 2) / 8 * 8;
        return sum32(&v[..mid]) + sum32(&v[mid..]);
    }
    let mut lane: [f32; 8] = v[..8].try_into().expect("eight lanes");
    let end = v.len() / 8 * 8;
    for chunk in v[8..end].chunks_exact(8) {
        for j in 0..8 {
            lane[j] += chunk[j];
        }
    }
    let s = ((lane[0] + lane[1]) + (lane[2] + lane[3])) + ((lane[4] + lane[5]) + (lane[6] + lane[7]));
    v[end..].iter().fold(s, |s, x| s + x)
}
pub fn normalise(v: &mut [f32]) -> Result<(), String> {
    normalise_layout(v, true)
}
pub fn normalise_layout(v: &mut [f32], contiguous: bool) -> Result<(), String> {
    if v.iter().any(|x| !x.is_finite()) {
        return Err("Invalid CCIP feature vector".into());
    }
    let squares: Vec<_> = v.iter().map(|x| x * x).collect();
    let norm = if contiguous { sum32(&squares) } else { squares.iter().fold(-0.0, |s, x| s + x) }.sqrt();
    // 有限的输入仍可能让平方和溢出为 Inf；除以它会悄悄得到零向量，
    // 所以像零范数一样拒绝（#45）。
    if norm <= 1e-12 || !norm.is_finite() {
        return Err("Invalid CCIP feature vector".into());
    }
    for x in v {
        *x /= norm;
    }
    Ok(())
}
// float64 的 NPY 输入保留 float64 的求范数和除法，再把结果行转成 float32。
pub(crate) fn sum64(v: &[f64]) -> f64 {
    if v.len() < 8 {
        return v.iter().fold(-0.0, |s, x| s + x);
    }
    if v.len() > 128 {
        let mid = (v.len() / 2) / 8 * 8;
        return sum64(&v[..mid]) + sum64(&v[mid..]);
    }
    let mut lane: [f64; 8] = v[..8].try_into().expect("eight lanes");
    let end = v.len() / 8 * 8;
    for chunk in v[8..end].chunks_exact(8) {
        for j in 0..8 {
            lane[j] += chunk[j];
        }
    }
    let sum = ((lane[0] + lane[1]) + (lane[2] + lane[3])) + ((lane[4] + lane[5]) + (lane[6] + lane[7]));
    v[end..].iter().fold(sum, |s, x| s + x)
}
pub fn normalise64(v: &[f64], contiguous: bool) -> Result<Vec<f32>, String> {
    if v.iter().any(|x| !x.is_finite()) {
        return Err("Invalid CCIP feature vector".into());
    }
    let squares: Vec<_> = v.iter().map(|x| x * x).collect();
    let norm = if contiguous { sum64(&squares) } else { squares.iter().fold(-0.0, |s, x| s + x) }.sqrt();
    if norm <= 1e-12 || !norm.is_finite() {
        return Err("Invalid CCIP feature vector".into());
    }
    Ok(v.iter().map(|x| (x / norm) as f32).collect())
}
pub fn distances(a: &[Vec<f32>], b: &[Vec<f32>]) -> Result<Vec<Vec<f32>>, String> {
    if a.is_empty() || b.is_empty() {
        return Ok(vec![vec![]; a.len()]);
    }
    let dim = a[0].len();
    if a.iter().chain(b).any(|v| v.len() != dim) {
        return Err("CCIP feature dimension mismatch".into());
    }
    let left: Vec<_> = a.iter().flatten().copied().collect();
    let right: Vec<_> =
        if a.len() == b.len() && a.as_ptr() == b.as_ptr() { vec![] } else { b.iter().flatten().copied().collect() };
    let right = if right.is_empty() { &left } else { &right };
    let product = illustash_platform::platform_research_math::cross(&left, a.len(), right, b.len(), dim)?;
    Ok(product.chunks(b.len()).map(|row| row.iter().map(|&v| (1.0f32 - v) / 2.0f32).collect()).collect())
}
pub fn centre(rows: &[Vec<f32>], members: &[usize]) -> Result<Vec<f32>, String> {
    let mut out = vec![0.0; rows[0].len()];
    // 在 C 顺序的种子矩阵里沿轴 0 是跨步的：按顺序做 float32 累加。
    for &i in members {
        for (sum, x) in out.iter_mut().zip(&rows[i]) {
            *sum += x;
        }
    }
    for x in &mut out {
        *x /= members.len() as f32;
    }
    let norm = illustash_platform::platform_research_math::dot(&out, &out)?.sqrt();
    if norm <= 1e-12 || !norm.is_finite() {
        return Err("Invalid CCIP prototype centre".into());
    }
    for x in &mut out {
        *x /= norm;
    }
    Ok(out)
}
pub fn percentile90(v: &mut [f32]) -> f64 {
    v.sort_by(f32::total_cmp);
    let at = (v.len() - 1) as f64 * 0.9;
    let lo = at.floor() as usize;
    let hi = at.ceil() as usize;
    let t = at - lo as f64;
    // 用户 2026-10-04 批准：按 NumPy 1.26 用 float32 做端点相减，
    // 再用 float64 计算索引、比例和插值，包括 >=0.5 时的 lerp 分支。
    // 不采用 NumPy 2.x 标量提升后的 dtype，也不引入容差。
    let diff = (v[hi] - v[lo]) as f64;
    if t >= 0.5 {
        v[hi] as f64 - diff * (1.0 - t)
    } else {
        v[lo] as f64 + diff * t
    }
}

/// NumPy 的标量 introsort，包括限制深度后回退到堆排序。
/// SIMD 实现可能以不同的顺序排列并列项；这里不增加跨后端的并列顺序保证。
/// 参考：numpy/core/src/npysort/{quicksort.cpp,npysort_heapsort.h}，v1.26.4。
pub fn argsort<T: PartialOrd>(values: &[T]) -> Vec<usize> {
    let depth = if values.is_empty() { 0 } else { values.len().ilog2() as i32 * 2 };
    argsort_depth(values, depth)
}
fn heapsort<T: PartialOrd>(values: &[T], ids: &mut [usize]) {
    fn sift<T: PartialOrd>(v: &[T], ids: &mut [usize], item: usize, mut i: usize, n: usize) {
        let mut child = 2 * i;
        while child <= n {
            if child < n && v[ids[child - 1]] < v[ids[child]] {
                child += 1;
            }
            if v[item] < v[ids[child - 1]] {
                ids[i - 1] = ids[child - 1];
                i = child;
                child *= 2;
            } else {
                break;
            }
        }
        ids[i - 1] = item;
    }
    let mut n = ids.len();
    for root in (1..=n / 2).rev() {
        let item = ids[root - 1];
        sift(values, ids, item, root, n);
    }
    while n > 1 {
        let item = ids[n - 1];
        ids[n - 1] = ids[0];
        n -= 1;
        sift(values, ids, item, 1, n);
    }
}
fn argsort_depth<T: PartialOrd>(v: &[T], depth: i32) -> Vec<usize> {
    let mut ids: Vec<_> = (0..v.len()).collect();
    if ids.len() < 2 {
        return ids;
    }
    let mut pending = vec![(0, ids.len() - 1, depth)];
    while let Some((mut first, mut last, mut depth)) = pending.pop() {
        // 与标量参考实现一样在出栈时检查；较小的分区立即处理，
        // 较大的分区带着 --depth 入栈。
        if depth < 0 {
            heapsort(v, &mut ids[first..=last]);
            continue;
        }
        while last - first > 15 {
            let mid = first + (last - first) / 2;
            if v[ids[mid]] < v[ids[first]] {
                ids.swap(mid, first);
            }
            if v[ids[last]] < v[ids[mid]] {
                ids.swap(last, mid);
            }
            if v[ids[mid]] < v[ids[first]] {
                ids.swap(mid, first);
            }
            let pivot = ids[mid];
            ids.swap(mid, last - 1);
            let (mut left, mut right) = (first, last - 1);
            loop {
                left += 1;
                while v[ids[left]] < v[pivot] {
                    left += 1;
                }
                right -= 1;
                while v[pivot] < v[ids[right]] {
                    right -= 1;
                }
                if left >= right {
                    break;
                }
                ids.swap(left, right);
            }
            ids.swap(left, last - 1);
            depth -= 1;
            if left - first < last - left {
                pending.push((left + 1, last, depth));
                last = left - 1;
            } else {
                pending.push((first, left - 1, depth));
                first = left + 1;
            }
        }
        for i in first + 1..=last {
            let item = ids[i];
            let mut j = i;
            while j > first && v[item] < v[ids[j - 1]] {
                ids[j] = ids[j - 1];
                j -= 1;
            }
            ids[j] = item;
        }
    }
    ids
}

pub fn groups(rows: &[Vec<f32>], radius: f32, minimum: usize, limit: Option<usize>) -> Result<Vec<Vec<usize>>, String> {
    groups_blocked(rows, radius, minimum, limit, pair_block_rows(rows.len()))
}
fn groups_blocked(
    rows: &[Vec<f32>],
    radius: f32,
    minimum: usize,
    limit: Option<usize>,
    block: usize,
) -> Result<Vec<Vec<usize>>, String> {
    let n = rows.len();
    if n == 0 {
        return Ok(vec![]);
    }
    let bytes = n.div_ceil(8);
    let mut packed = vec![vec![0u8; bytes]; n];
    let mut counts = vec![0i64; n];
    // 每个块之后只留下位邻接表；不分配稠密的 N x N 浮点矩阵。
    for start in (0..n).step_by(block) {
        let distance = distances(&rows[start..(start + block).min(n)], rows)?;
        for i in start..(start + block).min(n) {
            for j in 0..n {
                if distance[i - start][j] <= radius {
                    packed[i][j / 8] |= 1 << (j % 8);
                    counts[i] -= 1;
                }
            }
        }
    }
    let mut free = vec![true; n];
    let mut out = vec![];
    for i in argsort(&counts) {
        if !free[i] {
            continue;
        }
        let members: Vec<_> = (0..n).filter(|&j| free[j] && packed[i][j / 8] & (1 << (j % 8)) != 0).collect();
        if members.len() < minimum {
            continue;
        }
        for &j in &members {
            free[j] = false;
        }
        out.push(members);
        if limit.is_some_and(|limit| limit != 0 && out.len() >= limit) {
            break;
        }
    }
    Ok(out)
}

// PCG64(0) 是现有唯一的抽样契约，不是通用的随机数接口。
struct Pcg {
    state: u128,
    buffered: Option<u32>,
}
impl Pcg {
    fn new() -> Self {
        Self { state: 35399562948360463058890781895381311971, buffered: None }
    }
    fn next(&mut self) -> u64 {
        self.state = self
            .state
            .wrapping_mul(47026247687942121848144207491837523525)
            .wrapping_add(87136372517582989555478159403783844777);
        ((self.state >> 64) as u64 ^ self.state as u64).rotate_right((self.state >> 122) as u32)
    }
    fn next32(&mut self) -> u32 {
        if let Some(x) = self.buffered.take() {
            return x;
        }
        let x = self.next();
        self.buffered = Some((x >> 32) as u32);
        x as u32
    }
    fn bounded(&mut self, hi: usize) -> usize {
        if hi == 0 {
            return 0;
        }
        if hi < u32::MAX as usize {
            let range = hi as u32 + 1;
            let threshold = range.wrapping_neg() % range;
            loop {
                let product = self.next32() as u64 * range as u64;
                if product as u32 >= threshold {
                    return (product >> 32) as usize;
                }
            }
        }
        let range = hi as u64 + 1;
        let threshold = range.wrapping_neg() % range;
        loop {
            let product = self.next() as u128 * range as u128;
            if product as u64 >= threshold {
                return (product >> 64) as usize;
            }
        }
    }
    fn shuffle(&mut self, ids: &mut [usize], first: usize) {
        for i in (first..ids.len()).rev() {
            let j = self.bounded(i);
            ids.swap(i, j);
        }
    }
}
pub fn sample600(n: usize) -> Vec<usize> {
    if n <= 600 {
        return (0..n).collect();
    }
    let mut rng = Pcg::new();
    if n > 10000 && 600 > n / 50 {
        let mut ids: Vec<_> = (0..n).collect();
        rng.shuffle(&mut ids, n - 600);
        return ids[n - 600..].to_vec();
    }
    let mut used = HashSet::new();
    let mut out = Vec::with_capacity(600);
    for j in n - 600..n {
        let value = rng.bounded(j);
        let chosen = if used.contains(&value) { j } else { value };
        used.insert(chosen);
        out.push(chosen);
    }
    rng.shuffle(&mut out, 1);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bitpacked_groups_match_dense_across_blocks_bytes_minimum_and_limit() {
        // 固定的几何形状既有拥挤的同计数领头行，也有孤立的行。
        // 稠密判断与打包方式和分块无关；两者都沿用已接受的标量排序。
        for n in [0, 1, 7, 9, 100, 517] {
            let rows: Vec<_> = (0..n)
                .map(|i| {
                    let angle = (i % 17) as f32 * 0.31;
                    vec![angle.cos(), angle.sin()]
                })
                .collect();
            let d = distances(&rows, &rows).unwrap();
            let counts: Vec<i64> = d.iter().map(|r| -(r.iter().filter(|&&x| x <= 0.07).count() as i64)).collect();
            for (minimum, limit) in [(1, None), (3, None), (3, Some(4))] {
                let mut free = vec![true; n];
                let mut expected = vec![];
                for i in argsort(&counts) {
                    if !free[i] {
                        continue;
                    }
                    let members: Vec<_> = (0..n).filter(|&j| free[j] && d[i][j] <= 0.07).collect();
                    if members.len() < minimum {
                        continue;
                    }
                    for &j in &members {
                        free[j] = false;
                    }
                    expected.push(members);
                    if limit.is_some_and(|v| expected.len() >= v) {
                        break;
                    }
                }
                for block in [3, 8, 19, pair_block_rows(n)] {
                    assert_eq!(
                        groups_blocked(&rows, 0.07, minimum, limit, block).unwrap(),
                        expected,
                        "n={n} block={block}"
                    );
                }
            }
        }
    }
    #[test]
    fn scalar_depth_fallback_matches_numpy_126_heap_and_adversarial_order() {
        use sha2::{Digest, Sha256};
        // 1.26.4 下的 np.argsort(np.ones(40,dtype='i2'), kind='heapsort')。
        let expected: Vec<_> = (1..40).chain([0]).collect();
        assert_eq!(argsort_depth(&[1i16; 40], -1), expected);
        // 固定的标量快速排序 organ-pipe 输入，包括并列项和深度回退。
        let values: Vec<i16> = (0..8192).chain((0..8192).rev()).collect();
        let ids = argsort(&values);
        let bytes: Vec<_> = ids.iter().flat_map(|&i| (i as u64).to_le_bytes()).collect();
        assert_eq!(
            format!("{:x}", Sha256::digest(bytes)),
            "7dcb33c25266c0914ad6e42ec3522fa5b3e6f9165bb05fd7d434dde090612070"
        );
        assert!(ids.windows(2).all(|p| values[p[0]] <= values[p[1]]));
    }
    #[test]
    fn fixed_numpy_pcg64_zero_choice_contract() {
        use sha2::{Digest, Sha256};
        // NumPy PCG64(0).choice(n,600,replace=False)，预期摘要独立求得并固定。
        for (n, expected) in [
            (601, "e4e98363a99cacfef07dde14d38aabcfd21b8f7d419cb3960fb97cc6290447fb"),
            (3000, "3b54af48738f6669621371811bdc5e7c080fc5eda5c5677b9701f7655157a345"),
            (10001, "9f5088c82212285c07389171df69d6ba5c782129fe74d80058188d999640d740"),
            (20000, "9cc0fb51800a3c8e945f79853dd5b0d0c42585405b3049302fc83978b48be0aa"),
            (30050, "9f4329d05fbcadb687fa329fd08b3e9d8b645e4893328e3d86e7d96bab82053d"),
        ] {
            let bytes: Vec<_> = sample600(n).into_iter().flat_map(|i| (i as u64).to_le_bytes()).collect();
            assert_eq!(format!("{:x}", Sha256::digest(bytes)), expected);
        }
    }
    #[test]
    fn block_budget() {
        for n in [100, 1000, 48000, 100000] {
            assert!(pair_block_rows(n) * n * 4 <= PAIR_BLOCK_BYTES);
        }
    }
    #[test]
    fn scalar_quicksort_ties() {
        assert_eq!(
            argsort(&[1i64; 40]),
            vec![
                0, 22, 23, 24, 25, 26, 27, 28, 21, 29, 31, 32, 33, 34, 35, 36, 37, 30, 20, 19, 18, 1, 2, 3, 4, 5, 6, 7,
                8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 38, 39
            ]
        );
    }
    #[test]
    fn finite_norm_overflow_is_rejected_and_largest_finite_square_passes() {
        // #45：平方和溢出的有限元素以前会除以 Inf 得到全零。
        for contiguous in [true, false] {
            for row in [vec![1e30f32; 768], vec![2e19, 0.0], vec![f32::MAX, 1.0]] {
                let mut v = row.clone();
                assert_eq!(normalise_layout(&mut v, contiguous), Err("Invalid CCIP feature vector".into()));
                assert_eq!(v, row, "rejected rows stay untouched");
            }
            for row in [vec![1e200f64; 768], vec![f64::MAX, 1.0]] {
                assert_eq!(normalise64(&row, contiguous), Err("Invalid CCIP feature vector".into()));
            }
            // 最大的有限平方值仍能归一化；[3, 4] 保持精确的位。
            let mut v = vec![1e19f32, 0.0];
            normalise_layout(&mut v, contiguous).unwrap();
            assert_eq!(v, vec![1.0, 0.0]);
            let mut v = vec![3.0f32, 4.0];
            normalise_layout(&mut v, contiguous).unwrap();
            assert_eq!(v.iter().map(|x| x.to_bits()).collect::<Vec<_>>(), [0.6f32.to_bits(), 0.8f32.to_bits()]);
            assert_eq!(normalise64(&[1e40, 0.0], contiguous).unwrap(), vec![1.0, 0.0]);
        }
    }
    #[test]
    fn ordinary_rows_keep_reference_normalisation_bits() {
        use sha2::Digest;
        let row: Vec<f32> = (0..768).map(|i| ((i * 37 % 101) as f32 - 50.0) * 0.013).collect();
        for (contiguous, expected) in [
            (true, "ad69f5ac1e9afa20bbccd5e6bb244177cfe88545a404ffe18d561d419c4873b3"),
            (false, "09f5d705ec2682886574112e61648c7e4d26c527f8093b056d5bf0d43f87807f"),
        ] {
            let mut v = row.clone();
            normalise_layout(&mut v, contiguous).unwrap();
            let digest = sha2::Sha256::digest(v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<_>>());
            assert_eq!(format!("{digest:x}"), expected, "contiguous={contiguous}");
        }
        let row64: Vec<f64> = row.iter().map(|&x| x as f64 * 1e3).collect();
        let digest = sha2::Sha256::digest(
            normalise64(&row64, true).unwrap().iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<_>>(),
        );
        assert_eq!(format!("{digest:x}"), "5b69579d79e7a40bc40f5aa0ad79eb09b652dcd6e5f344af18a8731453a488dd");
    }
    #[test]
    fn floor_and_linear_percentile() {
        assert_eq!(percentile90(&mut [0.0, 0.5, 1.0]), 0.9);
        assert_eq!(groups(&vec![vec![1.0, 0.0]; 9], 0.03, 3, None).unwrap(), vec![(0..9).collect::<Vec<_>>()]);
    }
}
