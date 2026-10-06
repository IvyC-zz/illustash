//! Exact PAM/MD5 contract, deliberately separate from display/signature decoding.
//! Explicit LCMS2.19/scalar TIFF candidate; installed adapter remains gated.
use super::pixel_icc;
use crate::image_samples::{
    pnm::{pnm_samples, PnmSamples},
    tiff_scalar,
};
use crate::{Error, Result};
use image::{DynamicImage, ImageDecoder, ImageReader, RgbaImage};
use md5::{Digest, Md5};
use std::{fs, io::Cursor, path::Path};

/// Hash already normalised, straight (unpremultiplied) sRGB RGBA bytes.
/// Includes hidden RGB under alpha=0; EXIF, filename and compressed bytes are absent.
pub fn rgba_hash(width: u32, height: u32, pixels: &[u8]) -> Result<String> {
    let length = (width as usize).checked_mul(height as usize).and_then(|n| n.checked_mul(4));
    if width == 0 || height == 0 || length != Some(pixels.len()) {
        return Err(Error::Invalid("PAM RGBA 像素长度无效".into()));
    }
    let header = format!("P7\nWIDTH {width}\nHEIGHT {height}\nDEPTH 4\nMAXVAL 255\nTUPLTYPE RGB_ALPHA\nENDHDR\n");
    let mut md5 = Md5::new();
    md5.update(header.as_bytes());
    md5.update(pixels);
    Ok(format!("{:x}", md5.finalize()))
}

/// Candidate only: fixed LCMS2.19 plus narrow scalar TIFF; production remains gated.
/// Full decoder/ICC coverage is a release gate; this is not selected by the adapter.
pub fn candidate_path(path: &Path) -> Result<String> {
    let bytes = fs::read(path)?;
    candidate_bytes_with_path(&bytes, Some(path))
}
pub fn candidate_bytes(bytes: &[u8]) -> Result<String> {
    candidate_bytes_with_path(bytes, None)
}
fn candidate_bytes_with_path(bytes: &[u8], path: Option<&Path>) -> Result<String> {
    let mut reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    if reader.format().is_none() {
        if let Some(format) = path.and_then(|p| image::ImageFormat::from_path(p).ok()) {
            reader.set_format(format);
        }
    }
    let mut icc = None;
    let mut image = if reader.format() == Some(image::ImageFormat::Gif) {
        gif_first_frame(bytes)?
    } else if reader.format() == Some(image::ImageFormat::Pnm) {
        pnm(bytes)?
    } else if reader.format() == Some(image::ImageFormat::Tiff) {
        let (image, profile) = tiff_bytes(bytes, path.is_some())?;
        icc = profile;
        image
    } else if reader.format() == Some(image::ImageFormat::Jpeg) {
        // Full-resolution legacy RGB/CMYK decode, never recognition draft.
        let (image, profile) = std::panic::catch_unwind(|| -> Result<(RgbaImage, Option<Vec<u8>>)> {
            let d = mozjpeg::Decompress::with_markers(&[mozjpeg::Marker::APP(2)]).from_mem(bytes)?;
            let profile = jpeg_icc(
                d.markers().filter(|m| m.data.starts_with(b"ICC_PROFILE\0")).map(|m| m.data.to_vec()).collect(),
            )?;
            let cmyk = matches!(d.color_space(), mozjpeg::ColorSpace::JCS_CMYK | mozjpeg::ColorSpace::JCS_YCCK);
            let mut d = if cmyk { d.to_colorspace(mozjpeg::ColorSpace::JCS_CMYK)? } else { d.rgb()? };
            let (width, height) = (d.width() as u32, d.height() as u32);
            let raw = d.read_scanlines::<u8>()?;
            d.finish()?;
            let pixels = if cmyk {
                raw.chunks_exact(4)
                    .flat_map(|p| {
                        let rgb = [0, 1, 2].map(|c| ((u32::from(p[c]) * u32::from(p[3]) + 127) / 255) as u8);
                        [rgb[0], rgb[1], rgb[2], 255]
                    })
                    .collect()
            } else {
                raw.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect()
            };
            let image =
                RgbaImage::from_raw(width, height, pixels).ok_or_else(|| Error::Invalid("JPEG 像素长度无效".into()))?;
            Ok((image, profile))
        })
        .map_err(|_| Error::Invalid("JPEG 解码失败".into()))??;
        icc = profile;
        image
    } else {
        // ImageDecoder preserves straight alpha; no matte or EXIF transpose here.
        let mut decoder = reader.into_decoder().map_err(|e| Error::Invalid(e.to_string()))?;
        icc = decoder.icc_profile().map_err(|e| Error::Invalid(e.to_string()))?;
        match DynamicImage::from_decoder(decoder).map_err(|e| Error::Invalid(e.to_string()))? {
            DynamicImage::ImageLuma16(gray) => RgbaImage::from_fn(gray.width(), gray.height(), |x, y| {
                let v = gray.get_pixel(x, y)[0].min(255) as u8;
                image::Rgba([v, v, v, 255])
            }),
            DynamicImage::ImageLumaA16(_)
            | DynamicImage::ImageRgb16(_)
            | DynamicImage::ImageRgba16(_)
            | DynamicImage::ImageRgb32F(_)
            | DynamicImage::ImageRgba32F(_) => {
                return Err(Error::Invalid("像素哈希高位深彩色解码尚未验收".into()));
            }
            other => other.to_rgba8(),
        }
    };
    if let Some(profile) = icc {
        pixel_icc::srgb(&mut image, &profile)?;
    }
    rgba_hash(image.width(), image.height(), image.as_raw())
}

