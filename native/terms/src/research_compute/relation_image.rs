//! Displayed-image contract of web/{similar,relate}. No ICC conversion is added.
//! Pillow's BOX byte signature and BILINEAR mode-F arrays are distinct from IQDB's Haar input.
use crate::{
    image_samples::tiff_scalar::{self, ScalarMode, ScalarSamples, ScalarTiff},
    Error, Result,
};
use image::{metadata::Orientation, DynamicImage, ImageReader};
use std::{
    fs,
    io::{BufRead, BufReader, Cursor, Read, Seek},
    path::Path,
};

const WRONG_MODE: &str = "image has wrong mode";
const UNKNOWN_RAW_MODE: &str = "unknown raw mode for given image mode";
pub(super) fn is_mode_error(error: &Error) -> bool {
    matches!(error, Error::Invalid(message) if message == WRONG_MODE || message == UNKNOWN_RAW_MODE)
}

#[derive(Clone, Debug)]
pub struct Plane {
    pub width: usize,
    pub height: usize,
    pub channels: usize,
    pub pixels: Vec<f32>,
}
impl Plane {
    pub fn new(width: usize, height: usize, channels: usize, pixels: Vec<f32>) -> Result<Self> {
        if width == 0
            || height == 0
            || ![1, 3].contains(&channels)
            || width.checked_mul(height).and_then(|n| n.checked_mul(channels)) != Some(pixels.len())
            || pixels.iter().any(|x| !x.is_finite())
        {
            return Err(Error::Invalid("invalid relation image".into()));
        }
        Ok(Self { width, height, channels, pixels })
    }
    pub fn at(&self, y: usize, x: usize, c: usize) -> f32 {
        self.pixels[(y * self.width + x) * self.channels + c]
    }
    pub fn crop(&self, x0: usize, y0: usize, x1: usize, y1: usize) -> Result<Self> {
        if x0 >= x1 || y0 >= y1 || x1 > self.width || y1 > self.height {
            return Err(Error::Invalid("empty relation crop".into()));
        }
        let mut pixels = Vec::with_capacity((x1 - x0) * (y1 - y0) * self.channels);
        for y in y0..y1 {
            pixels.extend_from_slice(
                &self.pixels[(y * self.width + x0) * self.channels..(y * self.width + x1) * self.channels],
            );
        }
        Self::new(x1 - x0, y1 - y0, self.channels, pixels)
    }
    pub fn resize(&self, width: usize, height: usize) -> Self {
        // Each channel is a Pillow mode F image: double coefficients/accumulators,
        // f32 storage after each axis. Never clamp or quantise these float samples.
        if vertical_first(self.width, self.height, height) {
            let swapped = Self {
                width: self.height,
                height: self.width,
                channels: self.channels,
                pixels: transpose_pixels(&self.pixels, self.width, self.height, self.channels),
            }
            .resize(height, width);
            return Self {
                width,
                height,
                channels: self.channels,
                pixels: transpose_pixels(&swapped.pixels, height, width, self.channels),
            };
        }
        let mut p = self.clone();
        if width != p.width {
            let coeff = coefficients(p.width, width, Filter::Linear, p.width as f64);
            let mut dst = vec![0.; width * p.height * p.channels];
            for y in 0..p.height {
                for (x, (start, weights)) in coeff.iter().enumerate() {
                    for c in 0..p.channels {
                        dst[(y * width + x) * p.channels + c] = weights
                            .iter()
                            .enumerate()
                            .fold(0., |s, (j, w)| resample_add(f64::from(p.at(y, start + j, c)), *w, s))
                            as f32;
                    }
                }
            }
            p.pixels = dst;
            p.width = width;
        }
        if height != p.height {
            let coeff = coefficients(p.height, height, Filter::Linear, p.height as f64);
            let mut dst = vec![0.; p.width * height * p.channels];
            for (y, (start, weights)) in coeff.iter().enumerate() {
                for x in 0..p.width {
                    for c in 0..p.channels {
                        dst[(y * p.width + x) * p.channels + c] = weights
                            .iter()
                            .enumerate()
                            .fold(0., |s, (j, w)| resample_add(f64::from(p.at(start + j, x, c)), *w, s))
                            as f32;
                    }
                }
            }
            p.pixels = dst;
            p.height = height;
        }
        p
    }
}
// Image.resize in Pillow 12.3 runs vertical first for a shrinking image whose
// current height exceeds 100 times its width, including after reducing_gap.
// Retain that strict boundary and the existing per-axis storage/rounding.
fn vertical_first(width: usize, height: usize, out_height: usize) -> bool {
    height > width.saturating_mul(100) && out_height < height
}
fn transpose_pixels<T: Copy>(pixels: &[T], width: usize, height: usize, channels: usize) -> Vec<T> {
    let mut transposed = Vec::with_capacity(pixels.len());
    for x in 0..width {
        for y in 0..height {
            transposed.extend_from_slice(&pixels[(y * width + x) * channels..(y * width + x + 1) * channels]);
        }
    }
    transposed
}
fn resample_add(pixel: f64, weight: f64, sum: f64) -> f64 {
    // Current macOS Pillow 12.3 wheel contracts this C double multiply/add.
    // An independent libSystem fma probe confirms the otherwise observed 1-ulp
    // mode-F discrepancy; no epsilon or output rounding is added.
    #[cfg(target_os = "macos")]
    {
        pixel.mul_add(weight, sum)
    }
    #[cfg(not(target_os = "macos"))]
    {
        sum + pixel * weight
    }
}
#[derive(Clone)]
pub struct Loaded {
    pub image: Plane,
    pub size: (usize, usize),
}
#[derive(Clone, Copy)]
enum Filter {
    Box,
    Linear,
    Cubic,
}
fn coefficients(input: usize, output: usize, filter: Filter, extent: f64) -> Vec<(usize, Vec<f64>)> {
    // Pillow's box endpoints are C floats; 12.3 precomputes reciprocal filter scale.
    let scale = f64::from(extent as f32) / output as f64;
    let spread = scale.max(1.);
    let support = spread
        * match filter {
            Filter::Cubic => 2.,
            Filter::Box => 0.5,
            _ => 1.,
        };
    (0..output)
        .map(|i| {
            let center = (i as f64 + 0.5) * scale;
            let start = ((center - support + 0.5) as isize).max(0) as usize;
            let end = ((center + support + 0.5) as usize).min(input);
            let mut weights: Vec<_> = (start..end)
                .map(|j| {
                    let d = (j as f64 - center + 0.5) * (1. / spread);
                    let x = d.abs();
                    match filter {
                        Filter::Box => {
                            if d > -0.5 && d <= 0.5 {
                                1.
                            } else {
                                0.
                            }
                        }
                        Filter::Linear => (1. - x).max(0.),
                        Filter::Cubic if x < 1. => (1.5 * x - 2.5) * x * x + 1.,
                        Filter::Cubic if x < 2. => (((x - 5.) * x + 8.) * x - 4.) * -0.5,
                        _ => 0.,
                    }
                })
                .collect();
            let sum: f64 = weights.iter().sum();
            if sum != 0. {
                for w in &mut weights {
                    *w /= sum;
                }
            }
            (start, weights)
        })
        .collect()
}
fn resize_bytes(
    pixels: &[u8],
    size: (usize, usize),
    out: (usize, usize),
    channels: usize,
    filter: Filter,
    extent: (f64, f64),
) -> Vec<u8> {
    let (w, h) = size;
    let (ow, oh) = out;
    if vertical_first(w, h, oh) {
        let swapped = resize_bytes(
            &transpose_pixels(pixels, w, h, channels),
            (h, w),
            (oh, ow),
            channels,
            filter,
            (extent.1, extent.0),
        );
        return transpose_pixels(&swapped, oh, ow, channels);
    }
    let horizontal = if w == ow && extent.0 == w as f64 {
        pixels.to_vec()
    } else {
        let coeff = coefficients(w, ow, filter, extent.0);
        let mut dst = vec![0; ow * h * channels];
        for y in 0..h {
            for (x, (start, weights)) in coeff.iter().enumerate() {
                for c in 0..channels {
                    let sum = weights.iter().enumerate().fold(1i64 << 21, |s, (j, k)| {
                        s + i64::from(pixels[(y * w + start + j) * channels + c])
                            * (*k * (1u32 << 22) as f64 + if *k < 0. { -0.5 } else { 0.5 }) as i64
                    });
                    dst[(y * ow + x) * channels + c] = (sum >> 22).clamp(0, 255) as u8;
                }
            }
        }
        dst
    };
    if h == oh && extent.1 == h as f64 {
        return horizontal;
    }
    let coeff = coefficients(h, oh, filter, extent.1);
    let mut dst = vec![0; ow * oh * channels];
    for (y, (start, weights)) in coeff.iter().enumerate() {
        for x in 0..ow {
            for c in 0..channels {
                let sum = weights.iter().enumerate().fold(1i64 << 21, |s, (j, k)| {
                    s + i64::from(horizontal[((start + j) * ow + x) * channels + c])
                        * (*k * (1u32 << 22) as f64 + if *k < 0. { -0.5 } else { 0.5 }) as i64
                });
                dst[(y * ow + x) * channels + c] = (sum >> 22).clamp(0, 255) as u8;
            }
        }
    }
    dst
}
#[derive(Clone, Copy)]
enum IntegerMode {
    U16,
    U16Big,
    I32,
}
// The TIFF owner returns unconverted samples. Only display owns thumbnail and
// its per-mode storage, while signature/colour reuse the owner's RGB conversion.
fn scalar_domain(metadata: Option<&exif::Exif>) -> bool {
    let Some(metadata) = metadata else { return false };
    let uint = |number| {
        metadata.get_field(exif::Tag(exif::Context::Tiff, number), exif::In::PRIMARY).and_then(|f| f.value.get_uint(0))
    };
    uint(0x106) == Some(1)
        && uint(0x115).unwrap_or(1) == 1
        && matches!((uint(0x102), uint(0x153).unwrap_or(1)), (Some(16), 1) | (Some(32), 2 | 3))
}
// Pillow's other accepted scalar raw modes need their original I/F storage
// before thumbnail; do not route signed16 or unsigned32 through RGB scaling.
fn extra_scalar_domain(metadata: Option<&exif::Exif>) -> bool {
    let Some(m) = metadata else { return false };
    let uint = |n| m.get_field(exif::Tag(exif::Context::Tiff, n), exif::In::PRIMARY).and_then(|f| f.value.get_uint(0));
    let mode = (uint(262), uint(258), uint(339).unwrap_or(1));
    let extra = matches!(
        mode,
        (Some(1), Some(12), 1)
            | (Some(1), Some(16), 2)
            | (Some(1), Some(32), 1)
            | (Some(0), Some(16), 1)
            | (Some(0), Some(32), 3)
    );
    let tiled = m.get_field(exif::Tag(exif::Context::Tiff, 324), exif::In::PRIMARY).is_some();
    let mm32 = !m.little_endian()
        && uint(259).unwrap_or(1) != 1
        && (uint(259) != Some(8) || tiled || uint(284).unwrap_or(1) != 1 || uint(317).unwrap_or(1) != 1)
        && matches!(mode, (Some(1), Some(32), 2 | 3));
    let tiled_u16 = tiled && mode == (Some(1), Some(16), 1);
    let compressed_fill_u16 =
        m.little_endian() && uint(266) == Some(2) && uint(259).unwrap_or(1) != 1 && mode == (Some(1), Some(16), 1);
    uint(277).unwrap_or(1) == 1 && (compressed_fill_u16 || uint(266).unwrap_or(1) == 1 && (extra || mm32 || tiled_u16))
}
enum ScalarInput {
    Shared(ScalarTiff),
    Extra(Vec<u8>),
}
fn extra_scalar_image(mut bytes: Vec<u8>, thumbnail: bool, path_input: bool) -> Result<(DynamicImage, (usize, usize))> {
    use tiff_display::{
        decoder::{Decoder, DecodingResult, Limits},
        tags::Tag,
    };
    let invalid = |e: tiff_display::TiffError| Error::Invalid(e.to_string());
    let mut decoder = Decoder::new(Cursor::new(&bytes)).map_err(invalid)?;
    let (width, height) = decoder.dimensions().map_err(invalid)?;
    limit(width, height)?;
    let photo: u16 = decoder.get_tag_unsigned(Tag::PhotometricInterpretation).map_err(invalid)?;
    let bits: u16 = decoder.get_tag_unsigned(Tag::BitsPerSample).map_err(invalid)?;
    let sample: u16 = decoder.find_tag_unsigned(Tag::SampleFormat).map_err(invalid)?.unwrap_or(1);
    let orientation: u16 = decoder.find_tag_unsigned(Tag::Orientation).map_err(invalid)?.unwrap_or(1);
    let compression: u16 = decoder.find_tag_unsigned(Tag::Compression).map_err(invalid)?.unwrap_or(1);
    let planar: u16 = decoder.find_tag_unsigned(Tag::PlanarConfiguration).map_err(invalid)?.unwrap_or(1);
    let offsets: Vec<u64> = decoder.find_tag_unsigned_vec(Tag::StripOffsets).map_err(invalid)?.unwrap_or_default();
    let fill: u16 = decoder.find_tag_unsigned(Tag::FillOrder).map_err(invalid)?.unwrap_or(1);
    let big = bytes.starts_with(b"MM");
    if fill == 2 && compression != 1 {
        drop(decoder);
        normalise_tiff_fill(&mut bytes)?;
        decoder = Decoder::new(Cursor::new(&bytes)).map_err(invalid)?;
    }
    if big && sample == 1 && (photo == 0 || bits == 32) {
        return Err(Error::Invalid("unknown pixel mode".into()));
    }
    // Pillow's WhiteIsZero I;16 and F raw modes do not invert scalar values.
    if photo == 0 {
        drop(decoder);
        palette_as_gray(&mut bytes)?;
        decoder = Decoder::new(Cursor::new(&bytes)).map_err(invalid)?;
    }
    let values = if bits == 12 {
        let tile_width: Option<u32> = decoder.find_tag_unsigned(Tag::TileWidth).map_err(invalid)?;
        drop(decoder);
        set_tiff_uint(
            &mut bytes,
            256,
            width.checked_mul(12).ok_or_else(|| Error::Invalid("invalid packed TIFF width".into()))?.div_ceil(8),
        )?;
        set_tiff_uint(&mut bytes, 258, 8)?;
        if let Some(tile_width) = tile_width {
            // Read every tile's complete encoded row, including edge padding;
            // the original clipped-pixel stride is applied below.
            let packed_tile_width = tile_width
                .checked_mul(12)
                .ok_or_else(|| Error::Invalid("invalid packed TIFF tile".into()))?
                .div_ceil(8);
            set_tiff_uint(
                &mut bytes,
                256,
                width
                    .div_ceil(tile_width)
                    .checked_mul(packed_tile_width)
                    .ok_or_else(|| Error::Invalid("invalid packed TIFF width".into()))?,
            )?;
            set_tiff_uint(
                &mut bytes,
                322,
                tile_width
                    .checked_mul(12)
                    .ok_or_else(|| Error::Invalid("invalid packed TIFF tile".into()))?
                    .div_ceil(8),
            )?;
        }
        let mut packed = Decoder::new(Cursor::new(&bytes)).map_err(invalid)?.with_limits(Limits::unlimited());
        let mut values = vec![0u16; width as usize * height as usize];
        let unpack = |row: &[u8], x: usize| -> Result<u16> {
            let bit = x * 12;
            let at = bit / 8;
            let a = *row.get(at).ok_or_else(|| Error::Invalid("truncated packed TIFF row".into()))?;
            let b = *row.get(at + 1).ok_or_else(|| Error::Invalid("truncated packed TIFF row".into()))?;
            Ok(((u16::from(a) << 8) | u16::from(b)) >> (4 - bit % 8) & 4095)
        };
        if let Some(cw) = tile_width {
            let ch: u32 = packed.get_tag_unsigned(Tag::TileLength).map_err(invalid)?;
            let across = width.div_ceil(cw) as usize;
            let count = packed.tile_count().map_err(invalid)?;
            for chunk in 0..count {
                let DecodingResult::U8(data) = packed.read_chunk(chunk).map_err(invalid)? else {
                    return Err(Error::Invalid("invalid packed TIFF samples".into()));
                };
                let (x0, y0) = ((chunk as usize % across) * cw as usize, (chunk as usize / across) * ch as usize);
                let dw = (cw as usize).min(width as usize - x0);
                let dh = (ch as usize).min(height as usize - y0);
                // Pillow raw edge tile uses floor(tile_width*12/8) stride;
                // full tile rows use the unpacker's ceil stride. Keep both.
                let stride = if x0 + cw as usize > width as usize {
                    cw as usize * 12 / 8
                } else {
                    (cw as usize * 12).div_ceil(8)
                };
                for y in 0..dh {
                    let row =
                        data.get(y * stride..).ok_or_else(|| Error::Invalid("truncated packed TIFF tile".into()))?;
                    for x in 0..dw {
                        values[(y0 + y) * width as usize + x0 + x] = unpack(row, x)?;
                    }
                }
            }
        } else {
            let DecodingResult::U8(data) = packed.read_image().map_err(invalid)? else {
                return Err(Error::Invalid("invalid packed TIFF samples".into()));
            };
            let stride = (width as usize * 12).div_ceil(8);
            for (y, row) in data.chunks_exact(stride).enumerate() {
                for x in 0..width as usize {
                    values[y * width as usize + x] = unpack(row, x)?;
                }
            }
        }
        DecodingResult::U16(values)
    } else {
        decoder.with_limits(Limits::unlimited()).read_image().map_err(invalid)?
    };
    let (mut w, mut h) = (width as usize, height as usize);
    if path_input
        && bits == 16
        && sample == 1
        && compression == 1
        && planar == 1
        && offsets.len() == 1
        && (5..=8).contains(&orientation)
    {
        std::mem::swap(&mut w, &mut h);
    }
    let size = if (5..=8).contains(&orientation) { (h, w) } else { (w, h) };
    let out = if thumbnail { thumbnail_size(size) } else { size };
    let gray: Vec<u8> = match values {
        DecodingResult::I16(values) => {
            let values = values
                .into_iter()
                .map(|v| i32::from(if big && compression != 1 { v.swap_bytes() } else { v }))
                .collect();
            let values = orient_scalar(values, w, h, orientation);
            thumbnail_i32(values, size, out).into_iter().map(|v| v.clamp(0, 255) as u8).collect()
        }
        DecodingResult::I32(values) => {
            let values = values.into_iter().map(|v| if big && compression != 1 { v.swap_bytes() } else { v }).collect();
            thumbnail_i32(orient_scalar(values, w, h, orientation), size, out)
                .into_iter()
                .map(|v| v.clamp(0, 255) as u8)
                .collect()
        }
        DecodingResult::U32(values) => {
            let values = orient_scalar(values.into_iter().map(|v| v as i32).collect(), w, h, orientation);
            thumbnail_i32(values, size, out).into_iter().map(|v| v.clamp(0, 255) as u8).collect()
        }
        DecodingResult::U16(values) => {
            if thumbnail && ((size.0 / out.0 / 2).max(1) > 1 || (size.1 / out.1 / 2).max(1) > 1) {
                return Err(Error::Invalid(WRONG_MODE.into()));
            }
            let mut values = orient_scalar(values, w, h, orientation);
            if big && size != out {
                values.iter_mut().for_each(|v| *v = v.swap_bytes());
            }
            let mut values = resize_u16(&values, size, out);
            if big && size != out {
                values.iter_mut().for_each(|v| *v = v.swap_bytes());
            }
            values.into_iter().map(|v| v.min(255) as u8).collect()
        }
        DecodingResult::F32(values) => {
            let values = values
                .into_iter()
                .map(|v| if big && compression != 1 { f32::from_bits(v.to_bits().swap_bytes()) } else { v })
                .collect();
            let values = orient_scalar(values, w, h, orientation);
            thumbnail_32(values, size, out, |a, b| a + b, |v| v as f32).into_iter().map(|v| v as u8).collect()
        }
        _ => return Err(Error::Invalid("invalid extra scalar samples".into())),
    };
    let pixels = gray.into_iter().flat_map(|v| [v; 3]).collect();
    let im = image::RgbImage::from_raw(out.0 as u32, out.1 as u32, pixels)
        .ok_or_else(|| Error::Invalid("invalid extra scalar buffer".into()))?;
    Ok((DynamicImage::ImageRgb8(im), size))
}

