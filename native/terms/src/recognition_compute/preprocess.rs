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
/// The padded side x side square is virtual: samples outside the image read white instead of
/// materialising max(w,h)^2 pixels. Coefficients, indices, fold order, shifts and clipping are
/// those of `resize` on the materialised square, so output is bit-identical to it.
pub fn wd_input(image: &RgbImage, size: u32) -> Vec<f32> {
    assert!(size > 0 && image.width() > 0 && image.height() > 0);
    let (width, height) = image.dimensions();
    let side = width.max(height);
    let (left, top) = ((side - width) / 2, (side - height) / 2);
    let padded = |x: u32, y: u32| {
        if x >= left && y >= top && x - left < width && y - top < height {
            *image.get_pixel(x - left, y - top)
        } else {
            image::Rgb([255; 3])
        }
    };
    let output = if size == side {
        // `resize` skips both passes here; the output is the padded square itself.
        RgbImage::from_fn(size, size, padded)
    } else {
        // Both passes map side -> size, so `resize` derives the same coefficients twice.
        let coeffs = coefficients(side, size, Filter::Cubic);
        // The horizontal sums are i64 integer sums, which are exact and independent of order: per
        // output column, |k| sums to about 1.1 * 2^22 plus half a unit of rounding per tap, so even
        // with 4 * u32::MAX taps times 255 every partial sum stays below 2^42. A column's padding
        // taps therefore contribute 255 * (total weight - weight over image columns), and an image
        // row only visits the taps that land on the image: the result equals the sequential fold
        // over the materialised square, in time linear in the image instead of in side^2 (#74).
        let columns: Vec<(i64, std::ops::Range<usize>)> = coeffs
            .iter()
            .map(|(start, weights)| {
                let total: i64 = weights.iter().map(|k| i64::from(*k)).sum();
                let first = (*start).max(left as usize);
                let end = (start + weights.len()).min((left + width) as usize);
                let inside = if first < end { first - start..end - start } else { 0..0 };
                let inside_total: i64 = weights[inside.clone()].iter().map(|k| i64::from(*k)).sum();
                (255 * (total - inside_total), inside)
            })
            .collect();
        // A padding row is white across the whole square; its horizontal result depends only on x.
        let white: Vec<u8> = coeffs
            .iter()
            .map(|(_, weights)| {
                let sum = (1i64 << 21) + 255 * weights.iter().map(|k| i64::from(*k)).sum::<i64>();
                (sum >> 22).clamp(0, 255) as u8
            })
            .collect();
        // Horizontal pass for image rows only (size x height); padding rows come from `white`.
        let rows = RgbImage::from_fn(size, height, |x, y| {
            let (start, weights) = &coeffs[x as usize];
            let (padding, inside) = &columns[x as usize];
            image::Rgb(std::array::from_fn(|c| {
                let sum = weights[inside.clone()].iter().enumerate().fold((1i64 << 21) + padding, |sum, (j, k)| {
                    let column = (start + inside.start + j) as u32 - left;
                    sum + i64::from(image.get_pixel(column, y)[c]) * i64::from(*k)
                });
                (sum >> 22).clamp(0, 255) as u8
            }))
        });
        RgbImage::from_fn(size, size, |x, y| {
            let (start, weights) = &coeffs[y as usize];
            image::Rgb(std::array::from_fn(|c| {
                let sum = weights.iter().enumerate().fold(1i64 << 21, |sum, (j, k)| {
                    let row = (start + j) as u32;
                    let value = if row >= top && row - top < height {
                        rows.get_pixel(x, row - top)[c]
                    } else {
                        white[x as usize]
                    };
                    sum + i64::from(value) * i64::from(*k)
                });
                (sum >> 22).clamp(0, 255) as u8
            }))
        })
    };
    output.pixels().flat_map(|p| [f32::from(p[2]), f32::from(p[1]), f32::from(p[0])]).collect()
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
    /// The pre-#46 implementation: materialised white square, then the generic resize.
    fn wd_input_materialised(image: &RgbImage, size: u32) -> Vec<f32> {
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
    fn noise(width: u32, height: u32, seed: u32) -> RgbImage {
        let mut state = seed;
        RgbImage::from_fn(width, height, |_, _| {
            image::Rgb(std::array::from_fn(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                state as u8
            }))
        })
    }
    #[test]
    fn virtual_padding_is_bit_identical_to_materialised_square() {
        let shapes = [
            (1, 1),
            (1, 2),
            (2, 1),
            (2, 3),
            (3, 2),
            (7, 4),
            (4, 7),
            (16, 16),
            (31, 25),
            (25, 31),
            (1, 37),
            (37, 1),
            (64, 5),
            (5, 64),
            (300, 3),
            (3, 300),
            (449, 1),
            (1, 449),
            (512, 1),
            (1, 512),
            (2, 1000),
            (1000, 2),
            (448, 448),
        ];
        let mut cases = 0;
        for (index, &(width, height)) in shapes.iter().enumerate() {
            let side = width.max(height);
            let image = noise(width, height, 0x4632_91ab ^ index as u32);
            // Down/up-sampling around side; the production size 448 on a representative subset
            // keeps the debug test fast (each 448 case costs a full 448 x 448 resize twice).
            let mut sizes = vec![1, 2, 16, side];
            // side - 1 costs a near-full-size resize of the materialised square; large sides use 448 below.
            if side <= 512 {
                sizes.push(side - 1);
            }
            if side <= 64 {
                sizes.push(side + 1);
            }
            if [(1, 1), (2, 3), (4, 7), (37, 1), (5, 64), (449, 1), (1, 449), (2, 1000), (1000, 2), (448, 448)]
                .contains(&(width, height))
            {
                sizes.push(448);
            }
            sizes.retain(|&size| size > 0);
            sizes.sort_unstable();
            sizes.dedup();
            for size in sizes {
                let (actual, expected) = (wd_input(&image, size), wd_input_materialised(&image, size));
                assert_eq!(actual.len(), expected.len());
                let mismatch = actual.iter().zip(&expected).position(|(a, b)| a.to_bits() != b.to_bits());
                assert_eq!(mismatch, None, "{width}x{height} -> {size}");
                cases += 1;
            }
        }
        assert_eq!(cases, 124);
        // Uniform and border-heavy images: odd margins and image edges meet the padding exactly.
        for (width, height) in [(1, 6), (6, 1), (3, 10), (10, 3)] {
            for colour in [[0; 3], [255; 3], [254, 0, 255]] {
                let image = RgbImage::from_pixel(width, height, image::Rgb(colour));
                for size in [1, 4, 5, 7] {
                    let (actual, expected) = (wd_input(&image, size), wd_input_materialised(&image, size));
                    assert!(actual.iter().zip(&expected).all(|(a, b)| a.to_bits() == b.to_bits()));
                }
            }
        }
    }
    #[test]
    fn extreme_aspect_ratio_does_not_allocate_padded_square() {
        // The materialised square would be 3 * 1_000_000^2 bytes (about 2.7 TiB); the virtual
        // padding keeps one 4 x 1 horizontal row, the coefficients and the 4 x 4 output.
        let image = noise(1_000_000, 1, 0x0046);
        let result = wd_input(&image, 4);
        assert_eq!(result.len(), 4 * 4 * 3);
        assert!(result.iter().all(|v| (0. ..=255.).contains(v)));
    }
}
