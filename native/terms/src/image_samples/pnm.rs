//! Raw PNM samples for image comparison; no RGB clamp or thumbnail here.
use crate::{Error, Result};
use image::ImageDecoder;
use std::io::Cursor;

pub(crate) struct PnmSamples {
    pub width: u32,
    pub height: u32,
    pub bands: usize,
    pub maxval: u32,
    pub samples: Vec<u32>,
}
pub(crate) fn pnm_samples(bytes: &[u8], maximum_pixels: Option<u64>) -> Result<Option<PnmSamples>> {
    use image::codecs::pnm::{PnmDecoder, PnmSubtype, SampleEncoding};
    let decoder = PnmDecoder::new(Cursor::new(bytes)).map_err(|e| Error::Invalid(e.to_string()))?;
    let (width, height) = decoder.dimensions();
    let (reader, header) = decoder.into_inner();
    if maximum_pixels.is_some_and(|max| u64::from(width) * u64::from(height) > max) {
        return Err(Error::Invalid("image exceeds decompression limit".into()));
    }
    let (bands, encoding) = match header.subtype() {
        PnmSubtype::Pixmap(encoding) => (3usize, encoding),
        PnmSubtype::Graymap(encoding) => (1usize, encoding),
        PnmSubtype::Bitmap(_) => return Ok(None),
        // Pillow doesn't accept P7/PAM as a compressed input image.
        PnmSubtype::ArbitraryMap => return Err(Error::Invalid("像素哈希 P7 输入未被旧解码器支持".into())),
    };
    let maxval = header.maximal_sample();
    let count = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(bands))
        .ok_or_else(|| Error::Invalid("PNM 像素长度溢出".into()))?;
    let data = &bytes[reader.position() as usize..];
    let mut samples = Vec::new();
    samples.try_reserve(count).map_err(|e| Error::Invalid(e.to_string()))?;
    match encoding {
        SampleEncoding::Binary => {
            let size = if maxval > 255 { 2 } else { 1 };
            let length = count.checked_mul(size).ok_or_else(|| Error::Invalid("PNM 像素长度溢出".into()))?;
            let data = data.get(..length).ok_or_else(|| Error::Invalid("PNM 像素被截断".into()))?;
            for chunk in data.chunks_exact(size) {
                samples.push(if size == 1 {
                    u32::from(chunk[0])
                } else {
                    u32::from(u16::from_be_bytes([chunk[0], chunk[1]]))
                });
            }
        }
        SampleEncoding::Ascii => {
            let mut at = 0;
            while samples.len() < count {
                while at < data.len() {
                    if data[at] == b'#' {
                        while at < data.len() && !b"\r\n".contains(&data[at]) {
                            at += 1;
                        }
                    } else if data[at].is_ascii_whitespace() {
                        at += 1;
                    } else {
                        break;
                    }
                }
                let start = at;
                while at < data.len() && !data[at].is_ascii_whitespace() && data[at] != b'#' {
                    at += 1;
                }
                let value = std::str::from_utf8(&data[start..at])
                    .ok()
                    .and_then(|s| s.parse::<u32>().ok())
                    .filter(|&v| v <= maxval)
                    .ok_or_else(|| Error::Invalid("PNM 文本像素无效".into()))?;
                samples.push(value);
            }
        }
    }
    Ok(Some(PnmSamples { width, height, bands, maxval, samples }))
}