fn scalar_path(path: &Path) -> Result<Option<ScalarInput>> {
    let mut reader = BufReader::new(fs::File::open(path)?);
    let header = reader.fill_buf()?;
    if !header.starts_with(b"II") && !header.starts_with(b"MM") {
        return Ok(None);
    }
    let metadata = exif::Reader::new().read_from_container(&mut reader).ok();
    let extra = extra_scalar_domain(metadata.as_ref());
    if !extra && !scalar_domain(metadata.as_ref()) {
        return Ok(None);
    }
    reader.rewind()?;
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes)?;
    if extra {
        return Ok(Some(ScalarInput::Extra(bytes)));
    }
    let scalar = tiff_scalar::scalar_samples(&bytes)?;
    limit(scalar.width, scalar.height)?;
    Ok(Some(ScalarInput::Shared(scalar)))
}
fn orient_scalar<T: Copy + Default>(pixels: Vec<T>, width: usize, height: usize, orientation: u16) -> Vec<T> {
    if !(2..=8).contains(&orientation) {
        return pixels;
    }
    let out_width = if (5..=8).contains(&orientation) { height } else { width };
    let mut out = vec![T::default(); pixels.len()];
    for y in 0..height {
        for x in 0..width {
            let (dx, dy) = match orientation {
                2 => (width - 1 - x, y),
                3 => (width - 1 - x, height - 1 - y),
                4 => (x, height - 1 - y),
                5 => (y, x),
                6 => (height - 1 - y, x),
                7 => (height - 1 - y, width - 1 - x),
                8 => (y, width - 1 - x),
                _ => (x, y),
            };
            out[dy * out_width + dx] = pixels[y * width + x];
        }
    }
    out
}
fn load_scalar(scalar: ScalarTiff) -> Result<Loaded> {
    let ScalarTiff {
        width,
        height,
        mode,
        samples,
        orientation,
        raw_path_mmap_eligible,
        big_endian,
        compression,
        layout,
        icc: _,
    } = scalar;
    // Follow the TIFF owner's measured ordinary Deflate8 consumer qualification.
    // Pure samples remain numeric originals; Pillow's MM 32-bit libtiff rawmode
    // reinterpretation belongs here, before orientation, thumbnail and reduce.
    let swap_32 = if big_endian && compression != 1 && matches!(mode, ScalarMode::Signed32 | ScalarMode::Float32) {
        let expected_strips =
            if layout.rows_per_strip == 0 { None } else { Some(height.div_ceil(layout.rows_per_strip) as usize) };
        if compression != 8
            || layout.tiled
            || layout.planar_configuration != 1
            || layout.predictor != 1
            || expected_strips.is_none()
            || layout.strip_count != expected_strips
        {
            return Err(Error::Invalid("TIFF big-endian compressed scalar layout remains unaccepted".into()));
        }
        cfg!(target_endian = "little")
    } else {
        false
    };
    // Display's filename entry has the original mmap reshape before load_end.
    let (w, h) = if raw_path_mmap_eligible && (5..=8).contains(&orientation) {
        (height as usize, width as usize)
    } else {
        (width as usize, height as usize)
    };
    let size = if (5..=8).contains(&orientation) { (h, w) } else { (w, h) };
    let out = thumbnail_size(size);
    let rgb = match (mode, samples) {
        (ScalarMode::Unsigned16Le | ScalarMode::Unsigned16Be, ScalarSamples::U16(values)) => {
            if (size.0 / out.0 / 2).max(1) > 1 || (size.1 / out.1 / 2).max(1) > 1 {
                return Err(Error::Invalid(WRONG_MODE.into()));
            }
            let mut values = orient_scalar(values, w, h, orientation);
            let native_big = mode == ScalarMode::Unsigned16Be && size != out;
            if native_big {
                values.iter_mut().for_each(|v| *v = v.swap_bytes());
            }
            let mut values = resize_u16(&values, size, out);
            if native_big {
                values.iter_mut().for_each(|v| *v = v.swap_bytes());
            }
            values.into_iter().map(|v| v.min(255) as u8).collect::<Vec<_>>()
        }
        (ScalarMode::Signed32, ScalarSamples::I32(mut values)) => {
            if swap_32 {
                values.iter_mut().for_each(|v| *v = v.swap_bytes());
            }
            thumbnail_i32(orient_scalar(values, w, h, orientation), size, out)
                .into_iter()
                .map(|v| v.clamp(0, 255) as u8)
                .collect()
        }
        (ScalarMode::Float32, ScalarSamples::F32(mut values)) => {
            if swap_32 {
                values.iter_mut().for_each(|v| *v = f32::from_bits(v.to_bits().swap_bytes()));
            }
            thumbnail_32(orient_scalar(values, w, h, orientation), size, out, |a, b| a + b, |v| v as f32)
                .into_iter()
                .map(|v| v as u8)
                .collect()
        }
        _ => return Err(Error::Invalid("invalid scalar display buffer".into())),
    };
    let pixels = rgb.into_iter().flat_map(|v| [f32::from(v); 3]).collect();
    Ok(Loaded { image: Plane::new(out.0, out.1, 3, pixels)?, size })
}
fn decode(path: &Path, bound: usize) -> Result<(DynamicImage, bool, bool, IntegerMode)> {
    decode_reader(BufReader::new(fs::File::open(path)?), bound, image::ImageFormat::from_path(path).ok(), true, true)
}
fn decode_reader<R: BufRead + Seek + std::panic::UnwindSafe>(
    mut reader: R,
    bound: usize,
    fallback: Option<image::ImageFormat>,
    transpose: bool,
    is_path: bool,
) -> Result<(DynamicImage, bool, bool, IntegerMode)> {
    let metadata = exif::Reader::new().read_from_container(&mut reader).ok();
    let orientation = metadata
        .as_ref()
        .and_then(|e| e.get_field(exif::Tag::Orientation, exif::In::PRIMARY).and_then(|f| f.value.get_uint(0)))
        .and_then(|v| u8::try_from(v).ok())
        .and_then(Orientation::from_exif)
        .unwrap_or(Orientation::NoTransforms);
    reader.rewind()?;
    let header = reader.fill_buf()?;
    // PNG tRNS expands RGB/L to alpha in image's decoder; Pillow retains the
    // original RGB/L bands, so the legacy matte condition must not see that A.
    let png_colour = header.starts_with(b"\x89PNG\r\n\x1a\n").then(|| header.get(25).copied()).flatten();
    if (header.starts_with(b"II") || header.starts_with(b"MM")) && extra_scalar_domain(metadata.as_ref()) {
        reader.rewind()?;
        let mut bytes = vec![];
        reader.read_to_end(&mut bytes)?;
        let (image, _) = extra_scalar_image(bytes, false, is_path)?;
        return Ok((image, false, false, IntegerMode::U16));
    }
    if (header.starts_with(b"II") || header.starts_with(b"MM")) && scalar_domain(metadata.as_ref()) {
        reader.rewind()?;
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes)?;
        // Signature/colour both convert before BOX and keep TIFF's load_end
        // orientation. The owner's conversion is shared; no ICC is added here.
        let (im, _) = tiff_scalar::scalar_bytes(&bytes, is_path)?;
        limit(im.width(), im.height())?;
        return Ok((DynamicImage::ImageRgba8(im), false, false, IntegerMode::U16));
    }

    let tiff_jpeg = metadata.as_ref().is_some_and(|m| {
        m.get_field(exif::Tag(exif::Context::Tiff, 259), exif::In::PRIMARY).and_then(|f| f.value.get_uint(0)) == Some(7)
    });
    let tiff_special = metadata.as_ref().is_some_and(|m| {
        let uint =
            |n| m.get_field(exif::Tag(exif::Context::Tiff, n), exif::In::PRIMARY).and_then(|f| f.value.get_uint(0));
        uint(262) == Some(3)
            || uint(258).unwrap_or(1) < 8
            || uint(277) == Some(2)
            || uint(284) == Some(2)
            || uint(338) == Some(1)
            || uint(266) == Some(2)
    });
    let tga = tga_header(header);
    let mut nearest = (tga && (header[2] == 1 || header[2] == 9) && header[1] == 1)
        || (tga && header[16] == 1)
        || header.starts_with(b"GIF")
        || header.starts_with(b"P1")
        || header.starts_with(b"P4")
        || (header.starts_with(b"\x89PNG\r\n\x1a\n")
            && (header.get(25) == Some(&3) || header.get(24) == Some(&1) && header.get(25) == Some(&0)))
        || bmp_nearest(header);
    let gif = header.starts_with(b"GIF");
    let mut integer_mode = if header.starts_with(b"MM") { IntegerMode::U16Big } else { IntegerMode::U16 };
    let gif_canvas = if gif {
        reader.rewind()?;
        let canvas = gif_first_canvas(&mut reader)?;
        reader.rewind()?;
        Some(canvas)
    } else {
        None
    };
    let mut reader = ImageReader::new(reader).with_guessed_format()?;
    if reader.format().is_none() {
        if let Some(f) = fallback.or(tga.then_some(image::ImageFormat::Tga)) {
            reader.set_format(f);
        }
    }
    // Pillow's TIFF decoder itself applies EXIF in load_end, including callers
    // that do not explicitly request ImageOps.exif_transpose (_colour).
    let decoder_transposes = reader.format() == Some(image::ImageFormat::Tiff);
    let (mut im, cmyk) = if reader.format() == Some(image::ImageFormat::Jpeg) {
        std::panic::catch_unwind(|| -> std::io::Result<(DynamicImage, bool)> {
            let mut d = mozjpeg::Decompress::new_reader(reader.into_inner())?;
            limit(d.width() as u32, d.height() as u32)?;
            let ratio = d.width().min(d.height()) / bound;
            d.scale(if ratio >= 8 {
                1
            } else if ratio >= 4 {
                2
            } else if ratio >= 2 {
                4
            } else {
                8
            });
            let cmyk = matches!(d.color_space(), mozjpeg::ColorSpace::JCS_CMYK | mozjpeg::ColorSpace::JCS_YCCK);
            let mut d = if cmyk { d.to_colorspace(mozjpeg::ColorSpace::JCS_CMYK)? } else { d.rgb()? };
            let (w, h) = (d.width() as u32, d.height() as u32);
            let p = d.read_scanlines::<u8>()?;
            d.finish()?;
            let im = if cmyk {
                // Store non-inverted CMYK channels in a four-channel container;
                // its fourth channel is K, never alpha. Resize before converting RGB.
                DynamicImage::ImageRgba8(
                    image::RgbaImage::from_raw(w, h, p.into_iter().map(|v| 255 - v).collect())
                        .ok_or_else(|| std::io::Error::other("invalid CMYK JPEG buffer"))?,
                )
            } else {
                DynamicImage::ImageRgb8(
                    image::RgbImage::from_raw(w, h, p).ok_or_else(|| std::io::Error::other("invalid JPEG buffer"))?,
                )
            };
            Ok((im, cmyk))
        })
        .map_err(|_| Error::Invalid("JPEG decoder failed".into()))??
    } else if reader.format() == Some(image::ImageFormat::Tga) {
        let mut input = reader.into_inner();
        input.rewind()?;
        let mut bytes = vec![];
        input.read_to_end(&mut bytes)?;
        (display_tga(bytes)?, false)
    } else if reader.format() == Some(image::ImageFormat::Ico) {
        let mut input = reader.into_inner();
        input.rewind()?;
        let mut bytes = vec![];
        input.read_to_end(&mut bytes)?;
        let (image, is_nearest, is_cmyk, mode) = display_ico(&bytes, bound)?;
        nearest = is_nearest;
        integer_mode = mode;
        (image, is_cmyk)
    } else if reader.format() == Some(image::ImageFormat::Pnm) {
        let mut input = reader.into_inner();
        input.rewind()?;
        let mut bytes = Vec::new();
        input.read_to_end(&mut bytes)?;
        let im = if let Some(raw) = crate::image_samples::pnm::pnm_samples(&bytes, Some(178_956_970))? {
            // The shared parser owns raw samples only. Keep display's original
            // I-mode thumbnail before RGB clamp separate from PAM/hash conversion.
            let integer = raw.bands == 1 && raw.maxval > 255;
            let maximum = if integer { 65535. } else { 255. };
            let values: Vec<_> = raw
                .samples
                .into_iter()
                .map(|v| (f64::from(v) / f64::from(raw.maxval) * maximum).round_ties_even().min(maximum) as u16)
                .collect();
            if integer {
                integer_mode = IntegerMode::I32;
                DynamicImage::ImageLuma16(
                    image::ImageBuffer::from_raw(raw.width, raw.height, values)
                        .ok_or_else(|| Error::Invalid("invalid PNM integer buffer".into()))?,
                )
            } else if raw.bands == 1 {
                DynamicImage::ImageLuma8(
                    image::GrayImage::from_raw(raw.width, raw.height, values.into_iter().map(|v| v as u8).collect())
                        .ok_or_else(|| Error::Invalid("invalid PNM gray buffer".into()))?,
                )
            } else {
                DynamicImage::ImageRgb8(
                    image::RgbImage::from_raw(raw.width, raw.height, values.into_iter().map(|v| v as u8).collect())
                        .ok_or_else(|| Error::Invalid("invalid PNM RGB buffer".into()))?,
                )
            }
        } else {
            image::load_from_memory(&bytes).map_err(|e| Error::Invalid(e.to_string()))?
        };
        (im, false)
    } else if decoder_transposes && tiff_jpeg {
        let mut input = reader.into_inner();
        input.rewind()?;
        let mut bytes = vec![];
        input.read_to_end(&mut bytes)?;
        display_jpeg_tiff(&bytes)?
    } else if decoder_transposes && tiff_special {
        let mut input = reader.into_inner();
        input.rewind()?;
        let mut bytes = vec![];
        input.read_to_end(&mut bytes)?;
        let (image, cmyk, is_nearest) = display_tiff(bytes)?;
        nearest = is_nearest;
        (image, cmyk)
    } else if decoder_transposes
        && metadata.as_ref().is_some_and(|m| {
            let uint = |number| {
                m.get_field(exif::Tag(exif::Context::Tiff, number), exif::In::PRIMARY).and_then(|f| f.value.get_uint(0))
            };
            uint(0x106) == Some(5) && uint(0x102) == Some(8) && uint(0x115) == Some(4)
        })
    {
        // image's TIFF adapter converts CMYK to RGB with float truncation.
        // Pillow keeps the four raw channels through thumbnail/reduce, then
        // uses its integer RGB conversion. Reuse that existing CMYK pipeline.
        let mut decoder =
            tiff::decoder::Decoder::new(reader.into_inner()).map_err(|e| Error::Invalid(e.to_string()))?;
        let (w, h) = decoder.dimensions().map_err(|e| Error::Invalid(e.to_string()))?;
        limit(w, h)?;
        decoder = decoder.with_limits(tiff::decoder::Limits::unlimited());
        let tiff::decoder::DecodingResult::U8(pixels) =
            decoder.read_image().map_err(|e| Error::Invalid(e.to_string()))?
        else {
            return Err(Error::Invalid("invalid CMYK TIFF channels".into()));
        };
        let im = image::RgbaImage::from_raw(w, h, pixels)
            .ok_or_else(|| Error::Invalid("invalid CMYK TIFF buffer".into()))?;
        (DynamicImage::ImageRgba8(im), true)
    } else {
        reader.no_limits();
        let d = reader.into_decoder().map_err(|e| Error::Invalid(e.to_string()))?;
        let (w, h) = image::ImageDecoder::dimensions(&d);
        limit(w, h)?;
        (DynamicImage::from_decoder(d).map_err(|e| Error::Invalid(e.to_string()))?, false)
    };
    if png_colour == Some(0) {
        im = match im {
            DynamicImage::ImageLumaA8(g) => {
                DynamicImage::ImageLuma8(image::GrayImage::from_fn(g.width(), g.height(), |x, y| {
                    image::Luma([g.get_pixel(x, y)[0]])
                }))
            }
            DynamicImage::ImageLumaA16(g) => {
                DynamicImage::ImageLuma16(image::ImageBuffer::from_fn(g.width(), g.height(), |x, y| {
                    image::Luma([g.get_pixel(x, y)[0]])
                }))
            }
            other => other,
        };
    } else if png_colour == Some(2) {
        im = match im {
            DynamicImage::ImageRgba8(g) => {
                DynamicImage::ImageRgb8(image::RgbImage::from_fn(g.width(), g.height(), |x, y| {
                    image::Rgb([g.get_pixel(x, y)[0], g.get_pixel(x, y)[1], g.get_pixel(x, y)[2]])
                }))
            }
            DynamicImage::ImageRgba16(g) => {
                DynamicImage::ImageRgb16(image::ImageBuffer::from_fn(g.width(), g.height(), |x, y| {
                    image::Rgb([g.get_pixel(x, y)[0], g.get_pixel(x, y)[1], g.get_pixel(x, y)[2]])
                }))
            }
            other => other,
        };
    }
    if decoder_transposes || png_colour.is_some() {
        // Pillow's PNG/TIFF RGB(A) 16-bit raw modes discard low bytes at
        // decode, before alpha composition or thumbnail. This is not image's
        // rounded scaling by 257, and is not PNM's separate maxval scaling.
        im = match im {
            DynamicImage::ImageRgb16(g) => {
                DynamicImage::ImageRgb8(image::RgbImage::from_fn(g.width(), g.height(), |x, y| {
                    image::Rgb(g.get_pixel(x, y).0.map(|v| (v >> 8) as u8))
                }))
            }
            DynamicImage::ImageRgba16(g) => {
                DynamicImage::ImageRgba8(image::RgbaImage::from_fn(g.width(), g.height(), |x, y| {
                    image::Rgba(g.get_pixel(x, y).0.map(|v| (v >> 8) as u8))
                }))
            }
            DynamicImage::ImageLumaA16(g) => {
                DynamicImage::ImageLumaA8(image::GrayAlphaImage::from_fn(g.width(), g.height(), |x, y| {
                    image::LumaA(g.get_pixel(x, y).0.map(|v| (v >> 8) as u8))
                }))
            }
            other => other,
        };
    }
    if decoder_transposes
        && metadata.as_ref().is_some_and(|m| {
            m.get_field(exif::Tag(exif::Context::Tiff, 0x152), exif::In::PRIMARY).and_then(|f| f.value.get_uint(0))
                == Some(0)
        })
    {
        // TIFF RGBX's fourth sample is padding, not Pillow's A band.
        if let DynamicImage::ImageRgba8(g) = im {
            im = DynamicImage::ImageRgb8(image::RgbImage::from_fn(g.width(), g.height(), |x, y| {
                let p = g.get_pixel(x, y);
                image::Rgb([p[0], p[1], p[2]])
            }));
        }
    }
    if decoder_transposes
        && is_path
        && tiff_path_mapped(metadata.as_ref())
        && matches!(
            orientation,
            Orientation::Rotate90 | Orientation::Rotate270 | Orientation::Rotate90FlipH | Orientation::Rotate270FlipH
        )
    {
        // Pillow's filename mmap fast path maps raw pixels using its already
        // swapped dimensions, before Tiff.load_end transposes. BytesIO uses the
        // ordinary decoder and does not have this behavior. Preserve both.
        im = reshape_image(im)?;
    }
    if let Some((left, top, width, height, background)) = gif_canvas {
        if let DynamicImage::ImageRgba8(g) = &mut im {
            for (x, y, pixel) in g.enumerate_pixels_mut() {
                if x < left || y < top || x >= left + width || y >= top + height {
                    *pixel = image::Rgba(background);
                }
            }
        }
    }
    if transpose || decoder_transposes {
        im.apply_orientation(orientation);
    }
    Ok((im, nearest, cmyk, integer_mode))
}
// TIFF 0.9's image adapter rejects palette, packed grayscale and LA and reads
// only one plane. Keep raw display samples here; ICC remains the #153 owner's job.
// Reuse the ordinary libjpeg backend for TIFF's abbreviated JPEG strips/tiles.
// The generic TIFF codecs use another IDCT and can change relation pixels.
fn display_jpeg_tiff(bytes: &[u8]) -> Result<(DynamicImage, bool)> {
    use tiff_display::{
        decoder::{ChunkType, Decoder},
        tags::Tag,
    };
    let bad = || Error::Invalid("invalid JPEG TIFF layout".into());
    let invalid = |e: tiff_display::TiffError| Error::Invalid(e.to_string());
    let mut decoder = Decoder::new(Cursor::new(bytes)).map_err(invalid)?;
    let (w, h) = decoder.dimensions().map_err(invalid)?;
    limit(w, h)?;
    let photo: u16 = decoder.get_tag_unsigned(Tag::PhotometricInterpretation).map_err(invalid)?;
    let bands: usize = decoder.find_tag_unsigned(Tag::SamplesPerPixel).map_err(invalid)?.unwrap_or(1);
    let planar: u16 = decoder.find_tag_unsigned(Tag::PlanarConfiguration).map_err(invalid)?.unwrap_or(1);
    if planar != 1 || !matches!((photo, bands), (1, 1) | (2 | 6, 3) | (5, 4)) {
        return Err(bad());
    }
    let (offset_tag, count_tag) = match decoder.get_chunk_type() {
        ChunkType::Strip => (Tag::StripOffsets, Tag::StripByteCounts),
        ChunkType::Tile => (Tag::TileOffsets, Tag::TileByteCounts),
    };
    let offsets: Vec<u64> = decoder.find_tag_unsigned_vec(offset_tag).map_err(invalid)?.ok_or_else(bad)?;
    let counts: Vec<u64> = decoder.find_tag_unsigned_vec(count_tag).map_err(invalid)?.ok_or_else(bad)?;
    let tables = decoder
        .find_tag(Tag::JPEGTables)
        .map_err(invalid)?
        .map(|v| v.into_u8_vec())
        .transpose()
        .map_err(invalid)?
        .unwrap_or_default();
    let (cw, ch) = decoder.chunk_dimensions();
    let across = w.div_ceil(cw) as usize;
    let expected = across * (h.div_ceil(ch) as usize);
    if offsets.len() != expected || counts.len() != expected {
        return Err(bad());
    }
    let mut pixels = vec![0; w as usize * h as usize * bands];
    for (i, (offset, count)) in offsets.into_iter().zip(counts).enumerate() {
        let start = usize::try_from(offset).map_err(|_| bad())?;
        let end = start.checked_add(usize::try_from(count).map_err(|_| bad())?).ok_or_else(bad)?;
        let chunk = bytes.get(start..end).ok_or_else(bad)?;
        let mut jpeg = vec![];
        if tables.is_empty() {
            jpeg.extend(chunk);
        } else {
            if !tables.starts_with(&[255, 216]) || !tables.ends_with(&[255, 217]) || !chunk.starts_with(&[255, 216]) {
                return Err(bad());
            }
            jpeg.extend(&tables[..tables.len() - 2]);
            jpeg.extend(&chunk[2..]);
        }
        let (dw, dh, data) = std::panic::catch_unwind(|| -> std::io::Result<_> {
            let d = mozjpeg::Decompress::new_mem(&jpeg)?;
            let mut d = d.to_colorspace(if bands == 1 {
                mozjpeg::ColorSpace::JCS_GRAYSCALE
            } else if bands == 4 {
                mozjpeg::ColorSpace::JCS_CMYK
            } else {
                mozjpeg::ColorSpace::JCS_RGB
            })?;
            let size = (d.width(), d.height());
            let data = d.read_scanlines::<u8>()?;
            d.finish()?;
            Ok((size.0, size.1, data))
        })
        .map_err(|_| bad())??;
        let (x0, y0) = ((i % across) * cw as usize, (i / across) * ch as usize);
        let width = (cw as usize).min(w as usize - x0);
        let height = (ch as usize).min(h as usize - y0);
        if dw < width || dh < height || data.len() != dw * dh * bands {
            return Err(bad());
        }
        for y in 0..height {
            let target = ((y0 + y) * w as usize + x0) * bands;
            pixels[target..target + width * bands].copy_from_slice(&data[y * dw * bands..(y * dw + width) * bands]);
        }
    }
    let im = match bands {
        1 => DynamicImage::ImageLuma8(image::GrayImage::from_raw(w, h, pixels).ok_or_else(bad)?),
        3 => DynamicImage::ImageRgb8(image::RgbImage::from_raw(w, h, pixels).ok_or_else(bad)?),
        _ => DynamicImage::ImageRgba8(image::RgbaImage::from_raw(w, h, pixels).ok_or_else(bad)?),
    };
    Ok((im, photo == 5))
}
fn normalise_tiff_fill(bytes: &mut [u8]) -> Result<()> {
    use tiff_display::{
        decoder::{ChunkType, Decoder},
        tags::Tag,
    };
    let invalid = |e: tiff_display::TiffError| Error::Invalid(e.to_string());
    let mut decoder = Decoder::new(Cursor::new(&*bytes)).map_err(invalid)?;
    let (offset_tag, count_tag) = match decoder.get_chunk_type() {
        ChunkType::Strip => (Tag::StripOffsets, Tag::StripByteCounts),
        ChunkType::Tile => (Tag::TileOffsets, Tag::TileByteCounts),
    };
    let offsets: Vec<u64> = decoder
        .find_tag_unsigned_vec(offset_tag)
        .map_err(invalid)?
        .ok_or_else(|| Error::Invalid("missing TIFF offsets".into()))?;
    let counts: Vec<u64> = decoder
        .find_tag_unsigned_vec(count_tag)
        .map_err(invalid)?
        .ok_or_else(|| Error::Invalid("missing TIFF byte counts".into()))?;
    drop(decoder);
    if offsets.len() != counts.len() {
        return Err(Error::Invalid("invalid TIFF chunk count".into()));
    }
    let mut ranges = Vec::with_capacity(offsets.len());
    for (offset, count) in offsets.into_iter().zip(counts) {
        let start = usize::try_from(offset).map_err(|_| Error::Invalid("invalid TIFF offset".into()))?;
        let length = usize::try_from(count).map_err(|_| Error::Invalid("invalid TIFF byte count".into()))?;
        let end = start.checked_add(length).ok_or_else(|| Error::Invalid("invalid TIFF chunk range".into()))?;
        if end > bytes.len() {
            return Err(Error::Invalid("truncated TIFF chunk".into()));
        }
        ranges.push((start, end));
    }
    ranges.sort_unstable();
    let mut reversed_end = 0;
    for (start, end) in ranges {
        // Several strips may share encoded storage; reverse their union once.
        for byte in &mut bytes[start.max(reversed_end)..end.max(reversed_end)] {
            *byte = byte.reverse_bits();
        }
        reversed_end = reversed_end.max(end);
    }
    Ok(())
}
fn display_tiff(mut bytes: Vec<u8>) -> Result<(DynamicImage, bool, bool)> {
    use tiff_display::{
        decoder::{Decoder, DecodingResult, Limits},
        tags::Tag,
    };
    let invalid = |e: tiff_display::TiffError| Error::Invalid(e.to_string());
    let mut decoder = Decoder::new(Cursor::new(&bytes)).map_err(invalid)?;
    let (w, h) = decoder.dimensions().map_err(invalid)?;
    limit(w, h)?;
    let photo: u16 = decoder.get_tag_unsigned(Tag::PhotometricInterpretation).map_err(invalid)?;
    let bits: u16 = decoder
        .find_tag_unsigned_vec::<u16>(Tag::BitsPerSample)
        .map_err(invalid)?
        .and_then(|v| v.first().copied())
        .unwrap_or(1);
    let bands: usize = decoder.find_tag_unsigned(Tag::SamplesPerPixel).map_err(invalid)?.unwrap_or(1);
    let planar: u16 = decoder.find_tag_unsigned(Tag::PlanarConfiguration).map_err(invalid)?.unwrap_or(1);
    let fill: u16 = decoder.find_tag_unsigned(Tag::FillOrder).map_err(invalid)?.unwrap_or(1);
    let extra: Vec<u16> = decoder.find_tag_unsigned_vec(Tag::ExtraSamples).map_err(invalid)?.unwrap_or_default();
    let compression: u16 = decoder.find_tag_unsigned(Tag::Compression).map_err(invalid)?.unwrap_or(1);
    if !(1..=4).contains(&bands) || !matches!(bits, 1 | 2 | 4 | 8 | 16) || photo == 3 && bits > 8 {
        return Err(Error::Invalid("unsupported TIFF display mode".into()));
    }
    if compression == 1 && planar == 2 && photo == 2 && extra.first() == Some(&1) {
        return Err(Error::Invalid(UNKNOWN_RAW_MODE.into()));
    }
    if fill == 2 && compression == 1 && (photo == 3 && bits < 8 || photo == 0 && bits == 8) {
        return Err(Error::Invalid(UNKNOWN_RAW_MODE.into()));
    }
    if fill == 2 && compression != 1 {
        // TIFF FillOrder reverses encoded stream bytes, before decompression.
        // libtiff normalises rawmode to FillOrder1 for its decoded samples.
        drop(decoder);
        normalise_tiff_fill(&mut bytes)?;
        decoder = Decoder::new(Cursor::new(&bytes)).map_err(invalid)?;
    }
    let palette: Vec<u16> = if photo == 3 {
        decoder
            .find_tag_unsigned_vec(Tag::ColorMap)
            .map_err(invalid)?
            .ok_or_else(|| Error::Invalid("missing TIFF palette".into()))?
    } else {
        vec![]
    };
    if photo == 3 {
        drop(decoder);
        // Only interpretation changes: the same locked codec decompresses raw
        // index samples, not RGB. Original metadata and file offsets stay intact.
        palette_as_gray(&mut bytes)?;
        decoder = Decoder::new(Cursor::new(&bytes)).map_err(invalid)?;
    }
    let raw_planar_offsets: Vec<u64> = if planar == 2 && bits == 16 && compression == 1 {
        let tag = match decoder.get_chunk_type() {
            tiff_display::decoder::ChunkType::Strip => Tag::StripOffsets,
            tiff_display::decoder::ChunkType::Tile => Tag::TileOffsets,
        };
        decoder
            .find_tag_unsigned_vec(tag)
            .map_err(invalid)?
            .ok_or_else(|| Error::Invalid("missing planar TIFF offsets".into()))?
    } else {
        vec![]
    };
    decoder = decoder.with_limits(Limits::unlimited());
    let raw = if planar == 2 && bands > 1 {
        let count = match decoder.get_chunk_type() {
            tiff_display::decoder::ChunkType::Strip => decoder.strip_count().map_err(invalid)?,
            tiff_display::decoder::ChunkType::Tile => decoder.tile_count().map_err(invalid)?,
        } as usize;
        if count % bands != 0 || !matches!(bits, 8 | 16) {
            return Err(Error::Invalid("invalid planar TIFF display layout".into()));
        }
        let per_band = count / bands;
        let (cw, ch) = decoder.chunk_dimensions();
        let across = w.div_ceil(cw) as usize;
        let mut pixels = vec![0u8; w as usize * h as usize * bands];
        for band in 0..bands {
            for chunk in 0..per_band {
                let index = band * per_band + chunk;
                let (dw, dh) = decoder.chunk_data_dimensions(index as u32);
                if !raw_planar_offsets.is_empty() {
                    // Pillow's raw planar setup indexes rawmode[layer], so
                    // RGB;16L/B becomes individual byte bands R/G/B/A. Retain
                    // that byte stride rather than silently repairing old pixels.
                    let (x0, y0) = ((chunk % across) * cw as usize, (chunk / across) * ch as usize);
                    let width = (cw as usize).min(w as usize - x0);
                    let height = (ch as usize).min(h as usize - y0);
                    let stride = if x0 + cw as usize > w as usize { cw as usize * 2 } else { width };
                    let start = usize::try_from(
                        *raw_planar_offsets
                            .get(index)
                            .ok_or_else(|| Error::Invalid("invalid planar TIFF offsets".into()))?,
                    )
                    .map_err(|_| Error::Invalid("invalid planar TIFF offset".into()))?;
                    for y in 0..height {
                        for x in 0..width {
                            let at = start
                                .checked_add(y * stride + x)
                                .ok_or_else(|| Error::Invalid("invalid planar TIFF buffer".into()))?;
                            pixels[((y0 + y) * w as usize + x0 + x) * bands + band] =
                                *bytes.get(at).ok_or_else(|| Error::Invalid("truncated planar TIFF buffer".into()))?;
                        }
                    }
                    continue;
                }
                let data = match decoder.read_chunk(index as u32).map_err(invalid)? {
                    DecodingResult::U8(data) => data,
                    DecodingResult::U16(data) if bits == 16 => data.into_iter().map(|v| (v >> 8) as u8).collect(),
                    _ => return Err(Error::Invalid("invalid planar TIFF sample type".into())),
                };
                let (x0, y0) = ((chunk % across) * cw as usize, (chunk / across) * ch as usize);
                for y in 0..(dh as usize).min(h as usize - y0) {
                    for x in 0..(dw as usize).min(w as usize - x0) {
                        pixels[((y0 + y) * w as usize + x0 + x) * bands + band] = data[y * dw as usize + x];
                    }
                }
            }
        }
        pixels
    } else {
        match decoder.read_image().map_err(invalid)? {
            DecodingResult::U8(data) => data,
            DecodingResult::U16(data) if bits == 16 => data.into_iter().map(|v| (v >> 8) as u8).collect(),
            _ => return Err(Error::Invalid("unsupported TIFF display samples".into())),
        }
    };
    let raw = if bits < 8 {
        let stride = (w as usize * bands * bits as usize).div_ceil(8);
        let mask = (1u8 << bits) - 1;
        let mut pixels = Vec::with_capacity(w as usize * h as usize * bands);
        for row in raw.chunks_exact(stride) {
            for x in 0..w as usize * bands {
                let byte = row[x * bits as usize / 8];
                let byte = if fill == 2 && compression == 1 { byte.reverse_bits() } else { byte };
                let index = (byte >> (8 - bits as usize - (x * bits as usize) % 8)) & mask;
                pixels.push(if photo == 3 { index } else { index * (255 / mask) });
            }
        }
        pixels
    } else if fill == 2 && compression == 1 && bits == 8 {
        raw.into_iter().map(u8::reverse_bits).collect()
    } else {
        raw
    };
    let expected = w as usize * h as usize * bands;
    if raw.len() != expected {
        return Err(Error::Invalid("invalid TIFF display sample count".into()));
    }
    let buffer_error = || Error::Invalid("invalid TIFF display buffer".into());
    let im = if photo == 3 {
        let n = 1usize << bits;
        if palette.len() != n * 3 || bands != 1 {
            return Err(buffer_error());
        }
        let pixels =
            raw.into_iter().flat_map(|i| [0, 1, 2].map(|c| (palette[c * n + usize::from(i)] >> 8) as u8)).collect();
        DynamicImage::ImageRgb8(image::RgbImage::from_raw(w, h, pixels).ok_or_else(buffer_error)?)
    } else if photo <= 1 && bands == 1 {
        DynamicImage::ImageLuma8(image::GrayImage::from_raw(w, h, raw).ok_or_else(buffer_error)?)
    } else if photo == 1 && bands == 2 {
        DynamicImage::ImageLumaA8(image::GrayAlphaImage::from_raw(w, h, raw).ok_or_else(buffer_error)?)
    } else if photo == 2 && bands == 3 {
        DynamicImage::ImageRgb8(image::RgbImage::from_raw(w, h, raw).ok_or_else(buffer_error)?)
    } else if (photo == 2 || photo == 5) && bands == 4 {
        let mut raw = raw;
        if photo == 2 && extra.first() == Some(&1) {
            for p in raw.chunks_exact_mut(4) {
                if p[3] == 0 {
                    p[..3].fill(0);
                } else {
                    for c in 0..3 {
                        p[c] = (u32::from(p[c]) * 255 / u32::from(p[3])).min(255) as u8;
                    }
                }
            }
        }
        DynamicImage::ImageRgba8(image::RgbaImage::from_raw(w, h, raw).ok_or_else(buffer_error)?)
    } else {
        return Err(Error::Invalid("unsupported TIFF display mode".into()));
    };
    Ok((im, photo == 5, photo == 3 || bits == 1))
}
fn palette_as_gray(bytes: &mut [u8]) -> Result<()> {
    set_tiff_uint(bytes, 262, 1)
}
fn set_tiff_uint(bytes: &mut [u8], tag: u16, value: u32) -> Result<()> {
    let big = bytes.starts_with(b"MM");
    let read = |data: &[u8]| -> u64 {
        if big {
            data.iter().fold(0, |n, b| (n << 8) | u64::from(*b))
        } else {
            data.iter().rev().fold(0, |n, b| (n << 8) | u64::from(*b))
        }
    };
    let bad = || Error::Invalid("invalid TIFF palette directory".into());
    let version = bytes.get(2..4).map(read).ok_or_else(bad)?;
    let (pointer, count_bytes, entry_bytes, value_offset) = match version {
        42 => (4..8, 2, 12, 8),
        43 => (8..16, 8, 20, 12),
        _ => return Err(bad()),
    };
    let offset = usize::try_from(read(bytes.get(pointer).ok_or_else(bad)?)).map_err(|_| bad())?;
    let start = offset.checked_add(count_bytes).ok_or_else(bad)?;
    let count = usize::try_from(read(bytes.get(offset..start).ok_or_else(bad)?)).map_err(|_| bad())?;
    let length = count.checked_mul(entry_bytes).ok_or_else(bad)?;
    let end = start.checked_add(length).ok_or_else(bad)?;
    let directory = bytes.get_mut(start..end).ok_or_else(bad)?;
    for entry in directory.chunks_exact_mut(entry_bytes) {
        if read(&entry[..2]) == u64::from(tag) {
            if !matches!(read(&entry[2..4]), 3 | 4) || read(&entry[4..value_offset]) != 1 {
                return Err(bad());
            }
            entry[2..4].copy_from_slice(if big { &[0, 4] } else { &[4, 0] });
            entry[value_offset..].fill(0);
            entry[value_offset..value_offset + 4].copy_from_slice(&if big {
                value.to_be_bytes()
            } else {
                value.to_le_bytes()
            });
            return Ok(());
        }
    }
    Err(bad())
}

