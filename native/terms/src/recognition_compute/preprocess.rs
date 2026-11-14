//! 识别用的 RGB 缩放契约。不做 EXIF 旋转、ICC 变换和 alpha 合成：
//! 与旧实现一样只在缩放前转成 RGB（Image.convert('RGB')），这一点与缩略图流程不同。
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
// Pillow 的像素中心采样、缩小时的支撑范围和 22 位系数；每一遍都截断并取整到 u8。
// 浮点的图像库滤波器不能互换。
// 契约来源：Pillow 12.2.0 src/libImaging/Resample.c（precompute_coeffs/8bpc）。
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
/// 白色补成正方形；多出的奇数像素留在右边和下边。BGR NHWC，0..255 的 f32。
/// 补边后的 side x side 正方形是虚拟的：图片外的采样读到白色，不实际生成 max(w,h)^2 个像素。
/// 系数、索引、累加顺序、移位和截断都与 `resize` 作用在实际生成的正方形上时相同，所以输出与之逐位一致。
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
        // `resize` 在这里两遍都跳过；输出就是补边后的正方形本身。
        RgbImage::from_fn(size, size, padded)
    } else {
        // 两遍都是 side -> size，所以 `resize` 两次算出的系数相同。
        let coeffs = coefficients(side, size, Filter::Cubic);
        // 水平方向的和是 i64 整数和，精确且与顺序无关：每个输出列的 |k| 之和约为 1.1 * 2^22，
        // 每个抽头另加半个单位的舍入，所以即使有 4 * u32::MAX 个抽头乘以 255，每个部分和也都小于 2^42。
        // 因此一列里补边抽头的贡献是 255 *（总权重 - 落在图片列上的权重），而图片的一行只需访问落在图片上的抽头：
        // 结果等于在实际生成的正方形上按顺序累加，耗时与图片大小成线性，而不是与 side^2 成正比（#74）。
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
        // 补边行在整个正方形上都是白色；它的水平结果只取决于 x。
        let white: Vec<u8> = coeffs
            .iter()
            .map(|(_, weights)| {
                let sum = (1i64 << 21) + 255 * weights.iter().map(|k| i64::from(*k)).sum::<i64>();
                (sum >> 22).clamp(0, 255) as u8
            })
            .collect();
        // 只对图片行做水平一遍（size x height）；补边行取自 `white`。
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
/// 先用 BILINEAR 直接拉伸，再在 f32 下做 CLIP 归一化，按 NCHW 分平面。
#[allow(clippy::excessive_precision)] // 让公开的 CLIP 常数与录制的 CCIP 预处理夹具看得出完全相同。
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
        // 原样保留公开的 CLIP 常数和生产代码里的 f32 表达式。
        #[allow(clippy::excessive_precision)]
        let expected_green = (20f32 / 255. - 0.4578275) / 0.26130258;
        assert_eq!(ccip[4], expected_green);
    }
    /// #46 之前的实现：实际生成白色正方形，再走通用的 resize。
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
            // 围绕 side 做缩小和放大；生产尺寸 448 只在有代表性的一部分形状上测，
            // 让 debug 测试保持快速（每个 448 用例要做两次完整的 448 x 448 缩放）。
            let mut sizes = vec![1, 2, 16, side];
            // side - 1 要对实际生成的正方形做接近原尺寸的缩放；大的 side 用下面的 448。
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
        // 纯色和边缘占比大的图片：奇数边距和图片边缘正好与补边相接。
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
        // 实际生成的正方形会有 3 * 1_000_000^2 字节（约 2.7 TiB）；虚拟补边只保留一行 4 x 1 的水平结果、
        // 系数和 4 x 4 的输出。
        let image = noise(1_000_000, 1, 0x0046);
        let result = wd_input(&image, 4);
        assert_eq!(result.len(), 4 * 4 * 3);
        assert!(result.iter().all(|v| (0. ..=255.).contains(v)));
    }
}
