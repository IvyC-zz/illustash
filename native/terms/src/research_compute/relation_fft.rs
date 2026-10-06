//! NumPy 2.5.3 NCC: forward double loops cast complex64 after each axis,
//! complex64 product and float32 inverse loops; exact original shape/order.
use crate::{Error, Result};
unsafe extern "C" {
    fn illustash_relation_rfft2(input: *const f32, ih: usize, iw: usize, h: usize, w: usize, output: *mut f32) -> i32;
    fn illustash_relation_irfft2(input: *const f32, h: usize, w: usize, output: *mut f32) -> i32;
}

pub(super) fn correlation(
    big: &[f32],
    (bh, bw): (usize, usize),
    template: &[f32],
    (th, tw): (usize, usize),
) -> Result<Vec<f32>> {
    if th == 0
        || tw == 0
        || th > bh
        || tw > bw
        || bh.checked_mul(bw) != Some(big.len())
        || th.checked_mul(tw) != Some(template.len())
    {
        return Err(Error::Invalid("invalid relation FFT shape".into()));
    }
    let overflow = || Error::Invalid("relation FFT size overflow".into());
    let (h, w) = (bh.checked_add(th).ok_or_else(overflow)?, bw.checked_add(tw).ok_or_else(overflow)?);
    let real_count = h.checked_mul(w).ok_or_else(overflow)?;
    let n = h.checked_mul(w / 2 + 1).and_then(|n| n.checked_mul(2)).ok_or_else(overflow)?;
    if n > isize::MAX as usize / std::mem::size_of::<f32>()
        || real_count > isize::MAX as usize / std::mem::size_of::<f32>()
    {
        return Err(overflow());
    }
    let zeroed = |n| -> Result<Vec<f32>> {
        let mut data = Vec::new();
        data.try_reserve_exact(n).map_err(|_| Error::Invalid("relation FFT allocation failed".into()))?;
        data.resize(n, 0.);
        Ok(data)
    };
    let mut a = zeroed(n)?;
    let mut b = zeroed(n)?;
    // Buffers are contiguous float32 pairs, exactly the wrapper's complex64 ABI.
    let status = unsafe {
        illustash_relation_rfft2(big.as_ptr(), bh, bw, h, w, a.as_mut_ptr())
            | illustash_relation_rfft2(template.as_ptr(), th, tw, h, w, b.as_mut_ptr())
    };
    if status != 0 {
        return Err(Error::Invalid("relation FFT forward failed".into()));
    }
    multiply_conjugate(&mut a, &b);
    let mut out = zeroed(real_count)?;
    if unsafe { illustash_relation_irfft2(a.as_ptr(), h, w, out.as_mut_ptr()) } != 0 {
        return Err(Error::Invalid("relation FFT inverse failed".into()));
    }
    Ok((0..=bh - th).flat_map(|y| out[y * w..y * w + bw - tw + 1].iter().copied()).collect())
}
fn multiply_conjugate(a: &mut [f32], b: &[f32]) {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    let fused = true;
    #[cfg(all(windows, target_arch = "x86_64"))]
    let fused = std::is_x86_feature_detected!("avx2") && std::is_x86_feature_detected!("fma");
    #[cfg(not(any(all(target_os = "macos", target_arch = "aarch64"), all(windows, target_arch = "x86_64"))))]
    let fused = false;
    for (a, b) in a.chunks_exact_mut(2).zip(b.chunks_exact(2)) {
        let (ar, ai, br, bi) = (a[0], a[1], b[0], -b[1]);
        // NumPy 2.5.3 simd_cmul_f32 rounds a_im*b_im/re before muladdsub.
        // NEON and Windows AVX2/FMA dispatch fuse the a_re product; baseline
        // x86 keeps separate operations. Do not fuse both products together.
        if fused {
            a[0] = ar.mul_add(br, -(ai * bi));
            a[1] = ar.mul_add(bi, ai * br);
        } else {
            a[0] = ar * br - ai * bi;
            a[1] = ar * bi + ai * br;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(any(all(target_os = "macos", target_arch = "aarch64"), all(windows, target_arch = "x86_64")))]
    use sha2::{Digest, Sha256};
    #[cfg(any(all(target_os = "macos", target_arch = "aarch64"), all(windows, target_arch = "x86_64")))]
    fn exact_hash(data: &[f32], expected: &str, stage: &str) {
        let mut hash = Sha256::new();
        for value in data {
            hash.update(value.to_le_bytes());
        }
        assert_eq!(format!("{:x}", hash.finalize()), expected, "{stage}");
    }
    #[test]
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    fn numpy_253_forward_product_inverse_stages_exact_bits() {
        let a: Vec<f32> = (0..20).map(|i| (i % 7) as f32 - 3.).collect();
        let b: Vec<f32> = (0..4).map(|i| (i % 3) as f32 - 1.).collect();
        let mut fa = vec![0.; 48];
        let mut fb = vec![0.; 48];
        assert_eq!(unsafe { illustash_relation_rfft2(a.as_ptr(), 4, 5, 6, 7, fa.as_mut_ptr()) }, 0);
        assert_eq!(unsafe { illustash_relation_rfft2(b.as_ptr(), 2, 2, 6, 7, fb.as_mut_ptr()) }, 0);
        exact_hash(&fa, "40fc0a62c6c9e5551e9a4ffeb5ed59aa84440c747975912f0a3bb15528be35f7", "forward-a");
        exact_hash(&fb, "e15e2d677626315a3e12c52d4564e5e8a51ac476d0f465b2ff9ab44bdcd81ed9", "forward-b");
        multiply_conjugate(&mut fa, &fb);
        exact_hash(&fa, "872f9f8053bca1f14729e22ce7f133e645ca09d4968b9996e125e1d335777a58", "product");
        let mut out = vec![0.; 42];
        assert_eq!(unsafe { illustash_relation_irfft2(fa.as_ptr(), 6, 7, out.as_mut_ptr()) }, 0);
        exact_hash(&out, "7e01b5c8eb703ab80e76bd24143c30312e97836ec77cfdb5fa1fe7121d6d337a", "inverse");
    }
    // Independent existing web.relate FFT expression, NumPy 2.5.3 / macOS arm64.
    // These expected hashes were generated before running the candidate. All
    // float32 bits (including signs of zero) are checked; no tolerance/golden update.
    #[test]
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    fn numpy_253_float32_prime_composite_and_small_outer_exact_bits() {
        for ((h, w, th, tw), expected) in [
            ((4, 5, 2, 2), "7f849533606124105d046db368481a26525995a960ec19003a280a379b82ace5"),
            ((6, 7, 3, 4), "778bbe0c86a66c6097ea201ce0f9115f52d3c7a8829704bad747bbdb0c6b64c1"),
            ((9, 8, 4, 5), "6e535aef90ac2d0c33dd75163fbaa2c70e8afff8409946f008f169c04d9d2853"),
            ((12, 13, 5, 6), "7169be1182df17bc79a3445d20716ac9c4268f38d1779afe6c74933ce72f5925"),
            ((3, 3, 2, 2), "0082fd1e069a3a1b51fb6b44162aa2a50b601b306f72a6194b55620000f49a6e"),
            ((31, 37, 11, 13), "01efdf386f7cb6477d132bb005bb9460538d7fd13a639c4d427eed630a60cce7"),
        ] {
            let a: Vec<f32> = (0..h * w).map(|i| (i % 7) as f32 - 3.).collect();
            let b: Vec<f32> = (0..th * tw).map(|i| (i % 3) as f32 - 1.).collect();
            let got = correlation(&a, (h, w), &b, (th, tw)).unwrap();
            exact_hash(&got, expected, &format!("{h}x{w}/{th}x{tw}"));
        }
    }
    // Fixed independent Windows NumPy 2.5.3 references, captured before the
    // candidate fix. Baseline references disabled X86_V3/X86_V4 as well as
    // AVX2/FMA3/AVX512F; disabling the legacy feature names alone is insufficient.
    #[test]
    #[cfg(all(windows, target_arch = "x86_64"))]
    fn windows_numpy_253_forward_product_inverse_stages_exact_bits() {
        let a: Vec<f32> = (0..20).map(|i| (i % 7) as f32 - 3.).collect();
        let b: Vec<f32> = (0..4).map(|i| (i % 3) as f32 - 1.).collect();
        let mut fa = vec![0.; 48];
        let mut fb = vec![0.; 48];
        assert_eq!(unsafe { illustash_relation_rfft2(a.as_ptr(), 4, 5, 6, 7, fa.as_mut_ptr()) }, 0);
        assert_eq!(unsafe { illustash_relation_rfft2(b.as_ptr(), 2, 2, 6, 7, fb.as_mut_ptr()) }, 0);
        exact_hash(&fa, "40fc0a62c6c9e5551e9a4ffeb5ed59aa84440c747975912f0a3bb15528be35f7", "forward-a");
        exact_hash(&fb, "e15e2d677626315a3e12c52d4564e5e8a51ac476d0f465b2ff9ab44bdcd81ed9", "forward-b");
        multiply_conjugate(&mut fa, &fb);
        let fused = std::is_x86_feature_detected!("avx2") && std::is_x86_feature_detected!("fma");
        let (product, inverse) = if fused {
            (
                "872f9f8053bca1f14729e22ce7f133e645ca09d4968b9996e125e1d335777a58",
                "39b41d8972ac9143cecf32a7147b0b5f8caab743ad0f87166c627b1c816ec113",
            )
        } else {
            (
                "fd4358fb81bff354a528f0d6775e2d05a9a430ed6dce650b32c1c75385e18069",
                "849deda37e55f177f1dd561d9c70999550f810333e8aa16ad215f806dd090002",
            )
        };
        exact_hash(&fa, product, "product");
        let mut out = vec![0.; 42];
        assert_eq!(unsafe { illustash_relation_irfft2(fa.as_ptr(), 6, 7, out.as_mut_ptr()) }, 0);
        exact_hash(&out, inverse, "inverse");
    }
    #[test]
    #[cfg(all(windows, target_arch = "x86_64"))]
    fn windows_numpy_253_prime_composite_valid_correlation_exact_bits() {
        let fused = std::is_x86_feature_detected!("avx2") && std::is_x86_feature_detected!("fma");
        for ((h, w, th, tw), fma, baseline) in [
            (
                (4, 5, 2, 2),
                "7bbaab625ad91135e25d49b9eb9fc805306a8196dba5f727e9720a757436a7bc",
                "6c8d7845de6e3f05a23be2bb7917247ec6c23f4a708e1510ee4df68dd2b9f848",
            ),
            (
                (6, 7, 3, 4),
                "ad4715574222b3805d64b01673a970e94781af847aa75f244f3b3ab883c5413f",
                "b9535d3137f25ad451524483664f28328ab5ebef7ddfbf4fb09603cdcd693f96",
            ),
            (
                (9, 8, 4, 5),
                "261e7cce8910749fb091e011afd35601d4ae6584df1e7b1e02a3ae9b8e7a1df4",
                "46d2aedd30b6c6e1fac66178de990b905c6166774a99d01da7241a7d34ce7c0d",
            ),
            (
                (12, 13, 5, 6),
                "43a03ca701bad7b4acc8851be8ce4836048178fe800eb32d9b890d0fe7163dd8",
                "88fdfb8caec8a355dc065472f7d1ec7b3c059b6373170b3b3d62438d4d01c2e9",
            ),
            (
                (3, 3, 2, 2),
                "6b8fdac04dbcc5f03a114c4f5661236088744d85b661fc917884ce4f332d70de",
                "be2e3e6d673469befc863fea7e7b9ab99ffd7f476cae913f33c48181df741783",
            ),
            (
                (31, 37, 11, 13),
                "f9ac0bf2e0aabfe2a26e474955e2c4afbc5d629cf6368b312e0182350153c309",
                "2773393aa1f953d347f7ed6e630a9be65ff1e7662293ae7f5ad8d9e1df24c576",
            ),
        ] {
            let a: Vec<f32> = (0..h * w).map(|i| (i % 7) as f32 - 3.).collect();
            let b: Vec<f32> = (0..th * tw).map(|i| (i % 3) as f32 - 1.).collect();
            let got = correlation(&a, (h, w), &b, (th, tw)).unwrap();
            exact_hash(&got, if fused { fma } else { baseline }, &format!("{h}x{w}/{th}x{tw}"));
        }
    }
    #[test]
    fn rejects_invalid_shapes_before_ffi() {
        assert!(correlation(&[1.], (1, 1), &[1.; 4], (2, 2)).is_err());
        assert!(correlation(&[1.], (1, 1), &[], (0, 0)).is_err());
        assert!(correlation(&[], (usize::MAX, 2), &[1.], (1, 1)).is_err());
    }
}