fn limit(w: u32, h: u32) -> std::io::Result<()> {
    // Pillow's Image.DecompressionBombError (2 * its default MAX_IMAGE_PIXELS).
    if u64::from(w) * u64::from(h) > 178_956_970 {
        Err(std::io::Error::other("image exceeds decompression limit"))
    } else {
        Ok(())
    }
}
fn tiff_path_mapped(metadata: Option<&exif::Exif>) -> bool {
    let Some(metadata) = metadata else {
        return false;
    };
    let field = |number| metadata.get_field(exif::Tag(exif::Context::Tiff, number), exif::In::PRIMARY);
    let uint = |number| field(number).and_then(|f| f.value.get_uint(0));
    let single_strip = field(0x111).is_some_and(|f| f.value.get_uint(0).is_some() && f.value.get_uint(1).is_none());
    if uint(0x103).unwrap_or(1) != 1 || !single_strip || uint(0x11c).unwrap_or(1) != 1 {
        return false;
    }
    let (photo, bits, samples) = (uint(0x106), uint(0x102), uint(0x115).unwrap_or(1));
    // Exactly Pillow's rawmode==mode and _MAPMODES qualification, not all TIFFs.
    (photo == Some(1) && samples == 1 && matches!(bits, Some(8 | 16)) && uint(0x153).unwrap_or(1) == 1)
        || (photo == Some(3) && samples == 1 && bits == Some(8))
        || (photo == Some(2) && samples == 4 && bits == Some(8) && uint(0x152) == Some(2))
        || (photo == Some(5) && samples == 4 && bits == Some(8))
}
fn reshape_image(im: DynamicImage) -> Result<DynamicImage> {
    macro_rules! reshape {
        ($variant:ident,$g:ident) => {
            image::ImageBuffer::from_raw($g.height(), $g.width(), $g.into_raw()).map(DynamicImage::$variant)
        };
    }
    let im = match im {
        DynamicImage::ImageLuma8(g) => reshape!(ImageLuma8, g),
        DynamicImage::ImageLuma16(g) => reshape!(ImageLuma16, g),
        DynamicImage::ImageRgb8(g) => reshape!(ImageRgb8, g),
        DynamicImage::ImageRgb16(g) => reshape!(ImageRgb16, g),
        DynamicImage::ImageRgba8(g) => reshape!(ImageRgba8, g),
        _ => None,
    };
    im.ok_or_else(|| Error::Invalid("unsupported TIFF mapped mode".into()))
}
fn tga_header(header: &[u8]) -> bool {
    header.len() >= 18
        && matches!(header[1], 0 | 1)
        && matches!(header[2], 1 | 2 | 3 | 9 | 10 | 11)
        && matches!(header[16], 1 | 8 | 16 | 24 | 32)
        && header[12..14] != [0, 0]
        && header[14..16] != [0, 0]
}
// Preserve Pillow's accepted raw modes missing from image's TGA adapter.
// Adapt samples to an already supported header; that decoder owns orientation/RLE.
fn display_tga(mut bytes: Vec<u8>) -> Result<DynamicImage> {
    let bad = || Error::Invalid("invalid TGA samples".into());
    if !tga_header(&bytes) {
        return Err(bad());
    }
    let w = u32::from(u16::from_le_bytes([bytes[12], bytes[13]]));
    let h = u32::from(u16::from_le_bytes([bytes[14], bytes[15]]));
    limit(w, h)?;
    let kind = bytes[2];
    let depth = bytes[16];
    // Pillow BGRA;15Z expands 5-bit values by floor(255*v/31); its inverted
    // high bit is alpha (Unpack.c: clear → 255, set → 0).
    let expand = |sample: &[u8]| {
        let v = u16::from_le_bytes([sample[0], sample[1]]);
        let alpha = if v & 0x8000 == 0 { 255 } else { 0 };
        [(v & 31) as u32 * 255 / 31, ((v >> 5) & 31) as u32 * 255 / 31, ((v >> 10) & 31) as u32 * 255 / 31, alpha]
            .map(|v| v as u8)
    };
    if matches!(kind, 1 | 9) && bytes[1] == 1 {
        // Palette alpha belongs to the entries, not the image descriptor.
        // Pillow ignores these low bits for P; image's adapter requires zero.
        bytes[17] &= 0xf0;
    }
    if bytes[1] == 1 && bytes[7] == 16 {
        let palette_start = 18 + usize::from(bytes[0]);
        let entries = usize::from(u16::from_le_bytes([bytes[5], bytes[6]]));
        let palette_end = palette_start + entries * 2;
        let palette = bytes.get(palette_start..palette_end).ok_or_else(bad)?;
        let expanded: Vec<u8> = palette.chunks_exact(2).flat_map(expand).collect();
        bytes.splice(palette_start..palette_end, expanded);
        bytes[7] = 32;
    }
    let start = 18
        + usize::from(bytes[0])
        + if bytes[1] == 1 {
            usize::from(u16::from_le_bytes([bytes[5], bytes[6]])) * usize::from(bytes[7]).div_ceil(8)
        } else {
            0
        };
    if matches!(kind, 3 | 11) && depth == 16 {
        bytes[17] = bytes[17] & 0xf0 | 8;
    } else if kind == 3 && depth == 1 {
        let stride = (w as usize).div_ceil(8);
        let data = bytes.get(start..start + stride * h as usize).ok_or_else(bad)?;
        let pixels: Vec<_> = data
            .chunks_exact(stride)
            .flat_map(|row| (0..w as usize).map(move |x| if row[x / 8] & (128 >> (x % 8)) != 0 { 255 } else { 0 }))
            .collect();
        bytes.truncate(start);
        bytes[16] = 8;
        bytes.extend(pixels);
    } else if matches!(kind, 2 | 10) && depth == 16 {
        let mut output = bytes.get(..start).ok_or_else(bad)?.to_vec();
        let count = w as usize * h as usize;
        let (mut at, mut done) = (start, 0);
        while done < count {
            let (samples, run) = if kind == 10 {
                let packet = *bytes.get(at).ok_or_else(bad)?;
                at += 1;
                output.push(packet);
                (usize::from(packet & 127) + 1, packet & 128 != 0)
            } else {
                (count, false)
            };
            if samples > count - done {
                return Err(bad());
            }
            for _ in 0..if run { 1 } else { samples } {
                output.extend(expand(bytes.get(at..at + 2).ok_or_else(bad)?));
                at += 2;
            }
            done += samples;
        }
        output[16] = 32;
        output[17] = output[17] & 0xf0 | 8;
        bytes = output;
    }
    let flip = bytes[17] & 16 != 0;
    let mut im = image::load_from_memory_with_format(&bytes, image::ImageFormat::Tga)
        .map_err(|e| Error::Invalid(e.to_string()))?;
    if flip {
        im = im.fliph();
    }
    Ok(im)
}
fn display_ico(bytes: &[u8], bound: usize) -> Result<(DynamicImage, bool, bool, IntegerMode)> {
    let bad = || Error::Invalid("invalid ICO directory".into());
    let count = bytes.get(4..6).map(|v| usize::from(u16::from_le_bytes([v[0], v[1]]))).ok_or_else(bad)?;
    let end = 6 + count * 16;
    let entries = bytes.get(6..end).ok_or_else(bad)?;
    let size = |value: u8| if value == 0 { 256u32 } else { u32::from(value) };
    let depth = |entry: &[u8]| {
        let bpp = u16::from_le_bytes([entry[6], entry[7]]);
        if bpp != 0 {
            u32::from(bpp)
        } else if entry[2] == 0 {
            256
        } else {
            8 - entry[2].saturating_sub(1).leading_zeros()
        }
    };
    // Pillow sorts stable: largest area first, then *lowest* colour depth.
    // image's ICO adapter instead prioritises highest depth before area.
    let entry = entries
        .chunks_exact(16)
        .enumerate()
        .min_by_key(|(i, e)| (std::cmp::Reverse(size(e[0]) * size(e[1])), depth(e), *i))
        .map(|(_, e)| e)
        .ok_or_else(bad)?;
    let u32_at = |start| u32::from_le_bytes(entry[start..start + 4].try_into().expect("fixed ICO entry")) as usize;
    let (offset, length) = (u32_at(12), u32_at(8));
    let end = offset.checked_add(length).ok_or_else(bad)?;
    let mut payload = bytes.get(offset..end).ok_or_else(bad)?;
    if payload.starts_with(b"\x89PNG\r\n\x1a\n") {
        // Accept Pillow's original RGB/L/P PNG modes; image's ICO wrapper
        // otherwise refuses any embedded PNG that isn't RGBA.
        return decode_reader(Cursor::new(payload), bound, Some(image::ImageFormat::Png), false, false);
    }
    if u16::from_le_bytes([entry[6], entry[7]]) == 32 && payload.len() >= 40 {
        // Pillow uses the stored fourth byte, ignoring AND bits for 32bpp.
        // Some valid 32-bit icons have no AND mask (including Pillow's encoder).
        let field = |at| u32::from_le_bytes(payload[at..at + 4].try_into().expect("DIB header")) as usize;
        let header_size = field(0);
        if header_size >= 40 && field(16) == 0 {
            let xor_size = (size(entry[0]) as usize)
                .checked_mul(size(entry[1]) as usize)
                .and_then(|n| n.checked_mul(4))
                .ok_or_else(bad)?;
            let palette_size = field(32).checked_mul(4).ok_or_else(bad)?;
            let xor_end =
                header_size.checked_add(palette_size).and_then(|n| n.checked_add(xor_size)).ok_or_else(bad)?;
            payload = payload.get(..xor_end).ok_or_else(bad)?;
        }
    }
    let mut one = vec![0, 0, 1, 0, 1, 0];
    one.extend_from_slice(entry);
    one[14..18].copy_from_slice(&(payload.len() as u32).to_le_bytes());
    one[18..22].copy_from_slice(&22u32.to_le_bytes());
    one.extend_from_slice(payload);
    let mut reader = ImageReader::with_format(Cursor::new(one), image::ImageFormat::Ico);
    reader.no_limits();
    let decoder = reader.into_decoder().map_err(|e| Error::Invalid(e.to_string()))?;
    let (w, h) = image::ImageDecoder::dimensions(&decoder);
    limit(w, h)?;
    let image = DynamicImage::from_decoder(decoder).map_err(|e| Error::Invalid(e.to_string()))?;
    Ok((image, false, false, IntegerMode::U16))
}

