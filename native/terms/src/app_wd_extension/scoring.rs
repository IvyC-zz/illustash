//! WD 扩展头的批量打分（#582）：一块图片的向量和全部头的权重做一次 f32 矩阵乘粗筛，离阈值近的头再用原来的 f64
//! 顺序累加复算。命中集合和写进证据的概率与逐图逐头的标量实现（[`probability`]）逐位相同。
//!
//! # 为什么粗筛不会漏掉命中
//!
//! 记 n 为维数，x、w 为一张图的向量和一个头的权重（都是有限的 f32 时才走粗筛），b 为偏置，t 为阈值。
//! 标量实现算的是 `z = fl64(s + b)`，s 是 f64 顺序累加的点积，再按 `F(z) = f64(f32(1 / (1 + exp(-z))))` 和 `F(z) >= t`
//! 判命中。粗筛用矩阵乘得到 f32 的 g，只在 `fl64(g + b + δ) < c` 时跳过这个头，其余一律复算。要证明跳过的头在标量实现
//! 里一定不命中，分两步：
//!
//! 1. **z ≤ g + b + δ。** 记真实点积为 S，A = Σ|xᵢwᵢ|。任何求和顺序（分块、FMA、多线程归约都算）的 f32 内积都满足
//!    |g − S| ≤ γ₃₂·A + μ（Higham，《Accuracy and Stability of Numerical Algorithms》第 2 版第 3.1 节的内积误差界），
//!    γ₃₂ = n·u/(1 − n·u)，u = 2⁻²⁴；μ 是下溢和清零（FTZ/DAZ）带来的绝对误差，每次运算不超过 2⁻¹²⁶，
//!    被清零的次正规输入合计不超过 2⁻¹²⁶·√n·(‖x‖ + ‖w‖)。f32 相乘在 f64 里是精确的，
//!    顺序累加的 s 满足 |s − S| ≤ γ₆₄·A，γ₆₄ 约 1.1×10⁻¹³。由 Cauchy–Schwarz，A ≤ ‖x‖·‖w‖。
//!    所以取 δ = 2γ₃₂·‖x‖·‖w‖ + μ + ρ：2γ₃₂ 盖住 γ₃₂ + γ₆₄ 和两个范数在 f64 里算出来的相对误差（约 10⁻¹³）；
//!    μ 取 2⁻¹⁰⁰·n·(1 + ‖x‖ + ‖w‖)；ρ = 10⁻¹²·(|g| + |b| + 2γ₃₂·‖x‖·‖w‖ + 1)，盖住 z 和 g + b + δ 自身在 f64 里的几次舍入
//!    （每次不超过 2⁻⁵³ 乘以参与运算的绝对值之和）。
//! 2. **z < c 时 F(z) < t。** 设 exp 的相对误差不超过 E = 2⁻⁴⁰（约 4,000 ulp，远宽于各平台的 libm）。对 z' ≤ z* − m，
//!    m ≥ 3E 时，算出来的 exp(−z') 不小于算出来的 exp(−z*)；之后的加 1、取倒数、转 f32 都是单调的舍入，
//!    所以 F(z') ≤ F(z*)。这一步不需要 exp 本身单调。z* 用二分法在 F 上找，直接验证 F(z*) < t；
//!    c = z* − 10⁻⁹ − 10⁻¹²·|z*|，减法的舍入远小于 10⁻⁹ − 3E。
//!
//! t ≤ 0 时取 c = −∞（全部复算）；t > 1 或不是数时取 c = +∞：F 的值不超过 1，没有头能命中。
//! g、δ 或 g + b + δ 不是有限数（权重有非有限值、f32 溢出）时比较为假，照样复算。
//! 所以结果和后端的求和顺序无关：macOS 的 Accelerate、Windows 的固定 OpenBLAS 和本文件的可移植循环给出相同的命中和概率。

/// 标量实现：f64 顺序累加和 sigmoid，概率取 f32。复算和测试都用它，不能改写。
pub(super) fn probability(vector: &[f32], weights: &[f32], bias: f32) -> f32 {
    // 累加与 sigmoid 用 f64，阈值与证据分数沿用冻结实验的 f32 概率精度。
    let sum: f64 = vector.iter().zip(weights).map(|(&x, &w)| f64::from(x) * f64::from(w)).sum();
    (1.0 / (1.0 + (-(sum + f64::from(bias))).exp())) as f32
}

