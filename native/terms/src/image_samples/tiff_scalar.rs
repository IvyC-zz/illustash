//! 原始标量 TIFF 模式，之后按 Pillow convert('RGB') 截断，并按 load_end 应用方向。
use crate::{Error, Result};
use image::{Rgba, RgbaImage};
use std::io::Cursor;
use tiff::{
    decoder::{Decoder, DecodingResult},
    tags::Tag,
    ColorType,
};

fn invalid(e: impl std::fmt::Display) -> Error {
    Error::Invalid(e.to_string())
}

/// Pillow 的标量模式，与按本机字节序解码出的采样分开保存。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ScalarMode {
    Unsigned16Le, // I;16
    Unsigned16Be, // I;16B
    Signed32,     // I
    Float32,      // F
}

#[derive(Debug)]
pub(crate) enum ScalarSamples {
    U16(Vec<u16>),
    I32(Vec<i32>),
    F32(Vec<f32>),
}
impl ScalarSamples {
    fn len(&self) -> usize {
        match self {
            Self::U16(values) => values.len(),
            Self::I32(values) => values.len(),
            Self::F32(values) => values.len(),
        }
    }
}

/// 编码布局的事实；这里不推断 Pillow 解码器或消费方的策略。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ScalarLayout {
    pub(crate) tiled: bool,
    pub(crate) planar_configuration: u16,
    pub(crate) rows_per_strip: u32,
    pub(crate) strip_count: Option<usize>,
    pub(crate) predictor: u16,
}

#[derive(Debug)]
/// 编码的尺寸和数值采样，不含 Pillow 路径输入的形状重排、截断、方向、ICC 转换，
/// 也不含各消费方特有的缩放或显示行为。
pub(crate) struct ScalarTiff {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) mode: ScalarMode,
    pub(crate) samples: ScalarSamples,
    pub(crate) orientation: u16,
    pub(crate) icc: Option<Vec<u8>>,
    pub(crate) big_endian: bool,
    pub(crate) compression: u16,
    pub(crate) layout: ScalarLayout,
    /// 已知满足原始单条带、模式匹配的条件；由调用方提供路径或字节。
    pub(crate) raw_path_mmap_eligible: bool,
}