fn gif_first_canvas<R: Read>(reader: &mut R) -> std::io::Result<(u32, u32, u32, u32, [u8; 4])> {
    let mut header = [0; 13];
    reader.read_exact(&mut header)?;
    let palette = |reader: &mut R, packed: u8| -> std::io::Result<Option<Vec<u8>>> {
        if packed & 128 == 0 {
            return Ok(None);
        }
        let length = 3 * (2usize << (packed & 7));
        let mut bytes = vec![0; length];
        reader.read_exact(&mut bytes)?;
        Ok(Some(bytes))
    };
    let global = palette(reader, header[10])?;
    let mut transparent = None;
    let byte = |reader: &mut R| -> std::io::Result<u8> {
        let mut b = [0];
        reader.read_exact(&mut b)?;
        Ok(b[0])
    };
    loop {
        match byte(reader)? {
            0x21 => {
                let label = byte(reader)?;
                loop {
                    let n = byte(reader)? as usize;
                    if n == 0 {
                        break;
                    }
                    let mut data = [0; 255];
                    reader.read_exact(&mut data[..n])?;
                    if label == 0xf9 && n == 4 {
                        transparent = (data[0] & 1 != 0).then_some(data[3]);
                    }
                }
            }
            0x2c => {
                let mut d = [0; 9];
                reader.read_exact(&mut d)?;
                let local = palette(reader, d[8])?;
                // Pillow fills the first P-mode canvas with the transparent
                // index when present, otherwise index zero (not GIF background).
                let index = usize::from(transparent.unwrap_or(0));
                let palette = local.or(global);
                let c = palette.as_ref().and_then(|p| p.get(index * 3..index * 3 + 3)).unwrap_or(&[0; 3]);
                let coord = |i| u32::from(u16::from_le_bytes([d[i], d[i + 1]]));
                return Ok((
                    coord(0),
                    coord(2),
                    coord(4),
                    coord(6),
                    [c[0], c[1], c[2], if transparent.is_some() { 0 } else { 255 }],
                ));
            }
            _ => return Err(std::io::Error::other("GIF first frame missing")),
        }
    }
}
fn bmp_nearest(header: &[u8]) -> bool {
    if !header.starts_with(b"BM") {
        return false;
    }
    let u32_at = |at| header.get(at..at + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]) as usize);
    let Some(dib) = u32_at(14) else {
        return false;
    };
    let at = if dib == 12 { 24 } else { 28 };
    let Some(bits) = header.get(at..at + 2).map(|s| u16::from_le_bytes([s[0], s[1]])) else {
        return false;
    };
    if ![1, 4, 8].contains(&bits) {
        return false;
    }
    let colors = if dib == 12 { 1usize << bits } else { u32_at(46).filter(|n| *n != 0).unwrap_or(1usize << bits) };
    let padding = if dib == 12 { 3 } else { 4 };
    let Some(start) = 14usize.checked_add(dib) else {
        return true;
    };
    // Pillow BmpImagePlugin treats an identity gray palette as L (BICUBIC),
    // except the black/white two-entry palette, which becomes mode 1 (NEAREST).
    let identity = colors <= 256
        && (0..colors).all(|i| {
            let value = if colors == 2 { i * 255 } else { i } as u8;
            header.get(start + i * padding..start + i * padding + 3) == Some(&[value; 3])
        });
    !identity || colors == 2
}
fn cmyk_rgb(raw: &[u8]) -> Vec<u8> {
    raw.chunks_exact(4)
        .flat_map(|p| {
            [0, 1, 2].map(|c| {
                let nk = 255 - u32::from(p[3]);
                (nk - (u32::from(p[c]) * nk + 127) / 255) as u8
            })
        })
        .collect()
}
fn flatten(im: &DynamicImage, cmyk: bool) -> Vec<u8> {
    if cmyk {
        cmyk_rgb(im.as_bytes())
    } else if im.color().has_alpha() {
        im.to_rgba8()
            .pixels()
            .flat_map(|p| {
                [0, 1, 2].map(|c| {
                    ((u32::from(p[c]) * u32::from(p[3]) + [244u32, 244, 242][c] * (255 - u32::from(p[3])) + 127) / 255)
                        as u8
                })
            })
            .collect()
    } else {
        pillow_rgb(im)
    }
}
fn pillow_rgb(im: &DynamicImage) -> Vec<u8> {
    // Pillow converts integer grayscale by clamping, after any thumbnail resize.
    if let DynamicImage::ImageLuma16(g) = im {
        g.pixels().flat_map(|p| [p[0].min(255) as u8; 3]).collect()
    } else {
        im.to_rgb8().into_raw()
    }
}
pub fn signature(path: &Path) -> Result<(f64, Vec<u8>)> {
    let (im, _, cmyk, _) = decode(path, 128)?;
    Ok(signature_of(&im, cmyk))
}
/// #153's already-authorized public thumbnail bytes, without a temporary file.
pub fn signature_bytes(bytes: &[u8]) -> Result<(f64, Vec<u8>)> {
    let (im, _, cmyk, _) = decode_reader(Cursor::new(bytes), 128, None, true, false)?;
    Ok(signature_of(&im, cmyk))
}
fn signature_of(im: &DynamicImage, cmyk: bool) -> (f64, Vec<u8>) {
    let size = (im.width() as usize, im.height() as usize);
    let rgb = flatten(im, cmyk);
    let gray: Vec<_> = rgb
        .chunks_exact(3)
        .map(|p| ((u32::from(p[0]) * 19595 + u32::from(p[1]) * 38470 + u32::from(p[2]) * 7471 + 32768) >> 16) as u8)
        .collect();
    (size.0 as f64 / size.1 as f64, resize_bytes(&gray, size, (16, 16), 1, Filter::Box, (size.0 as f64, size.1 as f64)))
}
/// Existing provenance.verify._colour: draft96, drop alpha, no EXIF transform,
/// 24x24 RGB BOX. Deliberately distinct from displayed-image signature/load.
pub fn reference_colour(path: &Path) -> Result<Vec<u8>> {
    let (im, _, cmyk, _) = decode_reader(
        BufReader::new(fs::File::open(path)?),
        96,
        image::ImageFormat::from_path(path).ok(),
        false,
        true,
    )?;
    Ok(colour_of(&im, cmyk))
}
pub fn reference_colour_bytes(bytes: &[u8]) -> Result<Vec<u8>> {
    let (im, _, cmyk, _) = decode_reader(Cursor::new(bytes), 96, None, false, false)?;
    Ok(colour_of(&im, cmyk))
}
fn colour_of(im: &DynamicImage, cmyk: bool) -> Vec<u8> {
    let size = (im.width() as usize, im.height() as usize);
    let rgb = if cmyk { cmyk_rgb(im.as_bytes()) } else { pillow_rgb(im) };
    resize_bytes(&rgb, size, (24, 24), 3, Filter::Box, (size.0 as f64, size.1 as f64))
}
fn thumbnail_size(size: (usize, usize)) -> (usize, usize) {
    let (w, h) = size;
    if w <= 1024 && h <= 1024 {
        size
    } else {
        let aspect = w as f64 / h as f64;
        if w <= h {
            let v = 1024. * aspect;
            let n = if (aspect - v.floor() / 1024.).abs() <= (aspect - v.ceil() / 1024.).abs() {
                v.floor()
            } else {
                v.ceil()
            };
            (n.max(1.) as usize, 1024)
        } else {
            let v = 1024. / aspect;
            let err = |n: f64| if n == 0. { 0. } else { (aspect - 1024. / n).abs() };
            let n = if err(v.floor()) <= err(v.ceil()) { v.floor() } else { v.ceil() };
            (1024, n.max(1.) as usize)
        }
    }
}
pub fn load(path: &Path) -> Result<Loaded> {
    if let Some(scalar) = scalar_path(path)? {
        return match scalar {
            ScalarInput::Shared(scalar) => load_scalar(scalar),
            ScalarInput::Extra(bytes) => {
                let (im, size) = extra_scalar_image(bytes, true, true)?;
                let pixels = im.to_rgb8().into_raw().into_iter().map(f32::from).collect();
                Ok(Loaded { image: Plane::new(im.width() as usize, im.height() as usize, 3, pixels)?, size })
            }
        };
    }
    let (im, nearest, cmyk, integer_mode) = decode(path, 1024)?;
    let size = (im.width() as usize, im.height() as usize);
    let (w, h) = size;
    let out = thumbnail_size(size);
    if let DynamicImage::ImageLuma16(g) = &im {
        if matches!(integer_mode, IntegerMode::I32) {
            let raw = thumbnail_i32(g.as_raw().iter().map(|v| i32::from(*v)).collect(), size, out);
            let pixels = raw.into_iter().flat_map(|p| [p.clamp(0, 255) as f32; 3]).collect();
            return Ok(Loaded { image: Plane::new(out.0, out.1, 3, pixels)?, size });
        }
        // Pillow's I;16 has a distinct double-coefficient, per-axis uint16
        // resampler. It cannot use the RGB byte path before resize/clamping.
        if (w / out.0 / 2).max(1) > 1 || (h / out.1 / 2).max(1) > 1 {
            // thumbnail's reducing_gap attempts Image.reduce, which rejects I;16.
            return Err(Error::Invalid(WRONG_MODE.into()));
        }
        let raw = if matches!(integer_mode, IntegerMode::U16Big) && size != out {
            // Pillow's I;16B resampler reads/writes native uint16 storage while
            // the mode retains big endian interpretation. Preserve that old
            // byte ordering, not an invented endian-corrected thumbnail.
            let native: Vec<_> = g.as_raw().iter().map(|v| v.swap_bytes()).collect();
            resize_u16(&native, size, out).into_iter().map(u16::swap_bytes).collect()
        } else {
            resize_u16(g.as_raw(), size, out)
        };
        let pixels = raw.into_iter().flat_map(|p| [f32::from(p.min(255)); 3]).collect();
        return Ok(Loaded { image: Plane::new(out.0, out.1, 3, pixels)?, size });
    }
    let alpha = im.color().has_alpha() && !cmyk;
    let channels = if alpha || cmyk { 4 } else { 3 };
    let mut raw = if channels == 4 { im.to_rgba8().into_raw() } else { im.to_rgb8().into_raw() };
    let rgb = if size == out {
        flatten(&im, cmyk)
    } else if nearest {
        let pixels: Vec<_> = (0..out.1)
            .flat_map(|y| {
                (0..out.0).flat_map({
                    let raw = &raw;
                    move |x| {
                        let sx = ((x as f64 + 0.5) * w as f64 / out.0 as f64) as usize;
                        let sy = ((y as f64 + 0.5) * h as f64 / out.1 as f64) as usize;
                        raw[(sy * w + sx) * channels..(sy * w + sx + 1) * channels].iter().copied()
                    }
                })
            })
            .collect();
        composite(pixels, alpha)
    } else {
        // Pillow RGBA.resize first premultiplies; this recursive resize omits reducing_gap.
        if alpha {
            for p in raw.chunks_exact_mut(4) {
                for c in 0..3 {
                    p[c] = ((u32::from(p[c]) * u32::from(p[3]) + 127) / 255) as u8;
                }
            }
        }
        let mut size = size;
        let mut extent = (w as f64, h as f64);
        if !alpha {
            let (fx, fy) = ((w / out.0 / 2).max(1), (h / out.1 / 2).max(1));
            if fx > 1 || fy > 1 {
                let (rw, rh) = (w.div_ceil(fx), h.div_ceil(fy));
                let mut reduced = vec![0; rw * rh * channels];
                for y in 0..rh {
                    for x in 0..rw {
                        let (x1, y1) = (((x + 1) * fx).min(w), ((y + 1) * fy).min(h));
                        let n = (x1 - x * fx) * (y1 - y * fy);
                        // Pillow 12.3 Reduce.c: float32 reciprocal, truncated
                        // UINT32 multiplier and wrapped product, not exact division.
                        let multiplier = ((1u64 << 32) as f32 / (n as u32).wrapping_mul(256) as f32) as u32;
                        for c in 0..channels {
                            let mut s = 0u64;
                            for yy in y * fy..y1 {
                                for xx in x * fx..x1 {
                                    s += u64::from(raw[(yy * w + xx) * channels + c]);
                                }
                            }
                            let amended = (s as u32).wrapping_add(n as u32 / 2);
                            reduced[(y * rw + x) * channels + c] = (amended.wrapping_mul(multiplier) >> 24) as u8;
                        }
                    }
                }
                raw = reduced;
                size = (rw, rh);
                extent = (w as f64 / fx as f64, h as f64 / fy as f64);
            }
        }
        let mut resized = resize_bytes(&raw, size, out, channels, Filter::Cubic, extent);
        if alpha {
            for p in resized.chunks_exact_mut(4) {
                if p[3] != 0 && p[3] != 255 {
                    for c in 0..3 {
                        p[c] = (u32::from(p[c]) * 255 / u32::from(p[3])).min(255) as u8;
                    }
                }
            }
        }
        if cmyk {
            cmyk_rgb(&resized)
        } else {
            composite(resized, alpha)
        }
    };
    Ok(Loaded { image: Plane::new(out.0, out.1, 3, rgb.into_iter().map(f32::from).collect())?, size })
}
fn composite(raw: Vec<u8>, alpha: bool) -> Vec<u8> {
    if !alpha {
        return raw;
    }
    raw.chunks_exact(4)
        .flat_map(|p| {
            [0, 1, 2].map(|c| {
                ((u32::from(p[c]) * u32::from(p[3]) + [244u32, 244, 242][c] * (255 - u32::from(p[3])) + 127) / 255)
                    as u8
            })
        })
        .collect()
}
fn resize_u16(pixels: &[u16], size: (usize, usize), out: (usize, usize)) -> Vec<u16> {
    resize_integer(pixels, size, out, (size.0 as f64, size.1 as f64), |v| (v + 0.5).trunc().clamp(0., 65535.) as u16)
}
fn store_i32(value: f64) -> i32 {
    let value = value.round();
    // Pillow's MSVC x64 double->INT32 storage returns INT_MIN on overflow;
    // arm64's conversion saturates. Preserve each original platform's pixels.
    #[cfg(target_os = "windows")]
    if value < f64::from(i32::MIN) || value > f64::from(i32::MAX) || !value.is_finite() {
        return i32::MIN;
    }
    value as i32
}
fn thumbnail_i32(pixels: Vec<i32>, size: (usize, usize), out: (usize, usize)) -> Vec<i32> {
    thumbnail_32(pixels, size, out, i32::wrapping_add, store_i32)
}
fn thumbnail_32<T: Copy + Default + Into<f64>>(
    mut pixels: Vec<T>,
    (w, h): (usize, usize),
    out: (usize, usize),
    add: impl Fn(T, T) -> T + Copy,
    quantize: impl Fn(f64) -> T + Copy,
) -> Vec<T> {
    let (fx, fy) = ((w / out.0 / 2).max(1), (h / out.1 / 2).max(1));
    let mut size = (w, h);
    let mut extent = (w as f64, h as f64);
    if fx > 1 || fy > 1 {
        let (rw, rh) = (w.div_ceil(fx), h.div_ceil(fy));
        let mut reduced = vec![T::default(); rw * rh];
        for y in 0..rh {
            for x in 0..rw {
                let (x0, y0) = (x * fx, y * fy);
                let (x1, y1) = (((x + 1) * fx).min(w), ((y + 1) * fy).min(h));
                let mut sum = 0.;
                if x1 - x0 == fx && y1 - y0 == fy {
                    // Original Reduce.c adds each full 2x2 group in its source
                    // type, then promotes to double. Preserve I32 wrap/F32 rounds.
                    for yy in (y0..y1 - 1).step_by(2) {
                        for xx in (x0..x1 - 1).step_by(2) {
                            let group = add(
                                add(add(pixels[yy * w + xx], pixels[yy * w + xx + 1]), pixels[(yy + 1) * w + xx]),
                                pixels[(yy + 1) * w + xx + 1],
                            );
                            sum += group.into();
                        }
                        if fx % 2 != 0 {
                            sum += add(pixels[yy * w + x1 - 1], pixels[(yy + 1) * w + x1 - 1]).into();
                        }
                    }
                    if fy % 2 != 0 {
                        for xx in (x0..x1 - 1).step_by(2) {
                            sum += add(pixels[(y1 - 1) * w + xx], pixels[(y1 - 1) * w + xx + 1]).into();
                        }
                        if fx % 2 != 0 {
                            sum += pixels[(y1 - 1) * w + x1 - 1].into();
                        }
                    }
                } else {
                    // Partial last rows/columns use scalar double addition.
                    for yy in y0..y1 {
                        for xx in x0..x1 {
                            sum += pixels[yy * w + xx].into();
                        }
                    }
                }
                let multiplier = 1. / ((x1 - x0) * (y1 - y0)) as f64;
                reduced[y * rw + x] = quantize(sum * multiplier);
            }
        }
        pixels = reduced;
        size = (rw, rh);
        extent = (w as f64 / fx as f64, h as f64 / fy as f64);
    }
    resize_integer(&pixels, size, out, extent, quantize)
}
fn resize_integer<T: Copy + Default + Into<f64>>(
    pixels: &[T],
    (w, h): (usize, usize),
    (ow, oh): (usize, usize),
    extent: (f64, f64),
    quantize: impl Fn(f64) -> T + Copy,
) -> Vec<T> {
    if vertical_first(w, h, oh) {
        let swapped =
            resize_integer(&transpose_pixels(pixels, w, h, 1), (h, w), (oh, ow), (extent.1, extent.0), quantize);
        return transpose_pixels(&swapped, oh, ow, 1);
    }
    let horizontal = if w == ow && extent.0 == w as f64 {
        pixels.to_vec()
    } else {
        let coeff = coefficients(w, ow, Filter::Cubic, extent.0);
        let mut dst = vec![T::default(); ow * h];
        for y in 0..h {
            for (x, (start, weights)) in coeff.iter().enumerate() {
                dst[y * ow + x] = quantize(
                    weights
                        .iter()
                        .enumerate()
                        .fold(0., |s, (j, k)| resample_add(pixels[y * w + start + j].into(), *k, s)),
                );
            }
        }
        dst
    };
    if h == oh && extent.1 == h as f64 {
        return horizontal;
    }
    let coeff = coefficients(h, oh, Filter::Cubic, extent.1);
    let mut dst = vec![T::default(); ow * oh];
    for (y, (start, weights)) in coeff.iter().enumerate() {
        for x in 0..ow {
            dst[y * ow + x] = quantize(
                weights
                    .iter()
                    .enumerate()
                    .fold(0., |s, (j, k)| resample_add(horizontal[(start + j) * ow + x].into(), *k, s)),
            );
        }
    }
    dst
}

#[cfg(test)]
mod integer_storage_tests {
    use super::store_i32;
    #[test]
    fn original_platform_i32_storage_at_cubic_overshoot() {
        assert_eq!(store_i32(127.5), 128);
        assert_eq!(store_i32(-127.5), -128);
        assert_eq!(store_i32(-2_399_141_888.), i32::MIN);
        let positive = if cfg!(target_os = "windows") { i32::MIN } else { i32::MAX };
        // Values measured independently from Pillow on both platforms before fix.
        for value in [2_161_663_712., 2_250_244_096., 2_304_770_048.] {
            assert_eq!(store_i32(value), positive);
        }
    }
}
