//! Recognition's RGB resize contract. No EXIF rotation, ICC transform or alpha compositing:
//! Python calls Image.convert('RGB') before resizing, unlike the thumbnail pipeline.
use image::RgbImage;

#[derive(Clone, Copy)]
enum Filter {
    Linear,
    Cubic,
}
impl Filter {
    fn support(self) -> f64 {
        match self {
            Self::Linear => 1.,
            Self::Cubic => 2.,
        }
    }
    fn weight(self, x: f64) -> f64 {
        let x = x.abs();
        match self {
            Self::Linear => (1. - x).max(0.),
            Self::Cubic if x < 1. => (1.5 * x - 2.5) * x * x + 1.,
            Self::Cubic if x < 2. => (((x - 5.) * x + 8.) * x - 4.) * -0.5,
            Self::Cubic => 0.,
        }
    }
}
// Pillow's pixel-centre sampling, downsampling support and 22-bit coefficients;
// each pass clips/rounds to u8. Float image-library filters are not interchangeable.
// Contract source: Pillow 12.2.0 src/libImaging/Resample.c (precompute_coeffs/8bpc).
fn coefficients(input: u32, output: u32, filter: Filter) -> Vec<(usize, Vec<i32>)> {
    let scale = f64::from(input) / f64::from(output);
    let spread = scale.max(1.);
    let support = filter.support() * spread;
    (0..output)
        .map(|i| {
            let center = (f64::from(i) + 0.5) * scale;
            let start = ((center - support + 0.5) as i64).max(0) as usize;
            let end = ((center + support + 0.5) as usize).min(input as usize);
            let weights: Vec<f64> = (start..end).map(|j| filter.weight((j as f64 - center + 0.5) / spread)).collect();
            let sum: f64 = weights.iter().sum();
            let weights = weights
                .into_iter()
                .map(|w| {
                    let w = if sum != 0. { w / sum } else { w };
                    (w * f64::from(1 << 22) + if w < 0. { -0.5 } else { 0.5 }) as i32
                })
                .collect();
            (start, weights)
        })
        .collect()
}
fn resize(input: &RgbImage, width: u32, height: u32, filter: Filter) -> RgbImage {
    let horizontal = if width == input.width() {
        input.clone()
    } else {
        let coeffs = coefficients(input.width(), width, filter);
        RgbImage::from_fn(width, input.height(), |x, y| {
            let (start, weights) = &coeffs[x as usize];
            image::Rgb(std::array::from_fn(|c| {
                let sum = weights.iter().enumerate().fold(1i64 << 21, |sum, (j, k)| {
                    sum + i64::from(input.get_pixel((start + j) as u32, y)[c]) * i64::from(*k)
                });
                (sum >> 22).clamp(0, 255) as u8
            }))
        })
    };
    if height == input.height() {
        return horizontal;
    }
    let coeffs = coefficients(input.height(), height, filter);
    RgbImage::from_fn(width, height, |x, y| {
        let (start, weights) = &coeffs[y as usize];
        image::Rgb(std::array::from_fn(|c| {
            let sum = weights.iter().enumerate().fold(1i64 << 21, |sum, (j, k)| {
                sum + i64::from(horizontal.get_pixel(x, (start + j) as u32)[c]) * i64::from(*k)
            });
            (sum >> 22).clamp(0, 255) as u8
        }))
    })
}
/// White square padding; odd spare pixel remains at right/bottom. BGR NHWC, 0..255 f32.
pub fn wd_input(image: &RgbImage, size: u32) -> Vec<f32> {
    assert!(size > 0 && image.width() > 0 && image.height() > 0);
    let side = image.width().max(image.height());
    let (left, top) = ((side - image.width()) / 2, (side - image.height()) / 2);
    let square = RgbImage::from_fn(side, side, |x, y| {
        if x >= left && y >= top && x - left < image.width() && y - top < image.height() {
            *image.get_pixel(x - left, y - top)
        } else {
            image::Rgb([255; 3])
        }
    });
    resize(&square, size, size, Filter::Cubic)
        .pixels()
        .flat_map(|p| [f32::from(p[2]), f32::from(p[1]), f32::from(p[0])])
        .collect()
}
/// Squash BILINEAR, then CLIP normalisation in f32 with NCHW planes.
#[allow(clippy::excessive_precision)] // Keep the published CLIP constants visibly identical to the recorded CCIP preprocessing fixture.
pub fn ccip_input(image: &RgbImage, size: u32) -> Vec<f32> {
    assert!(size > 0 && image.width() > 0 && image.height() > 0);
    let image = resize(image, size, size, Filter::Linear);
    let mean = [0.48145466f32, 0.4578275, 0.40821073];
    let std = [0.26862954f32, 0.26130258, 0.27577711];
    (0..3).flat_map(|c| image.pixels().map(move |p| (f32::from(p[c]) / 255. - mean[c]) / std[c])).collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn layouts_and_odd_padding_are_distinct() {
        let rgb = RgbImage::from_pixel(1, 2, image::Rgb([10, 20, 30]));
        assert_eq!(wd_input(&rgb, 2), vec![30., 20., 10., 255., 255., 255., 30., 20., 10., 255., 255., 255.]);
        let ccip = ccip_input(&rgb, 2);
        assert_eq!(ccip.len(), 12);
        assert_eq!(ccip[0], (10f32 / 255. - 0.48145466) / 0.26862954);
        // Preserve the published CLIP constant and the production f32 expression exactly.
        #[allow(clippy::excessive_precision)]
        let expected_green = (20f32 / 255. - 0.4578275) / 0.26130258;
        assert_eq!(ccip[4], expected_green);
    }
}