// Ordinary 8-bit RGB TIFF has no alpha or Pillow mmap mode. Keep the scalar
// consumer separate: its path-dependent reshape and clamp do not apply here.
fn tiff_bytes(bytes: &[u8], path_input: bool) -> Result<(RgbaImage, Option<Vec<u8>>)> {
    use image::imageops;
    use tiff::{
        decoder::{Decoder, DecodingResult},
        tags::Tag,
        ColorType,
    };
    let invalid = |e: tiff::TiffError| Error::Invalid(e.to_string());
    let mut decoder = Decoder::new(Cursor::new(bytes)).map_err(invalid)?;
    if decoder.colortype().map_err(invalid)? != ColorType::RGB(8) {
        return tiff_scalar::scalar_bytes(bytes, path_input);
    }
    let planar: u16 = decoder.find_tag_unsigned(Tag::PlanarConfiguration).map_err(invalid)?.unwrap_or(1);
    let fill: u16 = decoder.find_tag_unsigned(Tag::FillOrder).map_err(invalid)?.unwrap_or(1);
    let compression: u16 = decoder.find_tag_unsigned(Tag::Compression).map_err(invalid)?.unwrap_or(1);
    let predictor: u16 = decoder.find_tag_unsigned(Tag::Predictor).map_err(invalid)?.unwrap_or(1);
    if planar != 1
        || fill != 1
        || predictor != 1
        || !matches!(compression, 1 | 5 | 8 | 32773)
        || decoder.find_tag(Tag::TileOffsets).map_err(invalid)?.is_some()
    {
        return Err(Error::Invalid("TIFF RGB layout remains unaccepted".into()));
    }
    let orientation: u16 = decoder.find_tag_unsigned(Tag::Orientation).map_err(invalid)?.unwrap_or(1);
    let icc = decoder
        .find_tag(Tag::Unknown(34675))
        .map_err(invalid)?
        .map(|v| v.into_u8_vec())
        .transpose()
        .map_err(invalid)?;
    let (width, height) = decoder.dimensions().map_err(invalid)?;
    let DecodingResult::U8(raw) = decoder.read_image().map_err(invalid)? else {
        return Err(Error::Invalid("TIFF RGB sample type mismatch".into()));
    };
    let pixels = raw.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect();
    let image = RgbaImage::from_raw(width, height, pixels)
        .filter(|_| width != 0 && height != 0 && raw.len() % 3 == 0)
        .ok_or_else(|| Error::Invalid("TIFF RGB buffer length mismatch".into()))?;
    let image = match orientation {
        2 => imageops::flip_horizontal(&image),
        3 => imageops::rotate180(&image),
        4 => imageops::flip_vertical(&image),
        5 => imageops::flip_horizontal(&imageops::rotate90(&image)),
        6 => imageops::rotate90(&image),
        7 => imageops::flip_vertical(&imageops::rotate90(&image)),
        8 => imageops::rotate270(&image),
        _ => image,
    };
    Ok((image, icc))
}