/// 只支持 BlackIsZero 下无符号 16 位、有符号 32 位、32 位浮点这个子集。
/// FillOrder 校正属于解码的一部分。返回的 U16/I32/F32 值不截断；ICC 只作为元数据。
/// 其他 TIFF 类型和布局仍被拦住。
pub(crate) fn scalar_samples(bytes: &[u8]) -> Result<ScalarTiff> {
    let mut decoder = Decoder::new(Cursor::new(bytes)).map_err(invalid)?;
    let (width, height) = decoder.dimensions().map_err(invalid)?;
    let colour = decoder.colortype().map_err(invalid)?;
    let photo: u16 = decoder
        .find_tag_unsigned(Tag::PhotometricInterpretation)
        .map_err(invalid)?
        .ok_or_else(|| invalid("TIFF photometric tag missing"))?;
    let sample: u16 = decoder.find_tag_unsigned(Tag::SampleFormat).map_err(invalid)?.unwrap_or(1);
    if photo != 1 || !matches!((colour, sample), (ColorType::Gray(16), 1) | (ColorType::Gray(32), 2 | 3)) {
        return Err(invalid("TIFF scalar mode remains unaccepted"));
    }
    let mode = match (colour, sample) {
        (ColorType::Gray(16), 1) if bytes.starts_with(b"II") => ScalarMode::Unsigned16Le,
        (ColorType::Gray(16), 1) => ScalarMode::Unsigned16Be,
        (ColorType::Gray(32), 2) => ScalarMode::Signed32,
        (ColorType::Gray(32), 3) => ScalarMode::Float32,
        _ => return Err(invalid("TIFF scalar mode remains unaccepted")),
    };
    let orientation: u16 = decoder.find_tag_unsigned(Tag::Orientation).map_err(invalid)?.unwrap_or(1);
    let compression: u16 = decoder.find_tag_unsigned(Tag::Compression).map_err(invalid)?.unwrap_or(1);
    let big_endian = bytes.starts_with(b"MM");
    let offsets: Option<Vec<u64>> = decoder.find_tag_unsigned_vec(Tag::StripOffsets).map_err(invalid)?;
    let layout = ScalarLayout {
        tiled: decoder.find_tag(Tag::TileOffsets).map_err(invalid)?.is_some(),
        planar_configuration: decoder.find_tag_unsigned(Tag::PlanarConfiguration).map_err(invalid)?.unwrap_or(1),
        rows_per_strip: decoder.find_tag_unsigned(Tag::RowsPerStrip).map_err(invalid)?.unwrap_or(height),
        strip_count: offsets.as_ref().map(Vec::len),
        predictor: decoder.find_tag_unsigned(Tag::Predictor).map_err(invalid)?.unwrap_or(1),
    };
    let fill_order: u16 = decoder.find_tag_unsigned(Tag::FillOrder).map_err(invalid)?.unwrap_or(1);
    // Pillow 唯一的标量 FillOrder2 原始模式是小端的 I;16R。
    // 其他模式以及压缩数据的位序处理，这个候选实现都不接受。
    if fill_order != 1
        && !(fill_order == 2 && bytes.starts_with(b"II") && colour == ColorType::Gray(16) && compression == 1)
    {
        return Err(invalid("TIFF scalar FillOrder mode remains unaccepted"));
    }
    // 原始 U16 只接受普通条带，这样才能确定 Pillow 实际是否走 mmap：
    // 一个原始分块、模式匹配 I;16/I;16B（不是 I;16R）、缓冲区有效。
    let mut mmap_eligible = false;
    if colour == ColorType::Gray(16) && compression == 1 {
        if layout.tiled {
            return Err(invalid("TIFF scalar tiled layout remains unaccepted"));
        }
        let offsets = offsets.ok_or_else(|| invalid("TIFF scalar strip offsets missing"))?;
        let counts: Vec<u64> = decoder
            .find_tag_unsigned_vec(Tag::StripByteCounts)
            .map_err(invalid)?
            .ok_or_else(|| invalid("TIFF scalar strip byte counts missing"))?;
        let rows = layout.rows_per_strip;
        let planar = layout.planar_configuration;
        let strips = if rows == 0 { 0 } else { u64::from(height).div_ceil(u64::from(rows)) };
        if planar != 1 || strips == 0 || offsets.len() as u64 != strips || counts.len() != offsets.len() {
            return Err(invalid("TIFF scalar strip layout remains unaccepted"));
        }
        for (index, (&offset, &count)) in offsets.iter().zip(&counts).enumerate() {
            let strip_rows = u64::from(rows).min(u64::from(height) - index as u64 * u64::from(rows));
            let expected = u64::from(width)
                .checked_mul(strip_rows)
                .and_then(|size| size.checked_mul(2))
                .ok_or_else(|| invalid("TIFF scalar strip dimensions overflow"))?;
            if count != expected || offset.checked_add(count).is_none_or(|end| end > bytes.len() as u64) {
                return Err(invalid("TIFF scalar strip buffer remains unaccepted"));
            }
        }
        mmap_eligible = strips == 1 && fill_order == 1;
    }
    let icc = decoder
        .find_tag(Tag::Unknown(34675))
        .map_err(invalid)?
        .map(|v| v.into_u8_vec())
        .transpose()
        .map_err(invalid)?;
    let samples = match (mode, decoder.read_image().map_err(invalid)?) {
        (ScalarMode::Unsigned16Le | ScalarMode::Unsigned16Be, DecodingResult::U16(mut values)) => {
            // 在消费方截断之前，把存储的每个字节按位反转，保持字内的字节顺序。
            if fill_order == 2 {
                for value in &mut values {
                    *value = value.reverse_bits().rotate_left(8);
                }
            }
            ScalarSamples::U16(values)
        }
        (ScalarMode::Signed32, DecodingResult::I32(values)) => ScalarSamples::I32(values),
        (ScalarMode::Float32, DecodingResult::F32(values)) => ScalarSamples::F32(values),
        _ => return Err(invalid("TIFF scalar buffer type mismatch")),
    };
    let count = (width as usize).checked_mul(height as usize).ok_or_else(|| invalid("TIFF dimensions overflow"))?;
    if width == 0 || height == 0 || count != samples.len() {
        return Err(invalid("TIFF scalar buffer length mismatch"));
    }
    Ok(ScalarTiff {
        width,
        height,
        mode,
        samples,
        orientation,
        icc,
        big_endian,
        compression,
        layout,
        raw_path_mmap_eligible: mmap_eligible,
    })
}