/// 标量实现在已经算好 `z = s + b` 之后的判定值 F(z)。
fn decided(z: f64) -> f64 {
    f64::from((1.0 / (1.0 + (-z).exp())) as f32)
}

/// 粗筛的下界 c：z < c 时 F(z) < t，见模块文档第 2 步。
fn cut(threshold: f64) -> f64 {
    if threshold.is_nan() || threshold > 1.0 {
        return f64::INFINITY;
    }
    let (mut lo, mut hi) = (-1000.0_f64, 1000.0_f64);
    // 预期 F(lo) = 0、F(hi) = 1；不成立时（t ≤ 0）不做粗筛，全部复算。
    if decided(lo) >= threshold || decided(hi) < threshold {
        return f64::NEG_INFINITY;
    }
    for _ in 0..200 {
        let mid = lo + (hi - lo) / 2.0;
        if mid <= lo || mid >= hi {
            break;
        }
        if decided(mid) < threshold {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    lo - 1e-9 - 1e-12 * lo.abs()
}

fn norm(values: &[f32]) -> f64 {
    values.iter().map(|&v| f64::from(v) * f64::from(v)).sum::<f64>().sqrt()
}

/// f32 乘积 `block · weightsᵀ` 的实现：(block, 图片数, weights, 头数, 维数)。
type Product = fn(&[f32], usize, &[f32], usize, usize) -> Vec<f32>;

/// 一个包的全部头，打分前算好每个头的权重范数和粗筛下界。
pub(super) struct Heads<'a> {
    dim: usize,
    weights: &'a [f32],
    bias: &'a [f32],
    thresholds: Vec<f64>,
    norms: Vec<f64>,
    cuts: Vec<f64>,
}

/// 每块的行数：输出矩阵（行数 × 头数）不超过 2²⁰ 个 f32（4 MiB），最多 256 行。
fn block_rows(heads: usize) -> usize {
    ((1 << 20) / heads.max(1)).clamp(16, 256)
}

impl<'a> Heads<'a> {
    /// `weights` 是头数 × `dim` 的行主序矩阵，`bias`、`thresholds` 每头一个。
    pub(super) fn new(dim: usize, weights: &'a [f32], bias: &'a [f32], thresholds: &[f64]) -> Self {
        debug_assert!(dim > 0 && weights.len() == bias.len() * dim && thresholds.len() == bias.len());
        Self {
            dim,
            weights,
            bias,
            thresholds: thresholds.to_vec(),
            norms: weights.chunks_exact(dim).map(norm).collect(),
            cuts: thresholds.iter().map(|&t| cut(t)).collect(),
        }
    }

    /// 每块攒多少张图再打分。
    pub(super) fn block_rows(&self) -> usize {
        block_rows(self.bias.len())
    }

    /// 给一块图片打分：`block` 是图片数 × `dim` 的有限 f32 向量。按图片、再按头的顺序对每个命中调用
    /// `hit(图片序号, 头序号, 概率)`，顺序与逐图逐头的标量实现相同。
    pub(super) fn score(&self, block: &[f32], hit: impl FnMut(usize, usize, f64)) {
        self.score_with(block, product, hit);
    }

    fn score_with(&self, block: &[f32], product: Product, mut hit: impl FnMut(usize, usize, f64)) {
        let (dim, heads) = (self.dim, self.bias.len());
        let images = block.len() / dim;
        if images == 0 || heads == 0 {
            return;
        }
        let gamma = dim as f64 * 2f64.powi(-24) / (1.0 - dim as f64 * 2f64.powi(-24));
        let coarse = product(block, images, self.weights, heads, dim);
        for (image, (vector, row)) in block.chunks_exact(dim).zip(coarse.chunks_exact(heads)).enumerate() {
            let x = norm(vector);
            for (head, &g) in row.iter().enumerate() {
                let (b, w) = (f64::from(self.bias[head]), self.norms[head]);
                let g = f64::from(g);
                let spread = 2.0 * gamma * x * w;
                let delta =
                    spread + 2f64.powi(-100) * dim as f64 * (1.0 + x + w) + 1e-12 * (g.abs() + b.abs() + spread + 1.0);
                if g + b + delta < self.cuts[head] {
                    continue;
                }
                let weights = &self.weights[head * dim..(head + 1) * dim];
                let p = probability(vector, weights, self.bias[head]);
                if f64::from(p) >= self.thresholds[head] {
                    hit(image, head, f64::from(p));
                }
            }
        }
    }
}

/// f32 的 `block · weightsᵀ`。macOS 和 Windows 用平台的 BLAS（Windows 要能加载固定的 OpenBLAS，基础安装版没有时
/// 用可移植循环）；误差界与求和顺序无关，换后端不改变结果。
fn product(block: &[f32], images: usize, weights: &[f32], heads: usize, dim: usize) -> Vec<f32> {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    if let Ok(out) = illustash_platform::platform_research_math::cross(block, images, weights, heads, dim) {
        return out;
    }
    portable(block, images, weights, heads, dim)
}

/// 可移植的 f32 乘积：每个点积用 8 路独立累加，编译器能向量化。
fn portable(block: &[f32], images: usize, weights: &[f32], heads: usize, dim: usize) -> Vec<f32> {
    let mut out = Vec::with_capacity(images * heads);
    for x in block.chunks_exact(dim) {
        for w in weights.chunks_exact(dim) {
            let mut lanes = [0f32; 8];
            let (xs, ws) = (x.chunks_exact(8), w.chunks_exact(8));
            let tail = xs.remainder().iter().zip(ws.remainder()).map(|(a, b)| a * b).sum::<f32>();
            for (a, b) in xs.zip(ws) {
                for lane in 0..8 {
                    lanes[lane] += a[lane] * b[lane];
                }
            }
            out.push(lanes.iter().sum::<f32>() + tail);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIM: usize = 1024;

    /// 测试用的确定性伪随机数（xorshift），不引入依赖。
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        /// [-1, 1) 的均匀分布。
        fn unit(&mut self) -> f32 {
            (self.next() >> 40) as f32 / (1u64 << 23) as f32 - 1.0
        }
    }

    /// 逐图逐头的标量实现，即改动前的 `Package::hits`。
    fn reference(
        dim: usize,
        block: &[f32],
        weights: &[f32],
        bias: &[f32],
        thresholds: &[f64],
    ) -> Vec<(usize, usize, u64)> {
        let mut out = Vec::new();
        for (image, vector) in block.chunks_exact(dim).enumerate() {
            for (head, (w, &b)) in weights.chunks_exact(dim).zip(bias).enumerate() {
                let p = probability(vector, w, b);
                if f64::from(p) >= thresholds[head] {
                    out.push((image, head, f64::from(p).to_bits()));
                }
            }
        }
        out
    }

    fn batched(
        dim: usize,
        block: &[f32],
        weights: &[f32],
        bias: &[f32],
        thresholds: &[f64],
        product: Product,
    ) -> Vec<(usize, usize, u64)> {
        let heads = Heads::new(dim, weights, bias, thresholds);
        let mut out = Vec::new();
        heads.score_with(block, product, |image, head, p| out.push((image, head, p.to_bits())));
        out
    }

    fn assert_same(dim: usize, block: &[f32], weights: &[f32], bias: &[f32], thresholds: &[f64]) -> usize {
        let expected = reference(dim, block, weights, bias, thresholds);
        assert_eq!(batched(dim, block, weights, bias, thresholds, product), expected);
        assert_eq!(batched(dim, block, weights, bias, thresholds, portable), expected);
        expected.len()
    }

    /// 偏置取成让一部分头的 z 落在阈值对应的 logit 附近，正负各偏几个 f32 ulp 到 10⁻³，专门压粗筛的边界。
    fn edge_bias(dim: usize, block: &[f32], weights: &[f32], threshold: f64, rng: &mut Rng) -> Vec<f32> {
        let logit = (threshold / (1.0 - threshold)).ln();
        let first = &block[..dim];
        weights
            .chunks_exact(dim)
            .map(|w| {
                let s: f64 = first.iter().zip(w).map(|(&x, &w)| f64::from(x) * f64::from(w)).sum();
                let offset = match rng.next() % 4 {
                    0 => 0.0,
                    1 => f64::from(rng.unit()) * 1e-6,
                    2 => f64::from(rng.unit()) * 1e-3,
                    _ => f64::from(rng.unit()) * 3.0,
                };
                (logit - s + offset) as f32
            })
            .collect()
    }

    #[test]
    fn batched_scores_match_scalar_hits_near_the_threshold() {
        let mut rng = Rng(0x05ee_d582);
        let (images, heads) = (37, 53);
        // 首行之外的图是首行的小扰动，它们的 z 也靠近阈值。
        let first: Vec<f32> = (0..DIM).map(|_| rng.unit() * 2.0).collect();
        let mut block = first.clone();
        for _ in 1..images {
            let scale = [0.0, 1e-7, 1e-4, 1e-2][(rng.next() % 4) as usize];
            block.extend(first.iter().map(|&v| v + rng.unit() * scale));
        }
        let weights: Vec<f32> = (0..heads * DIM).map(|_| rng.unit() * 0.5).collect();
        let mut hits = 0;
        for threshold in [0.8008725047111511, 0.9794393181800842, 0.5, 0.99, 1e-6, 1.0 - 1e-7] {
            let bias = edge_bias(DIM, &block, &weights, threshold, &mut rng);
            hits += assert_same(DIM, &block, &weights, &bias, &vec![threshold; heads]);
        }
        assert!(hits > 0);
    }

    #[test]
    fn batched_scores_match_scalar_hits_on_extreme_values() {
        let mut rng = Rng(0xfeed_0582);
        let (images, heads) = (9, 11);
        let scales = [1e-40_f32, 1e-20, 1.0, 1e10, 1e19];
        let mut block = Vec::new();
        for image in 0..images {
            let scale = scales[image % scales.len()];
            block.extend((0..DIM).map(|_| rng.unit() * scale));
        }
        // 全零、次正规、溢出 f32 的乘积，以及非有限权重。
        block[..DIM].fill(0.0);
        let mut weights: Vec<f32> = (0..heads * DIM).map(|_| rng.unit()).collect();
        weights[DIM..2 * DIM].fill(f32::MIN_POSITIVE / 4.0);
        weights[2 * DIM..3 * DIM].iter_mut().for_each(|w| *w *= 1e30);
        weights[3 * DIM + 5] = f32::NAN;
        weights[4 * DIM + 7] = f32::INFINITY;
        weights[5 * DIM..6 * DIM].fill(0.0);
        let bias: Vec<f32> = (0..heads).map(|h| [0.0, 1.386_294_4, -50.0, 1e30, f32::MAX][h % 5]).collect();
        for thresholds in [
            vec![0.8008725047111511; heads],
            vec![0.0; heads],
            vec![-1.0; heads],
            vec![1.0; heads],
            vec![1.5; heads],
            vec![f64::NAN; heads],
            vec![1e-300; heads],
            (0..heads).map(|h| [0.2, 0.5, 0.99, 1.0, 0.0][h % 5]).collect(),
        ] {
            assert_same(DIM, &block, &weights, &bias, &thresholds);
        }
    }

    #[test]
    fn short_dimensions_and_empty_blocks() {
        let mut rng = Rng(7);
        for dim in [1, 3, 8, 13] {
            let block: Vec<f32> = (0..5 * dim).map(|_| rng.unit() * 4.0).collect();
            let weights: Vec<f32> = (0..4 * dim).map(|_| rng.unit()).collect();
            let bias = edge_bias(dim, &block, &weights, 0.8, &mut rng);
            assert_same(dim, &block, &weights, &bias, &[0.8; 4]);
            assert_same(dim, &block[..dim], &weights[..dim], &bias[..1], &[0.8]);
        }
        assert!(batched(DIM, &[], &[0.0; DIM], &[0.0], &[0.5], product).is_empty());
        assert!(batched(DIM, &[0.0; DIM], &[], &[], &[], product).is_empty());
    }

    #[test]
    fn cuts_are_below_every_hit() {
        for threshold in [1e-300, 1e-9, 0.1, 0.5, 0.8008725047111511, 0.97943931818, 0.999_999, 1.0] {
            let c = cut(threshold);
            assert!(c.is_finite() && decided(c) < threshold, "{threshold}");
            // 下界离真正的 logit 不远，粗筛才有用：差距不超过 f32 概率的一个间隔对应的 logit 变化。
            let logit = (threshold / (1.0 - threshold)).ln();
            let step = 2f64.powi(-24) / (threshold * (1.0 - threshold));
            assert!(
                threshold == 1.0 || threshold < 1e-200 || (logit - c).abs() < step + 1e-6,
                "{threshold} {c} {logit}"
            );
        }
        assert_eq!(cut(0.0), f64::NEG_INFINITY);
        assert_eq!(cut(-1.0), f64::NEG_INFINITY);
        assert_eq!(cut(1.0 + 1e-12), f64::INFINITY);
        assert_eq!(cut(f64::NAN), f64::INFINITY);
        assert_eq!(block_rows(228), 256);
        assert_eq!(block_rows(4013), 256);
        assert_eq!(block_rows(8192), 128);
        assert_eq!(block_rows(1 << 20), 16);
    }
}
