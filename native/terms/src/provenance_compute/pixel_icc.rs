//! 像素哈希的色彩转换：固定 LittleCMS 2.19，沿用旧 Pillow 的 RGB 变换。
use crate::{Error, Result};
use image::RgbaImage;
use lcms2::{Intent, PixelFormat, Profile, Transform};

/// 带色彩配置文件的像素哈希之前，必须在最终链接的可执行文件里通过这项检查。
/// 运行库不对是构建或资源错误，绝不按配置文件无效回退。
pub(crate) fn check_version() -> Result<()> {
    let version = lcms2::version();
    if version != 2190 {
        return Err(Error::Invalid(format!("像素哈希要求 LittleCMS 2.19，实际版本 {version}")));
    }
    Ok(())
}

pub(super) fn srgb(image: &mut RgbaImage, profile_bytes: &[u8]) -> Result<()> {
    if profile_bytes.is_empty() {
        return Ok(());
    }
    check_version()?;
    // Pillow 在转 RGB 之前取出 ICC，再打开那份原始配置文件。
    // ICC 无效或不兼容时，保留已经做过模式转换的原始像素。
    let Ok(input) = Profile::new_icc(profile_bytes) else {
        return Ok(());
    };
    let output = Profile::new_srgb();
    // Pillow 的 RGB 内部是四字节像素，以 TYPE_RGBA_8 交给 lcms。
    // 第四个字节不是源图的 alpha，源 alpha 在转换后再接回。
    let Ok(transform) = Transform::<[u8; 4], [u8; 4]>::new(
        &input,
        PixelFormat::RGBA_8,
        &output,
        PixelFormat::RGBA_8,
        Intent::Perceptual,
    ) else {
        return Ok(());
    };
    let width = image.width() as usize;
    if width == 0 {
        return Err(Error::Invalid("ICC 图片宽度无效".into()));
    }
    let mut input_row = vec![[0; 4]; width];
    let mut output_row = vec![[0; 4]; width];
    for row in image.as_mut().chunks_exact_mut(width * 4) {
        for (source, pixel) in row.chunks_exact(4).zip(&mut input_row) {
            *pixel = [source[0], source[1], source[2], 255];
        }
        // 与 Pillow 相同：逐扫描行调用，默认 flags=0、感知意图（perceptual）。
        transform.transform_pixels(&input_row, &mut output_row);
        for (destination, pixel) in row.chunks_exact_mut(4).zip(&output_row) {
            destination[..3].copy_from_slice(&pixel[..3]);
        }
    }
    Ok(())
}
