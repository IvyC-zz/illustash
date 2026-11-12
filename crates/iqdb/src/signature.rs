//! Danbooru IQDB 签名：danbooru/iqdb 的 src/haar.cpp 与 src/haar_signature.cpp 的移植。
//! 128×128 拉伸、RGB→YIQ、二维 Haar 变换，每个通道取绝对值最大的 40 个系数位置（按系数正负带符号）和直流分量，
//! 编码成服务器 `hash` 参数接受的 `iqdb_…` 文本。离开本机的只有这段文本，图片本身不出去。
use image::imageops::FilterType;
use image::DynamicImage;

pub const PIXELS: usize = 128;
pub const COEFS: usize = 40;

pub struct Signature {
    pub avgl: [f64; 3],
    pub sig: [[i16; COEFS]; 3],
}

/// Row then column decomposition, unnormalised, with haar.cpp's 0.7071 constant (bit-for-bit the same arithmetic).
#[expect(clippy::approx_constant, reason = "与 haar.cpp 的 0.7071 逐位一致，不能换成 FRAC_1_SQRT_2")]
fn haar2d(a: &mut [f64]) {
    let mut t = [0.0f64; PIXELS / 2];
    for row in 0..PIXELS {
        let i = row * PIXELS;
        let (mut h, mut c) = (PIXELS, 1.0f64);
        while h > 1 {
            let h1 = h >> 1;
            c *= 0.7071;
            for k in 0..h1 {
                let (j2, j21) = (i + 2 * k, i + 2 * k + 1);
                t[k] = (a[j2] - a[j21]) * c;
                a[i + k] = a[j2] + a[j21];
            }
            a[i + h1..i + h].copy_from_slice(&t[..h1]);
            h = h1;
        }
        a[i] *= c;
    }
    for col in 0..PIXELS {
        let (mut h, mut c) = (PIXELS, 1.0f64);
        while h > 1 {
            let h1 = h >> 1;
            c *= 0.7071;
            for k in 0..h1 {
                let (j2, j21) = (col + 2 * k * PIXELS, col + (2 * k + 1) * PIXELS);
                t[k] = (a[j2] - a[j21]) * c;
                a[col + k * PIXELS] = a[j2] + a[j21];
            }
            for k in 0..h1 {
                a[col + (h1 + k) * PIXELS] = t[k];
            }
            h = h1;
        }
        a[col] *= c;
    }
}

/// The NUM_COEFS largest magnitudes (index 0, the DC value, excluded), signed by the coefficient, sorted ascending.
fn largest(a: &[f64]) -> [i16; COEFS] {
    let mut idx: Vec<usize> = (1..a.len()).collect();
    idx.sort_by(|&x, &y| {
        a[y].abs().partial_cmp(&a[x].abs()).expect("Haar coefficients of finite pixels are finite").then(x.cmp(&y))
    });
    let mut out = [0i16; COEFS];
    for (o, &i) in out.iter_mut().zip(idx.iter().take(COEFS)) {
        *o = if a[i] > 0.0 { i as i16 } else { -(i as i16) };
    }
    out.sort();
    out
}

pub fn of_image(img: &DynamicImage) -> Signature {
    let rgb = img.resize_exact(PIXELS as u32, PIXELS as u32, FilterType::CatmullRom).to_rgb8();
    let n = PIXELS * PIXELS;
    let (mut y, mut i, mut q) = (vec![0.0; n], vec![0.0; n], vec![0.0; n]);
    for (k, p) in rgb.pixels().enumerate() {
        let (r, g, b) = (p[0] as f64, p[1] as f64, p[2] as f64);
        y[k] = 0.299 * r + 0.587 * g + 0.114 * b;
        i[k] = 0.596 * r - 0.275 * g - 0.321 * b;
        q[k] = 0.212 * r - 0.523 * g + 0.311 * b;
    }
    let mut avgl = [0.0; 3];
    let mut sig = [[0i16; COEFS]; 3];
    for (ch, data) in [&mut y, &mut i, &mut q].into_iter().enumerate() {
        haar2d(data);
        data[0] /= 256.0 * 128.0;
        avgl[ch] = data[0];
        sig[ch] = largest(data);
    }
    Signature { avgl, sig }
}

impl Signature {
    /// `iqdb_` + the three DC doubles as 16 hex digits of their bits + 120 coefficients as 4 hex digits each (533 chars).
    pub fn to_hash(&self) -> String {
        let mut s = String::with_capacity(533);
        s.push_str("iqdb_");
        for v in self.avgl {
            s.push_str(&format!("{:016x}", v.to_bits()));
        }
        for ch in &self.sig {
            for &c in ch {
                s.push_str(&format!("{:04x}", c as u16));
            }
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_layout() {
        let img = DynamicImage::ImageRgb8(image::RgbImage::from_fn(200, 150, |x, y| {
            image::Rgb([(x % 255) as u8, (y % 255) as u8, 90])
        }));
        let h = of_image(&img).to_hash();
        assert_eq!(h.len(), 533);
        assert!(h.starts_with("iqdb_"));
    }

    #[test]
    fn signs_and_order() {
        let img = DynamicImage::ImageRgb8(image::RgbImage::from_fn(128, 128, |x, _| {
            image::Rgb(if x < 64 { [250, 250, 250] } else { [10, 10, 10] })
        }));
        let s = of_image(&img);
        assert!(s.sig[0].windows(2).all(|w| w[0] <= w[1]));
        assert!(s.sig[0].iter().all(|&c| c != 0));
        assert!((s.avgl[0] - 130.0 / 256.0).abs() < 0.02, "{}", s.avgl[0]); // DC ≈ mean luminance / 256 (half white, half black)
    }
}
