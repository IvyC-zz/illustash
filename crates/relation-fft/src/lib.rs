//! 关系 NCC 的原始 FFT 接口。C++ 源码、PocketFFT header 和浮点参数保持不变，独立编译以供宿主链接。
#![no_std]

unsafe extern "C" {
    /// 把实数输入补零至 h×w 后写出连续 complex64，成功返回 0。
    ///
    /// # Safety
    /// `input` 必须可读 ih×iw 个 f32，`output` 必须可写 h×(w/2+1)×2 个 f32；尺寸非零，
    /// ih≤h、iw≤w，缓冲区不重叠，长度计算不能溢出。函数不保留指针。
    pub fn illustash_relation_rfft2(
        input: *const f32,
        ih: usize,
        iw: usize,
        h: usize,
        w: usize,
        output: *mut f32,
    ) -> i32;

    /// 把连续 complex64 频谱逆变换为 h×w 个 float32，成功返回 0。
    ///
    /// # Safety
    /// `input` 必须可读 h×(w/2+1)×2 个 f32，`output` 必须可写 h×w 个 f32；尺寸非零，
    /// 缓冲区不重叠，长度计算不能溢出。函数不保留指针。
    pub fn illustash_relation_irfft2(input: *const f32, h: usize, w: usize, output: *mut f32) -> i32;
}