/// 像素身份这个消费方保持它单独验收过的 Pillow RGB/PAM 顺序。
/// 其他消费方必须保持各自的模式、缩放、显示和 ICC 契约。
pub(crate) fn scalar_bytes(bytes: &[u8], path_input: bool) -> Result<(RgbaImage, Option<Vec<u8>>)> {
    let ScalarTiff {
        mut width,
        mut height,
        mode,
        samples,
        orientation,
        icc,
        big_endian,
        compression,
        layout,
        raw_path_mmap_eligible,
    } = scalar_samples(bytes)?;
    // Pillow12.3 的 Deflate 经过 libtiff，输出的是本机字节序的字，但对 MM 仍保留 I;32BS/F;32BF 的 rawmode。
    // 因此单独实测的旧 RGB 转换在小端主机上会把字节反转的 32 位字重新解释。纯采样本身保持正确。
    // 这里只接受实测过的普通 Deflate8 布局；不推断其他编码、分块/平面排列或 predictor 模式。
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
            return Err(invalid("TIFF big-endian compressed scalar layout remains unaccepted"));
        }
        cfg!(target_endian = "little")
    } else {
        false
    };
    // Pillow12.3 对走 mmap 的 I;16/I;16B 路径输入，会在转置之前把标量缓冲区按元数据里的尺寸重排。
    // 字节输入、多条带、I;16R、压缩数据以及 I/F 模式都不会。
    if path_input && raw_path_mmap_eligible && (5..=8).contains(&orientation) {
        std::mem::swap(&mut width, &mut height);
    }
    let gray: Vec<u8> = match (mode, samples) {
        (ScalarMode::Unsigned16Le | ScalarMode::Unsigned16Be, ScalarSamples::U16(values)) => {
            values.into_iter().map(|v| v.min(255) as u8).collect()
        }
        (ScalarMode::Signed32, ScalarSamples::I32(values)) => values
            .into_iter()
            .map(|v| if swap_32 { v.swap_bytes() } else { v })
            .map(|v| v.clamp(0, 255) as u8)
            .collect(),
        // 饱和转换：小数截掉，NaN/-infinity=0，+infinity=255。
        (ScalarMode::Float32, ScalarSamples::F32(values)) => values
            .into_iter()
            .map(|v| if swap_32 { f32::from_bits(v.to_bits().swap_bytes()) } else { v })
            .map(|v| v as u8)
            .collect(),
        _ => return Err(invalid("TIFF scalar buffer type mismatch")),
    };
    let (out_width, out_height) = if (5..=8).contains(&orientation) { (height, width) } else { (width, height) };
    let mut image = RgbaImage::new(out_width, out_height);
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
                _ => (x, y), // Pillow 对未知的 Orientation 值不做变换。
            };
            let v = gray[y as usize * width as usize + x as usize];
            image.put_pixel(dx, dy, Rgba([v, v, v, 255]));
        }
    }
    Ok((image, icc))
}

#[cfg(test)]
mod tests {
    use super::*;

    // 手写的 3x2 BlackIsZero 单条带 TIFF，orientation5，带不透明的 ICC 元数据。
    // 两个图像解码器都不用来生成采样期望值。
    fn fixture(big_endian: bool, bits: u16, sample: u16, fill: u16, compression: u16, payload: &[u8]) -> Vec<u8> {
        let short = |v: u16| if big_endian { v.to_be_bytes() } else { v.to_le_bytes() };
        let long = |v: u32| if big_endian { v.to_be_bytes() } else { v.to_le_bytes() };
        let icc = b"synthetic ICC";
        let icc_offset = 8 + 2 + 13 * 12 + 4;
        let data_offset = icc_offset + icc.len() as u32;
        let tags = [
            (256, 4, 1, 3),
            (257, 4, 1, 2),
            (258, 3, 1, u32::from(bits)),
            (259, 3, 1, u32::from(compression)),
            (262, 3, 1, 1),
            (266, 3, 1, u32::from(fill)),
            (273, 4, 1, data_offset),
            (274, 3, 1, 5),
            (277, 3, 1, 1),
            (278, 4, 1, 2),
            (279, 4, 1, payload.len() as u32),
            (339, 3, 1, u32::from(sample)),
            (34675, 7, icc.len() as u32, icc_offset),
        ];
        let mut bytes = Vec::from(if big_endian { *b"MM" } else { *b"II" });
        bytes.extend(short(42));
        bytes.extend(long(8));
        bytes.extend(short(tags.len() as u16));
        for (tag, kind, count, value) in tags {
            bytes.extend(short(tag));
            bytes.extend(short(kind));
            bytes.extend(long(count));
            if kind == 3 {
                bytes.extend(short(value as u16));
                bytes.extend([0, 0]);
            } else {
                bytes.extend(long(value));
            }
        }
        bytes.extend([0; 4]);
        bytes.extend(icc);
        bytes.extend(payload);
        bytes
    }

