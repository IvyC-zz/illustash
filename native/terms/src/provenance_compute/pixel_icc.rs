//! Candidate-only: fixed LittleCMS 2.19, legacy Pillow RGB transforms.
use crate::{Error, Result};
use image::RgbaImage;
use lcms2::{Intent, PixelFormat, Profile, Transform};

/// Must succeed in the final linked executable before profiled pixel hashing.
/// A wrong runtime is a build/resource error, never invalid-profile fallback.
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
    // Pillow captures ICC before RGB conversion, then opens that original profile.
    // Invalid/incompatible ICC keeps already mode-converted original pixels.
    let Ok(input) = Profile::new_icc(profile_bytes) else {
        return Ok(());
    };
    let output = Profile::new_srgb();
    // Pillow RGB is an internal four-byte pixel represented to lcms as TYPE_RGBA_8.
    // Its fourth byte is not the source alpha, which is reattached after conversion.
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
        // Same per-scanline call and default flags=0/perceptual intent as Pillow.
        transform.transform_pixels(&input_row, &mut output_row);
        for (destination, pixel) in row.chunks_exact_mut(4).zip(&output_row) {
            destination[..3].copy_from_slice(&pixel[..3]);
        }
    }
    Ok(())
}