/// Final-linked library guard, also exposed by the explicit candidate probe.
pub fn lcms_version() -> Result<u32> {
    pixel_icc::check_version()?;
    Ok(lcms2::version())
}
// Preserve Pillow JPEG APP2 handling: lexicographic fragment order, and the
// first sorted fragment's total compared to actual count (not a stricter parser).
fn jpeg_icc(mut fragments: Vec<Vec<u8>>) -> Result<Option<Vec<u8>>> {
    if fragments.is_empty() {
        return Ok(None);
    }
    if fragments.iter().any(|part| part.len() < 14) {
        return Err(Error::Invalid("JPEG ICC 分片被截断".into()));
    }
    fragments.sort();
    if fragments[0][13] as usize != fragments.len() {
        return Ok(None);
    }
    Ok(Some(fragments.into_iter().flat_map(|part| part[14..].to_vec()).collect()))
}

// Decode palette indices ourselves: the first Pillow canvas uses its transparent
// index when present, otherwise index 0; RGB survives even under zero alpha.
fn gif_first_frame(bytes: &[u8]) -> Result<RgbaImage> {
    let mut options = gif::DecodeOptions::new();
    options.set_color_output(gif::ColorOutput::Indexed);
    let mut decoder = options.read_info(Cursor::new(bytes)).map_err(|e| Error::Invalid(e.to_string()))?;
    let (width, height) = (u32::from(decoder.width()), u32::from(decoder.height()));
    let frame = decoder
        .read_next_frame()
        .map_err(|e| Error::Invalid(e.to_string()))?
        .ok_or_else(|| Error::Invalid("GIF 缺少首帧".into()))?
        .clone();
    let palette = frame.palette.as_deref().or_else(|| decoder.global_palette());
    let colour = |index: u8| -> Result<image::Rgba<u8>> {
        let rgb = match palette {
            Some(p) => {
                let p = p
                    .get(usize::from(index) * 3..usize::from(index) * 3 + 3)
                    .ok_or_else(|| Error::Invalid("GIF 调色板索引无效".into()))?;
                [p[0], p[1], p[2]]
            }
            None => [index, index, index],
        };
        Ok(image::Rgba([rgb[0], rgb[1], rgb[2], if frame.transparent == Some(index) { 0 } else { 255 }]))
    };
    let (left, top) = (u32::from(frame.left), u32::from(frame.top));
    let (fw, fh) = (u32::from(frame.width), u32::from(frame.height));
    if fw == 0 || fh == 0 || frame.buffer.len() != fw as usize * fh as usize {
        return Err(Error::Invalid("GIF 首帧像素长度无效".into()));
    }
    let mut image =
        RgbaImage::from_pixel(width.max(left + fw), height.max(top + fh), colour(frame.transparent.unwrap_or(0))?);
    for y in 0..fh {
        for x in 0..fw {
            image.put_pixel(left + x, top + y, colour(frame.buffer[(y * fw + x) as usize])?);
        }
    }
    Ok(image)
}

fn pnm(bytes: &[u8]) -> Result<RgbaImage> {
    let Some(PnmSamples { width, height, bands, maxval, samples }) = pnm_samples(bytes, None)? else {
        return Ok(image::load_from_memory(bytes).map_err(|e| Error::Invalid(e.to_string()))?.to_rgba8());
    };
    // Match Pillow's value/maxval*out_max order and Python round (ties even).
    // Gray >8bit becomes I and subsequently clamps at 255, not a 16→8 shift.
    let out_max = if bands == 1 && maxval > 255 { 65535. } else { 255. };
    let scaled: Vec<_> = samples
        .into_iter()
        .map(|v| ((f64::from(v) / f64::from(maxval) * out_max).round_ties_even().min(out_max) as u32).min(255) as u8)
        .collect();
    let pixels = if bands == 1 {
        scaled.into_iter().flat_map(|v| [v, v, v, 255]).collect()
    } else {
        scaled.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect()
    };
    RgbaImage::from_raw(width, height, pixels).ok_or_else(|| Error::Invalid("PNM 像素长度无效".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pam_header_dimensions_and_hidden_colour_are_identity() {
        // Independent Python hashlib over this literal PAM, never a candidate fixture.
        assert_eq!(rgba_hash(1, 1, &[12, 34, 56, 0]).unwrap(), "67222c4833b8bfd91eb0f1f2ce68ce67");
        assert_ne!(rgba_hash(1, 1, &[12, 34, 56, 0]).unwrap(), rgba_hash(1, 1, &[0, 0, 0, 0]).unwrap());
        assert_ne!(rgba_hash(2, 1, &[0; 8]).unwrap(), rgba_hash(1, 2, &[0; 8]).unwrap());
        assert!(rgba_hash(0, 1, &[]).is_err());
        assert!(rgba_hash(1, 1, &[0; 3]).is_err());
    }
}
