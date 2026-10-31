//! 现有研究矩阵计算器需要的 CPU float32 乘积。
//! macOS 用系统 BLAS；Windows 原生研究需要固定的 ILP64 OpenBLAS DLL。
//! 其他平台保留未经验收的可移植循环（Linux 已冻结）。
#[cfg(target_os = "windows")]
mod windows_openblas;

/// 固定的 Windows 后端的身份（从不搜索 PATH）。
#[cfg(target_os = "windows")]
pub fn windows_backend_identity() -> Result<String, String> {
    windows_openblas::identity()
}

/// C 顺序行矩阵 A 与 B 转置的乘积。保留原有的形状分派，包括向量和对称的情况：
/// 换成逐个独立的点积可能改变阈值判定。
pub fn cross(a: &[f32], m: usize, b: &[f32], p: usize, dim: usize) -> Result<Vec<f32>, String> {
    if m.checked_mul(dim) != Some(a.len())
        || p.checked_mul(dim) != Some(b.len())
        || [m, p, dim].iter().any(|&v| v > i32::MAX as usize)
    {
        return Err("invalid research matrix shape".into());
    }
    let count = m.checked_mul(p).ok_or("research matrix size overflow")?;
    let mut out = Vec::new();
    out.try_reserve_exact(count).map_err(|_| "research matrix allocation failed")?;
    out.resize(count, 0.0);
    if m == 0 || p == 0 || dim == 0 {
        return Ok(out);
    }
    #[cfg(target_os = "macos")]
    // SAFETY: 上面已检查长度、各维非空和 i32 上限；
    // BLAS 只拿到调用方持有的只读切片和大小已定的可写输出。
    unsafe {
        if m == 1 && p == 1 {
            out[0] = blas::cblas_sdot(dim as i32, a.as_ptr(), 1, b.as_ptr(), 1);
        } else if dim == 1 {
            for i in 0..m {
                for j in 0..p {
                    out[i * p + j] = a[i] * b[j];
                }
            }
        } else if p == 1 {
            blas::cblas_sgemv(
                102,
                112,
                dim as i32,
                m as i32,
                1.0,
                a.as_ptr(),
                dim as i32,
                b.as_ptr(),
                1,
                0.0,
                out.as_mut_ptr(),
                1,
            );
        } else if m == 1 {
            blas::cblas_sgemv(
                102,
                112,
                dim as i32,
                p as i32,
                1.0,
                b.as_ptr(),
                dim as i32,
                a.as_ptr(),
                1,
                0.0,
                out.as_mut_ptr(),
                1,
            );
        } else if m == p && a.as_ptr() == b.as_ptr() {
            blas::cblas_ssyrk(
                101,
                121,
                111,
                m as i32,
                dim as i32,
                1.0,
                a.as_ptr(),
                dim as i32,
                0.0,
                out.as_mut_ptr(),
                p as i32,
            );
            for i in 0..m {
                for j in i + 1..p {
                    out[j * p + i] = out[i * p + j];
                }
            }
        } else {
            blas::cblas_sgemm(
                101,
                111,
                112,
                m as i32,
                p as i32,
                dim as i32,
                1.0,
                a.as_ptr(),
                dim as i32,
                b.as_ptr(),
                dim as i32,
                0.0,
                out.as_mut_ptr(),
                p as i32,
            );
        }
    }
    #[cfg(target_os = "windows")]
    windows_openblas::cross(a, m, b, p, dim, &mut out)?;
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    for i in 0..m {
        for j in 0..p {
            out[i * p + j] =
                a[i * dim..(i + 1) * dim].iter().zip(&b[j * dim..(j + 1) * dim]).fold(0.0, |s, (a, b)| s + a * b);
        }
    }
    Ok(out)
}
/// Windows 上的 float64 C 顺序 A @ B.T，形状分派与 NumPy 相同。
/// 固定后端计算加权 trait 画像时使用。
#[cfg(target_os = "windows")]
pub fn cross64(a: &[f64], m: usize, b: &[f64], p: usize, dim: usize) -> Result<Vec<f64>, String> {
    if m.checked_mul(dim) != Some(a.len())
        || p.checked_mul(dim) != Some(b.len())
        || [m, p, dim].iter().any(|&v| v > i32::MAX as usize)
    {
        return Err("invalid research float64 matrix shape".into());
    }
    let count = m.checked_mul(p).ok_or("research matrix size overflow")?;
    let mut out = Vec::new();
    out.try_reserve_exact(count).map_err(|_| "research matrix allocation failed")?;
    out.resize(count, 0.0);
    if m != 0 && p != 0 && dim != 0 {
        windows_openblas::cross64(a, m, b, p, dim, &mut out)?;
    }
    Ok(out)
}
/// np.linalg.norm 用的一维 float32 点积；按行的特征归一化改用计算器的连续归约，与这里无关。
pub fn dot(a: &[f32], b: &[f32]) -> Result<f32, String> {
    if a.len() != b.len() || a.len() > i32::MAX as usize {
        return Err("invalid research dot shape".into());
    }
    if a.is_empty() {
        return Ok(0.0);
    }
    #[cfg(target_os = "macos")]
    // SAFETY: 已检查两个切片等长，步长为 1，存储有效且非空。
    unsafe {
        Ok(blas::cblas_sdot(a.len() as i32, a.as_ptr(), 1, b.as_ptr(), 1))
    }
    #[cfg(target_os = "windows")]
    {
        windows_openblas::dot(a, b)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    Ok(a.iter().zip(b).fold(0.0, |s, (a, b)| s + a * b))
}

#[cfg(target_os = "macos")]
mod blas {
    #[link(name = "Accelerate", kind = "framework")]
    extern "C" {
        pub fn cblas_sdot(n: i32, a: *const f32, inc_a: i32, b: *const f32, inc_b: i32) -> f32;
        pub fn cblas_sgemv(
            order: i32,
            transpose: i32,
            m: i32,
            n: i32,
            alpha: f32,
            a: *const f32,
            lda: i32,
            b: *const f32,
            inc_b: i32,
            beta: f32,
            out: *mut f32,
            inc_out: i32,
        );
        pub fn cblas_sgemm(
            order: i32,
            ta: i32,
            tb: i32,
            m: i32,
            n: i32,
            k: i32,
            alpha: f32,
            a: *const f32,
            lda: i32,
            b: *const f32,
            ldb: i32,
            beta: f32,
            out: *mut f32,
            ldc: i32,
        );
        pub fn cblas_ssyrk(
            order: i32,
            uplo: i32,
            transpose: i32,
            n: i32,
            k: i32,
            alpha: f32,
            a: *const f32,
            lda: i32,
            beta: f32,
            out: *mut f32,
            ldc: i32,
        );
    }
}
/// 建索引时 A @ anchor 沿用原 NPY 的 dtype 和内存布局；保持 GEMV 的形状。
pub fn index_product32(
    rows: &[f32],
    n: usize,
    dimension: usize,
    fortran: bool,
    anchor: &[f32],
) -> Result<Vec<f32>, String> {
    if !fortran {
        return cross(rows, n, anchor, 1, dimension);
    }
    if n.checked_mul(dimension) != Some(rows.len())
        || dimension != anchor.len()
        || n > i32::MAX as usize
        || dimension > i32::MAX as usize
    {
        return Err("invalid index matrix shape".into());
    }
    let mut out = vec![0.; n];
    if n == 0 || dimension == 0 {
        return Ok(out);
    }
    #[cfg(target_os = "macos")]
    // SAFETY: 上面已检查 `rows.len() == n * dimension`、`anchor.len() == dimension`、两者非零且不超过 i32；列主序 lda
    // = n，步长为 1，`out` 有 n 个元素。
    unsafe {
        if n == 1 {
            out[0] = blas::cblas_sdot(dimension as i32, rows.as_ptr(), 1, anchor.as_ptr(), 1);
        } else {
            blas::cblas_sgemv(
                102,
                111,
                n as i32,
                dimension as i32,
                1.,
                rows.as_ptr(),
                n as i32,
                anchor.as_ptr(),
                1,
                0.,
                out.as_mut_ptr(),
                1,
            );
        }
    }
    #[cfg(target_os = "windows")]
    windows_openblas::index_product32(rows, n, dimension, anchor, &mut out)?;
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    for i in 0..n {
        for j in 0..dimension {
            out[i] += rows[j * n + i] * anchor[j];
        }
    }
    Ok(out)
}
pub fn index_dot64(a: &[f64], b: &[f64]) -> Result<f64, String> {
    if a.len() != b.len() || a.len() > i32::MAX as usize {
        return Err("invalid index dot shape".into());
    }
    if a.is_empty() {
        return Ok(0.);
    }
    #[cfg(target_os = "macos")]
    // SAFETY: 上面已检查两个切片等长、非空且长度不超过 i32，步长为 1。
    unsafe {
        Ok(index_blas::cblas_ddot(a.len() as i32, a.as_ptr(), 1, b.as_ptr(), 1))
    }
    #[cfg(target_os = "windows")]
    {
        let mut out = [0.];
        windows_openblas::cross64(a, 1, b, 1, a.len(), &mut out)?;
        Ok(out[0])
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        Ok(a.iter().zip(b).fold(0., |s, (a, b)| s + a * b))
    }
}
pub fn index_product64(
    rows: &[f64],
    n: usize,
    dimension: usize,
    fortran: bool,
    anchor: &[f64],
) -> Result<Vec<f64>, String> {
    if n.checked_mul(dimension) != Some(rows.len())
        || dimension != anchor.len()
        || n > i32::MAX as usize
        || dimension > i32::MAX as usize
    {
        return Err("invalid index matrix shape".into());
    }
    let mut out = vec![0.; n];
    if n == 0 || dimension == 0 {
        return Ok(out);
    }
    #[cfg(target_os = "macos")]
    // SAFETY: 上面已检查 `rows.len() == n * dimension`、`anchor.len() == dimension`、两者非零且不超过 i32；
    // 按行主序或列主序选定的 lda 与形状一致，`out` 有 n 个元素。
    unsafe {
        if n == 1 {
            out[0] = index_blas::cblas_ddot(dimension as i32, rows.as_ptr(), 1, anchor.as_ptr(), 1);
        } else {
            let (trans, m, k, lda) = if fortran { (111, n, dimension, n) } else { (112, dimension, n, dimension) };
            index_blas::cblas_dgemv(
                102,
                trans,
                m as i32,
                k as i32,
                1.,
                rows.as_ptr(),
                lda as i32,
                anchor.as_ptr(),
                1,
                0.,
                out.as_mut_ptr(),
                1,
            );
        }
    }
    #[cfg(target_os = "windows")]
    windows_openblas::index_product64(rows, n, dimension, fortran, anchor, &mut out)?;
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    for i in 0..n {
        for j in 0..dimension {
            out[i] += rows[if fortran { j * n + i } else { i * dimension + j }] * anchor[j];
        }
    }
    Ok(out)
}
#[cfg(target_os = "macos")]
mod index_blas {
    #[link(name = "Accelerate", kind = "framework")]
    extern "C" {
        pub fn cblas_ddot(n: i32, a: *const f64, ia: i32, b: *const f64, ib: i32) -> f64;
        pub fn cblas_dgemv(
            order: i32,
            trans: i32,
            m: i32,
            n: i32,
            alpha: f64,
            a: *const f64,
            lda: i32,
            b: *const f64,
            ib: i32,
            beta: f64,
            out: *mut f64,
            io: i32,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn index_products_preserve_c_and_fortran_layout_and_reject_bad_shapes() {
        let c = [1., 2., 3., 4., 5., 6.];
        let f = [1., 3., 5., 2., 4., 6.];
        let anchor = [2., 3.];
        for layout in [false, true] {
            let rows = if layout { &f } else { &c };
            assert_eq!(index_product64(rows, 3, 2, layout, &anchor).unwrap(), vec![8., 18., 28.]);
            let rows: Vec<f32> = rows.iter().map(|x| *x as f32).collect();
            assert_eq!(index_product32(&rows, 3, 2, layout, &[2., 3.]).unwrap(), vec![8., 18., 28.]);
        }
        assert_eq!(index_product64(&[], 0, 2, true, &anchor).unwrap(), Vec::<f64>::new());
        assert!(index_product64(&c, 4, 2, false, &anchor).is_err());
        assert!(index_product32(&[1.], 1, 2, true, &[2., 3.]).is_err());
        assert!(index_dot64(&[1.], &[]).is_err());
    }
    #[test]
    fn shape_errors_zero_rows_and_vector_products() {
        assert!(cross(&[1.0], 1, &[1.0], 1, 2).is_err());
        assert!(dot(&[1.0], &[]).is_err());
        assert!(cross(&[], 0, &[1.0, 0.0], 1, 2).unwrap().is_empty());
        assert_eq!(cross(&[1.0, 0.0, 0.0, 1.0], 2, &[1.0, 0.0], 1, 2).unwrap(), vec![1.0, 0.0]);
        assert_eq!(cross(&[1.0, 0.0], 1, &[1.0, 0.0, 0.0, 1.0], 2, 2).unwrap(), vec![1.0, 0.0]);
        assert_eq!(dot(&[1.0, 2.0], &[3.0, 4.0]).unwrap(), 11.0);
    }
}