    #[test]
    fn pure_samples_keep_encoded_dimensions_mode_icc_and_high_unsigned_values() {
        let words: [u16; 6] = [0, 1, 127, 255, 256, 65535];
        for big in [false, true] {
            let payload: Vec<_> =
                words.iter().flat_map(|v| if big { v.to_be_bytes() } else { v.to_le_bytes() }).collect();
            let decoded = scalar_samples(&fixture(big, 16, 1, 1, 1, &payload)).unwrap();
            assert_eq!((decoded.big_endian, decoded.compression), (big, 1));
            assert_eq!((decoded.width, decoded.height, decoded.orientation), (3, 2, 5));
            assert_eq!(decoded.mode, if big { ScalarMode::Unsigned16Be } else { ScalarMode::Unsigned16Le });
            assert_eq!(decoded.icc.as_deref(), Some(b"synthetic ICC".as_slice()));
            assert!(decoded.raw_path_mmap_eligible);
            assert!(matches!(decoded.samples, ScalarSamples::U16(values) if values == words));
        }
        let payload: Vec<_> = words.iter().flat_map(|v| v.to_le_bytes()).collect();
        let decoded = scalar_samples(&fixture(false, 16, 1, 2, 1, &payload)).unwrap();
        assert_eq!((decoded.width, decoded.height, decoded.orientation), (3, 2, 5));
        assert_eq!(decoded.mode, ScalarMode::Unsigned16Le);
        assert!(!decoded.raw_path_mmap_eligible);
        assert!(matches!(decoded.samples, ScalarSamples::U16(values) if values == [0, 128, 254, 255, 32768, 65535]));
    }

    #[test]
    fn pure_samples_keep_signed_extremes_float_bits_and_original_order() {
        let signed = [i32::MIN, -1, 0, 1, 255, i32::MAX];
        let floats = [f32::NAN, f32::NEG_INFINITY, f32::INFINITY, -0.25, 0.75, 127.75];
        for big in [false, true] {
            let payload: Vec<_> =
                signed.iter().flat_map(|v| if big { v.to_be_bytes() } else { v.to_le_bytes() }).collect();
            let decoded = scalar_samples(&fixture(big, 32, 2, 1, 1, &payload)).unwrap();
            assert_eq!((decoded.width, decoded.height, decoded.orientation), (3, 2, 5));
            assert_eq!(decoded.mode, ScalarMode::Signed32);
            assert!(!decoded.raw_path_mmap_eligible);
            assert!(matches!(decoded.samples, ScalarSamples::I32(values) if values == signed));
            let payload: Vec<_> =
                floats.iter().flat_map(|v| if big { v.to_be_bytes() } else { v.to_le_bytes() }).collect();
            let decoded = scalar_samples(&fixture(big, 32, 3, 1, 1, &payload)).unwrap();
            assert_eq!((decoded.width, decoded.height, decoded.orientation), (3, 2, 5));
            assert_eq!(decoded.mode, ScalarMode::Float32);
            assert!(!decoded.raw_path_mmap_eligible);
            let ScalarSamples::F32(values) = decoded.samples else { panic!("float samples required") };
            assert_eq!(
                values.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                floats.iter().map(|v| v.to_bits()).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn deflate_metadata_keeps_correct_numeric_samples_before_consumer_reinterpretation() {
        // 手写的 LE/MM 标量字的 zlib 字面载荷，由独立的 Python struct/zlib 生成。期望的数值是手写的不变量。
        let signed = [i32::MIN, -1, 0, 127, 256, i32::MAX];
        let float_bits = [0x7fc00000u32, 0xff800000, 0x7f800000, 0xbe800000, 0x3f400000, 0x42ff8000];
        for (big, sample, hex) in [
            (false, 2, "789c63606068f80f040c40500f2218191880dc7a0063bf0879"),
            (true, 2, "789c6b606060f80f040c1050cfc0c0c8500fe4030062410879"),
            (false, 3, "789c63603850cfc0d0f01f8841f43e0606077b20d70900548e073c"),
            (true, 3, "789cab3fc0c0f0bf8181a11e88f701b1bd030383135000006141073c"),
        ] {
            let payload: Vec<_> =
                (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap()).collect();
            let decoded = scalar_samples(&fixture(big, 32, sample, 1, 8, &payload)).unwrap();
            assert_eq!((decoded.big_endian, decoded.compression), (big, 8));
            assert_eq!((decoded.width, decoded.height, decoded.orientation), (3, 2, 5));
            assert_eq!(
                decoded.layout,
                ScalarLayout {
                    tiled: false,
                    planar_configuration: 1,
                    rows_per_strip: 2,
                    strip_count: Some(1),
                    predictor: 1
                }
            );
            assert!(!decoded.raw_path_mmap_eligible);
            match decoded.samples {
                ScalarSamples::I32(values) => assert_eq!(values, signed),
                ScalarSamples::F32(values) => {
                    assert_eq!(values.iter().map(|v| v.to_bits()).collect::<Vec<_>>(), float_bits)
                }
                _ => panic!("32-bit samples required"),
            }
        }
    }
}
