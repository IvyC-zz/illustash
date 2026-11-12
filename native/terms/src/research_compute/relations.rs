//! 沿用现有的同画布 / 裁切关系判定。阈值和 float32 各阶段照 web/relate.py；
//! 原实现仍作为独立的算法参考。
use super::{
    numeric::sum32,
    relation_fft,
    relation_image::{self, Loaded, Plane},
};
use crate::{Error, Result};
use serde_json::{json, Value};
use std::{collections::HashMap, path::Path};
const SIDE: usize = 512;
const CELL: usize = 16;
const SEARCH: usize = 256;
const PIXEL: f32 = 28.;
const DUPLICATE: f64 = 0.004;
const DIFFERENT: f64 = 0.45;
const MATCH: f64 = 0.86;
const INSET: f64 = 0.06;
pub const GROUPING: [&str; 6] = ["identical", "duplicate", "subtle", "crop", "variant", "edit"];
const KINDS: [&str; 8] = ["identical", "duplicate", "subtle", "crop", "variant", "edit", "similar", "unrelated"];
const LABELS: [&str; 8] = ["完全相同", "重复", "细微差分", "局部重复", "差分", "局部编辑", "构图相似", "不同的图"];
fn round(x: f64, places: i32) -> f64 {
    // Python 的 round(float, digits) 对实际的二进制数按十进制舍入。
    // 先乘以 10^digits 会在恰好一半的情况下多引入一次舍入。
    format!("{x:.digits$}", digits = places as usize).parse().expect("finite decimal relation score")
}
fn pixel_round(x: f64) -> usize {
    x.round_ties_even().max(0.) as usize
}
fn canvas(ratio: f64) -> (usize, usize) {
    if ratio >= 1. {
        (SIDE, pixel_round(SIDE as f64 / ratio).max(8))
    } else {
        (pixel_round(SIDE as f64 * ratio).max(8), SIDE)
    }
}
fn blur(x: &Plane) -> Plane {
    let mut out = x.clone();
    for y in 0..x.height {
        for xx in 0..x.width {
            for c in 0..x.channels {
                // NumPy 的 sum(list-of-arrays)：按行主序做九次 f32 加法，再做 f32 除法。
                let mut s = 0.;
                for dy in -1..=1 {
                    for dx in -1..=1 {
                        s += x.at(
                            (y as isize + dy).clamp(0, x.height as isize - 1) as usize,
                            (xx as isize + dx).clamp(0, x.width as isize - 1) as usize,
                            c,
                        );
                    }
                }
                out.pixels[(y * x.width + xx) * x.channels + c] = s / 9.;
            }
        }
    }
    out
}
fn difference(a: &Plane, b: &Plane, reach: isize) -> Plane {
    assert_eq!((a.width, a.height, a.channels), (b.width, b.height, b.channels));
    let mut pixels = vec![0.; a.width * a.height];
    for y in 0..a.height {
        for x in 0..a.width {
            let (mut ab, mut ba) = (f32::INFINITY, f32::INFINITY);
            for dy in -reach..=reach {
                for dx in -reach..=reach {
                    let yy = (y as isize + dy).clamp(0, a.height as isize - 1) as usize;
                    let xx = (x as isize + dx).clamp(0, a.width as isize - 1) as usize;
                    let (mut da, mut db) = (0f32, 0f32);
                    for c in 0..a.channels {
                        da = da.max((a.at(y, x, c) - b.at(yy, xx, c)).abs());
                        db = db.max((b.at(y, x, c) - a.at(yy, xx, c)).abs());
                    }
                    ab = ab.min(da);
                    ba = ba.min(db);
                }
            }
            pixels[y * a.width + x] = ab.max(ba);
        }
    }
    Plane { width: a.width, height: a.height, channels: 1, pixels }
}
fn grey(x: &Plane) -> Result<Plane> {
    #[cfg(windows)]
    let pixels = {
        // NumPy 的 RGB[h,w,3] @ weights[3] 是 h 次独立的 (w,3) GEMV 调用，
        // w=1 时是 DOT。把高度展平会改变 BLAS 尾部的舍入。
        let mut pixels = Vec::with_capacity(x.width * x.height);
        for row in x.pixels.chunks_exact(x.width * 3) {
            pixels.extend(
                illustash_platform::platform_research_math::cross(row, x.width, &[0.299, 0.587, 0.114], 1, 3)
                    .map_err(Error::Invalid)?,
            );
        }
        pixels
    };
    #[cfg(not(windows))]
    let pixels = x.pixels.chunks_exact(3).map(|p| (p[0] * 0.299f32 + p[1] * 0.587f32) + p[2] * 0.114f32).collect();
    Ok(Plane { width: x.width, height: x.height, channels: 1, pixels })
}
fn cells(d: &Plane) -> (usize, usize, Vec<bool>) {
    let (rows, cols) = ((d.height / CELL).max(1), (d.width / CELL).max(1));
    // 公共画布有一个轴短于 CELL 时，Python 的 reshape 会失败。
    // 调用方把这种情况报告为不可读 / 待重试的一对，而不是编造格子。
    let mut out = vec![false; rows * cols];
    for y in 0..rows {
        for x in 0..cols {
            let mut n = 0;
            for yy in y * CELL..(y + 1) * CELL {
                for xx in x * CELL..(x + 1) * CELL {
                    n += usize::from(d.at(yy, xx, 0) > PIXEL);
                }
            }
            out[y * cols + x] = n as f64 / (CELL * CELL) as f64 > 0.10;
        }
    }
    (rows, cols, out)
}
type Region = (usize, usize, usize, usize, usize);
fn regions(cells: &[bool], rows: usize, cols: usize) -> Vec<Region> {
    let mut grown = vec![false; rows * cols];
    for y in 0..rows {
        for x in 0..cols {
            for dy in -1..=1 {
                for dx in -1..=1 {
                    let (v, u) = (y as isize + dy, x as isize + dx);
                    if v >= 0 && u >= 0 && v < rows as isize && u < cols as isize {
                        grown[y * cols + x] |= cells[v as usize * cols + u as usize];
                    }
                }
            }
        }
    }
    let mut seen = vec![false; grown.len()];
    let mut boxes = vec![];
    for start in 0..grown.len() {
        if !grown[start] || seen[start] {
            continue;
        }
        let mut stack = vec![start];
        seen[start] = true;
        let (mut x0, mut y0, mut x1, mut y1, mut n) = (cols, rows, 0, 0, 0);
        while let Some(i) = stack.pop() {
            let (y, x) = (i / cols, i % cols);
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
            n += 1;
            for (dy, dx) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
                let (v, u) = (y as isize + dy, x as isize + dx);
                if v >= 0 && u >= 0 && v < rows as isize && u < cols as isize {
                    let j = v as usize * cols + u as usize;
                    if grown[j] && !seen[j] {
                        seen[j] = true;
                        stack.push(j);
                    }
                }
            }
        }
        boxes.push((x0, y0, x1 - x0 + 1, y1 - y0 + 1, n));
    }
    boxes.sort_by_key(|b| std::cmp::Reverse(b.4));
    boxes
}
fn boxes_json(boxes: &[Region], rows: usize, cols: usize) -> Vec<Value> {
    boxes.iter().take(12).map(|&(x,y,w,h,_)|json!({"x":x as f64/cols as f64,"y":y as f64/rows as f64,"w":w as f64/cols as f64,"h":h as f64/rows as f64})).collect()
}
#[derive(Clone)]
struct Compared {
    kind: &'static str,
    changed: f64,
    boxes: Vec<Value>,
    edited: Option<&'static str>,
    how: Option<&'static str>,
}
fn correlation(a: &Plane, b: &Plane) -> Result<f64> {
    let mean_a = sum32(&a.pixels) / a.pixels.len() as f32;
    let mean_b = sum32(&b.pixels) / b.pixels.len() as f32;
    let a: Vec<_> = a.pixels.iter().map(|x| x - mean_a).collect();
    let b: Vec<_> = b.pixels.iter().map(|x| x - mean_b).collect();
    let dot = illustash_platform::platform_research_math::dot;
    let xy = dot(&a, &b).map_err(Error::Invalid)?;
    let xx = dot(&a, &a).map_err(Error::Invalid)?;
    let yy = dot(&b, &b).map_err(Error::Invalid)?;
    let denominator = (xx * yy).sqrt();
    // np 标量除以 Python 的 max(sqrt float32, 1e-6)：非平坦分支是 f32。
    Ok(if denominator >= 1e-6 { f64::from(xy / denominator) } else { f64::from(xy) / 1e-6 })
}
fn covered(arr: &Plane, box_: (usize, usize, usize, usize), changed: &Plane) -> Option<&'static str> {
    let (x0, y0, x1, y1) = box_;
    let (w, h) = (x1 - x0, y1 - y0);
    if w.min(h) < 12 {
        return None;
    }
    let mut g = vec![0.; w * h];
    for y in 0..h {
        for x in 0..w {
            g[y * w + x] = (arr.at(y + y0, x + x0, 0) * 0.299f32 + arr.at(y + y0, x + x0, 1) * 0.587f32)
                + arr.at(y + y0, x + x0, 2) * 0.114f32;
        }
    }
    let mut selected = vec![];
    let mut flat = 0;
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            if changed.at(y + y0, x + x0, 0) > PIXEL {
                let lap = ((((4. * g[y * w + x] - g[(y - 1) * w + x]) - g[(y + 1) * w + x]) - g[y * w + x - 1])
                    - g[y * w + x + 1])
                    .abs();
                flat += usize::from(lap < 1.5);
                selected.push([arr.at(y + y0, x + x0, 0), arr.at(y + y0, x + x0, 1), arr.at(y + y0, x + x0, 2)]);
            }
        }
    }
    if selected.len() > 200 && flat as f64 / selected.len() as f64 > 0.9 {
        let mut mean = [0f32; 3];
        for p in &selected {
            for c in 0..3 {
                mean[c] += p[c];
            }
        }
        for v in &mut mean {
            *v /= selected.len() as f32;
        }
        let mut var = [0f32; 3];
        for p in &selected {
            for c in 0..3 {
                let d = p[c] - mean[c];
                var[c] += d * d;
            }
        }
        let std: Vec<_> = var.iter().map(|v| (v / selected.len() as f32).sqrt()).collect();
        if sum32(&std) / 3. < 6. {
            return Some("fill");
        }
    }
    let mut col = vec![0usize; w - 1];
    let mut row = vec![0usize; h - 1];
    let (mut nx, mut ny) = (0, 0);
    for y in 0..h {
        for x in 0..w - 1 {
            if (g[y * w + x + 1] - g[y * w + x]).abs() > 6. {
                col[x] += 1;
                nx += 1;
            }
        }
    }
    for y in 0..h - 1 {
        for x in 0..w {
            if (g[(y + 1) * w + x] - g[y * w + x]).abs() > 6. {
                row[y] += 1;
                ny += 1;
            }
        }
    }
    let cols: Vec<_> = col.iter().map(|n| *n as f64 / h as f64 > 0.6).collect();
    let rows: Vec<_> = row.iter().map(|n| *n as f64 / w as f64 > 0.6).collect();
    let near = |v: &[bool], i: usize| v[i] || i > 0 && v[i - 1] || i + 1 < v.len() && v[i + 1];
    if cols.iter().filter(|v| **v).count() >= 3
        && rows.iter().filter(|v| **v).count() >= 3
        && col.iter().enumerate().filter(|(i, _)| near(&cols, *i)).map(|(_, n)| n).sum::<usize>() as f64
            > 0.6 * nx as f64
        && row.iter().enumerate().filter(|(i, _)| near(&rows, *i)).map(|(_, n)| n).sum::<usize>() as f64
            > 0.6 * ny as f64
    {
        Some("mosaic")
    } else {
        None
    }
}
fn compare(a: &Plane, b: &Plane, fine: Option<(&Plane, &Plane)>) -> Result<Compared> {
    if a.width < CELL || a.height < CELL {
        return Err(Error::Invalid("relation canvas too narrow for cells".into()));
    }
    let (fa, fb) = (blur(a), blur(b));
    let raw = difference(&fa, &fb, 1);
    let mut fitted = fa.clone();
    for k in 0..3 {
        let mut count = [0usize; 64];
        let mut mean = [0f64; 64];
        for (a, b) in fa.pixels.chunks_exact(3).zip(fb.pixels.chunks_exact(3)) {
            let i = (a[k].clamp(0., 255.) as usize) / 4;
            count[i] += 1;
            mean[i] += f64::from(b[k]);
        }
        for i in 0..64 {
            mean[i] /= count[i].max(1) as f64;
        }
        let used: Vec<_> = (0..64).filter(|i| count[*i] > 0).collect();
        for (dst, src) in fitted.pixels.chunks_exact_mut(3).zip(fa.pixels.chunks_exact(3)) {
            let x = f64::from(src[k]);
            let hi = used.partition_point(|i| *i as f64 * 4. + 1.5 <= x);
            let val = if hi == 0 {
                mean[used[0]]
            } else if hi == used.len() {
                mean[used[hi - 1]]
            } else {
                let (lo, high) = (used[hi - 1], used[hi]);
                mean[lo] + (mean[high] - mean[lo]) / (high - lo) as f64 / 4. * (x - (lo as f64 * 4. + 1.5))
            };
            dst[k] = val as f32;
        }
    }
    let tuned = difference(&fitted, &fb, 1);
    let (_, _, tone) = cells(&tuned);
    let (rows, cols, cells) = cells(&raw);
    let changed = cells.iter().filter(|v| **v).count() as f64 / cells.len() as f64;
    let after = tone.iter().filter(|v| **v).count() as f64 / tone.len() as f64;
    let mut r = Compared { kind: "duplicate", changed: round(changed, 4), boxes: vec![], edited: None, how: None };
    if changed < DUPLICATE {
        return Ok(r);
    }
    if after < DUPLICATE {
        r.kind = "edit";
        r.how = Some("tone");
        return Ok(r);
    }
    if changed > DIFFERENT {
        let structure = correlation(&grey(&fa)?, &grey(&fb)?)?;
        r.kind = if structure >= 0.75 {
            "variant"
        } else if structure >= 0.5 {
            "similar"
        } else {
            "unrelated"
        };
        return Ok(r);
    }
    let boxes = regions(&cells, rows, cols);
    r.boxes = boxes_json(&boxes, rows, cols);
    let (mx, my) = (pixel_round(cols as f64 * 0.07).max(1), pixel_round(rows as f64 * 0.07).max(1));
    if !(my..rows.saturating_sub(my)).any(|y| (mx..cols.saturating_sub(mx)).any(|x| cells[y * cols + x])) {
        r.kind = "edit";
        r.how = Some("border");
        return Ok(r);
    }
    let (aa, bb) = fine.unwrap_or((a, b));
    let (x, y, w, h, _) = boxes[0];
    let x0 = (x + usize::from(x > 0)) * CELL;
    let y0 = (y + usize::from(y > 0)) * CELL;
    let x1 = (x + w - usize::from(x + w < cols)).min(cols) * CELL;
    let y1 = (y + h - usize::from(y + h < rows)).min(rows) * CELL;
    let box_ = (
        pixel_round(x0 as f64 / a.width as f64 * aa.width as f64),
        pixel_round(y0 as f64 / a.height as f64 * aa.height as f64),
        pixel_round(x1 as f64 / a.width as f64 * aa.width as f64),
        pixel_round(y1 as f64 / a.height as f64 * aa.height as f64),
    );
    let changed_px = difference(&blur(aa), &blur(bb), 2);
    for (side, arr, other) in [("a", aa, bb), ("b", bb, aa)] {
        if let Some(how) = covered(arr, box_, &changed_px) {
            if covered(other, box_, &changed_px).is_none() {
                r.kind = "edit";
                r.how = Some(how);
                r.edited = Some(side);
                return Ok(r);
            }
        }
    }
    r.kind = "variant";
    Ok(r)
}
fn grey_at(x: &Plane, side: usize) -> Result<Plane> {
    let g = grey(x)?;
    let scale = side as f64 / g.width.max(g.height) as f64;
    Ok(blur(&g.resize(pixel_round(g.width as f64 * scale).max(8), pixel_round(g.height as f64 * scale).max(8))))
}
fn integral(x: &Plane, squared: bool) -> Vec<f32> {
    let (h, w) = (x.height, x.width);
    let mut s = vec![0.; (h + 1) * (w + 1)];
    // 先 cumsum(0) 再 cumsum(1)，各自按顺序做 f32 累加；不能换成 f64 递推。
    for xx in 0..w {
        let mut sum = 0.;
        for y in 0..h {
            let v = x.at(y, xx, 0);
            sum += if squared { v * v } else { v };
            s[(y + 1) * (w + 1) + xx + 1] = sum;
        }
    }
    for y in 1..=h {
        let mut sum = 0.;
        for xx in 1..=w {
            sum += s[y * (w + 1) + xx];
            s[y * (w + 1) + xx] = sum;
        }
    }
    s
}
fn ncc(big: &Plane, small: &Plane) -> Result<Option<(f64, usize, usize)>> {
    let (h, w) = (small.height, small.width);
    let area = h * w;
    let mean = sum32(&small.pixels) / area as f32;
    let t: Vec<_> = small.pixels.iter().map(|x| x - mean).collect();
    let squares: Vec<_> = t.iter().map(|x| x * x).collect();
    let norm = sum32(&squares).sqrt();
    if f64::from(norm) < 1e-3 * area as f64 {
        return Ok(None);
    }
    let corr = relation_fft::correlation(&big.pixels, (big.height, big.width), &t, (h, w))?;
    let s1 = integral(big, false);
    let s2 = integral(big, true);
    let stride = big.width + 1;
    let window = |s: &[f32], y: usize, x: usize| {
        ((s[(y + h) * stride + x + w] - s[y * stride + x + w]) - s[(y + h) * stride + x]) + s[y * stride + x]
    };
    let (mut best, mut at) = (f64::NEG_INFINITY, 0);
    for y in 0..=big.height - h {
        for x in 0..=big.width - w {
            let sum = window(&s1, y, x);
            let var = window(&s2, y, x) - sum * sum / area as f32;
            let score = if var > area as f32 {
                f64::from(corr[y * (big.width - w + 1) + x] / (var.max(1e-6).sqrt() * norm))
            } else {
                -1.
            };
            if score > best {
                best = score;
                at = y * (big.width - w + 1) + x;
            }
        }
    }
    Ok(Some((best, at % (big.width - w + 1), at / (big.width - w + 1))))
}
#[derive(Clone, Copy)]
struct Found {
    score: f64,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    f: f64,
}
fn search(g: &Plane, gs: &Plane, factors: &[f64], box_: Option<(usize, usize, usize, usize)>) -> Result<Option<Found>> {
    let (x0, y0, x1, y1) = box_.unwrap_or((0, 0, g.width, g.height));
    let region = g.crop(x0, y0, x1, y1)?;
    let mut best: Option<Found> = None;
    for &f in factors {
        let (w, h) = (pixel_round(gs.width as f64 * f), pixel_round(gs.height as f64 * f));
        if w < 24 || h < 24 || w > region.width || h > region.height {
            continue;
        }
        if let Some((score, x, y)) = ncc(&region, &blur(&gs.resize(w, h)))? {
            if best.is_none_or(|b| score > b.score) {
                best = Some(Found { score, x: (x + x0) as f64, y: (y + y0) as f64, w: w as f64, h: h as f64, f });
            }
        }
    }
    Ok(best)
}
fn geomspace(start: f64, end: f64, n: usize) -> Vec<f64> {
    let a = start.log10();
    let b = end.log10();
    let step = (b - a) / (n - 1) as f64;
    let mut out: Vec<_> = (0..n).map(|i| 10f64.powf(a + i as f64 * step)).collect();
    out[0] = start;
    out[n - 1] = end;
    out
}
/// NCC 分数和归一化的 (left, top, width, height) 坐标。
pub type Located = (f64, f64, f64, f64, f64);
/// #153 的最小比对接口：解码后的显示像素、NCC 定位和不变的关系分类。不用全局缓存、模型，也不访问资产。
pub fn locate(big: &Plane, small: &Plane, least: f64) -> Result<Option<Located>> {
    if big.channels != 3 || small.channels != 3 {
        return Err(Error::Invalid("locate requires RGB relation arrays".into()));
    }
    let g = grey_at(big, SEARCH)?;
    let gs = grey(small)?;
    let fit = (g.width as f64 / gs.width as f64).min(g.height as f64 / gs.height as f64);
    let area = (g.width * g.height) as f64;
    let factors: Vec<_> = geomspace(fit, fit * 0.2, 28)
        .into_iter()
        .filter(|f| {
            let a = gs.width as f64 * f * gs.height as f64 * f;
            a <= 0.97 * area && a >= least * area
        })
        .collect();
    let Some(mut best) = search(&g, &gs, &factors, None)? else {
        return Ok(None);
    };
    let fine = grey_at(big, SEARCH * 2)?;
    let k = fine.width as f64 / g.width as f64;
    best.x *= k;
    best.y *= k;
    best.w *= k;
    best.h *= k;
    best.f *= k;
    for (spread, steps) in [(1.08, 17), (1.012, 9)] {
        let margin = pixel_round(best.w.max(best.h) * 0.1) + 4;
        let box_ = (
            pixel_round(best.x).saturating_sub(margin),
            pixel_round(best.y).saturating_sub(margin),
            (pixel_round(best.x + best.w) + margin).min(fine.width),
            (pixel_round(best.y + best.h) + margin).min(fine.height),
        );
        if let Some(found) = search(&fine, &gs, &geomspace(best.f / spread, best.f * spread, steps), Some(box_))? {
            best = found;
        }
    }
    Ok(Some((
        best.score,
        best.x / fine.width as f64,
        best.y / fine.height as f64,
        best.w / fine.width as f64,
        best.h / fine.height as f64,
    )))
}
fn align(a: &Plane, b: &Plane) -> Result<Option<(Plane, Plane, f64, f64)>> {
    let core = b.crop(
        pixel_round(b.width as f64 * INSET),
        pixel_round(b.height as f64 * INSET),
        pixel_round(b.width as f64 * (1. - INSET)),
        pixel_round(b.height as f64 * (1. - INSET)),
    )?;
    let Some((score, x, y, w, h)) = locate(a, &core, 0.1)? else {
        return Ok(None);
    };
    if score < MATCH {
        return Ok(None);
    }
    let k = 1. - 2. * INSET;
    let (fx, fy, fw, fh) = (x - w * INSET / k, y - h * INSET / k, w / k, h / k);
    let (x0, y0, x1, y1) = (fx.max(0.), fy.max(0.), (fx + fw).min(1.), (fy + fh).min(1.));
    if x1 - x0 < 0.5 || y1 - y0 < 0.5 {
        return Ok(None);
    }
    let aa = a.crop(
        pixel_round(x0 * a.width as f64),
        pixel_round(y0 * a.height as f64),
        pixel_round(x1 * a.width as f64),
        pixel_round(y1 * a.height as f64),
    )?;
    let (bx0, by0, bx1, by1) = ((x0 - fx) / fw, (y0 - fy) / fh, (x1 - fx) / fw, (y1 - fy) / fh);
    let bb = b.crop(
        pixel_round(bx0 * b.width as f64),
        pixel_round(by0 * b.height as f64),
        pixel_round(bx1 * b.width as f64),
        pixel_round(by1 * b.height as f64),
    )?;
    Ok(Some((aa, bb, (x1 - x0) * (y1 - y0), (bx1 - bx0) * (by1 - by0))))
}
pub fn relate(a: &Loaded, b: &Loaded) -> Result<Value> {
    let (aa, bb) = (&a.image, &b.image);
    let (ra, rb) = (aa.width as f64 / aa.height as f64, bb.width as f64 / bb.height as f64);
    let mut fallback = None;
    if (ra / rb).ln().abs() < 0.02 {
        if a.size == b.size && aa.width == bb.width && aa.height == bb.height && aa.pixels == bb.pixels {
            return Ok(json!({"boxes_a":[],"boxes_b":[],"kind":"identical","changed":0.,"note":"像素完全一致"}));
        }
        let (w, h) = canvas(ra);
        let big = if (aa.height, aa.width) >= (bb.height, bb.width) { aa } else { bb };
        let fine = (aa.resize(big.width, big.height), bb.resize(big.width, big.height));
        let mut r = compare(&aa.resize(w, h), &bb.resize(w, h), Some((&fine.0, &fine.1)))?;
        if r.kind != "duplicate" {
            if let Some((pa, pb, sa, sb)) = align(aa, bb)? {
                if sa.min(sb) > 0.9 {
                    let (w, h) = canvas(pb.width as f64 / pb.height as f64);
                    let fine_a = pa.resize(pb.width, pb.height);
                    let again = compare(&pa.resize(w, h), &pb.resize(w, h), Some((&fine_a, &pb)))?;
                    if again.changed < r.changed {
                        r = again;
                    }
                }
            }
        }
        if !["similar", "unrelated"].contains(&r.kind) {
            let mut note = match r.kind {
                // 写完整的句子，或一条说明加后缀，这样页面才能翻译它们（i18n.js tb()）。
                "duplicate" if a.size != b.size => "同一张图（尺寸不同）".into(),
                "duplicate" => "同一张图（重新压缩过）".into(),
                "variant" if r.changed <= DIFFERENT => {
                    format!("局部不同，约占画面 {percent}%", percent = format_args!("{:.0}", r.changed * 100.))
                }
                "variant" => "线稿相同，大面积重新上色或修改".into(),
                "edit" => match r.how {
                    Some("tone") => "整体调色或亮度不同",
                    Some("border") => "边缘加了文字、水印或边框",
                    Some("mosaic") => "局部打了马赛克",
                    Some("fill") => "局部被色块遮住",
                    _ => "",
                }
                .into(),
                _ => String::new(),
            };
            if r.kind == "variant" && r.changed < 0.02 {
                r.kind = "subtle";
                note = "只有很小的地方不同（表情、签名、小物件等）".into();
            }
            if r.kind == "edit" {
                if let Some(side) = r.edited {
                    note = if side == "a" {
                        format!("{note}（改动在左图）")
                    } else {
                        format!("{note}（改动在右图）")
                    };
                }
            }
            return Ok(
                json!({"kind":r.kind,"changed":r.changed,"note":note,"boxes_a":r.boxes,"boxes_b":r.boxes,"edited":r.edited}),
            );
        }
        fallback = Some(r);
    }
    let first = locate(aa, bb, 0.1)?;
    let second = locate(bb, aa, 0.1)?;
    let (found, big, small, flip) = if second.map(|v| v.0).unwrap_or(-1.) > first.map(|v| v.0).unwrap_or(-1.) {
        (second, bb, aa, true)
    } else {
        (first, aa, bb, false)
    };
    if let Some((score, x, y, w, h)) = found.filter(|v| v.0 >= MATCH) {
        let part = big.crop(
            pixel_round(x * big.width as f64),
            pixel_round(y * big.height as f64),
            pixel_round((x + w) * big.width as f64),
            pixel_round((y + h) * big.height as f64),
        )?;
        let (cw, ch) = canvas(part.width as f64 / part.height as f64);
        let r = compare(&part.resize(cw, ch), &small.resize(cw, ch), None)?;
        let mut boxes_big = vec![json!({"x":x,"y":y,"w":w,"h":h,"match":true})];
        for b in &r.boxes {
            boxes_big.push(json!({"x":x+b["x"].as_f64().expect("numeric box")*w,"y":y+b["y"].as_f64().expect("numeric box")*h,"w":b["w"].as_f64().expect("numeric box")*w,"h":b["h"].as_f64().expect("numeric box")*h}));
        }
        let percent = format_args!("{:.0}", w * h * 100.);
        let mut note = if flip {
            format!("左图是右图的一部分（约占 {percent}%）")
        } else {
            format!("右图是左图的一部分（约占 {percent}%）")
        };
        if ["variant", "edit"].contains(&r.kind) && r.changed >= 0.02 {
            note = format!("{note}，重叠处也有改动");
        }
        let (ba, bb) = if flip { (r.boxes, boxes_big) } else { (boxes_big, r.boxes) };
        return Ok(
            json!({"kind":"crop","changed":r.changed,"score":round(score,3),"note":note,"boxes_a":ba,"boxes_b":bb}),
        );
    }
    if let Some(r) = fallback {
        Ok(
            json!({"kind":r.kind,"changed":r.changed,"note":if r.kind=="similar"{"构图接近，但画的不是同一张"}else{""},"boxes_a":[],"boxes_b":[]}),
        )
    } else {
        Ok(json!({"kind":"unrelated","note":"","boxes_a":[],"boxes_b":[]}))
    }
}
pub fn relate_all(images: &[(String, Loaded)]) -> Result<Value> {
    let mut pairs = vec![];
    let mut parent: Vec<_> = (0..images.len()).collect();
    fn find(parent: &[usize], mut i: usize) -> usize {
        while parent[i] != i {
            i = parent[i];
        }
        i
    }
    for i in 0..images.len() {
        for j in i + 1..images.len() {
            let mut p = relate(&images[i].1, &images[j].1)?;
            let rank = KINDS.iter().position(|kind| p["kind"] == *kind).expect("known relation");
            if rank <= 1 {
                let root = find(&parent, i);
                let b = find(&parent, j);
                parent[b] = root;
            }
            p["a"] = json!(images[i].0);
            p["b"] = json!(images[j].0);
            p["label"] = json!(LABELS[rank]);
            pairs.push((rank, i, j, p));
        }
    }
    pairs.sort_by_key(|p| (p.0, p.1, p.2));
    let mut groups: Vec<Vec<String>> = vec![];
    let mut group = HashMap::new();
    let mut sizes = json!({});
    for (i, (u, image)) in images.iter().enumerate() {
        let root = find(&parent, i);
        let n = *group.entry(root).or_insert_with(|| {
            groups.push(vec![]);
            groups.len() - 1
        });
        groups[n].push(u.clone());
        sizes[u] = json!({"width":image.size.0,"height":image.size.1});
    }
    Ok(
        json!({"pairs":pairs.into_iter().map(|p|p.3).collect::<Vec<_>>(),"same":groups.into_iter().filter(|g|g.len()>1).collect::<Vec<_>>(),"sizes":sizes}),
    )
}
pub fn load(path: &Path) -> Result<Loaded> {
    relation_image::load(path)
}
/// 查询响应保持旧 Python 的插入顺序，包括请求里的尺寸顺序。这只是 HTTP 的序列化；
/// 已存的表和指纹不变。
pub(super) fn wire_result(result: &Value, ids: &Value) -> Result<crate::http_json::Json> {
    use crate::http_json::Json;
    fn object(v: &Value, keys: &[&str]) -> Json {
        Json::object(keys.iter().filter_map(|k| v.get(*k).cloned().map(|v| (*k, v.into()))))
    }
    fn boxes(v: &Value) -> Json {
        Json::Array(
            v.as_array().expect("relation boxes").iter().map(|b| object(b, &["x", "y", "w", "h", "match"])).collect(),
        )
    }
    let pairs = result["pairs"].as_array().ok_or_else(|| Error::Invalid("invalid relation pairs".into()))?;
    let pairs = Json::Array(
        pairs
            .iter()
            .map(|p| {
                let mut out = object(p, &["a", "b", "label"]);
                out.set("boxes_a", boxes(&p["boxes_a"]));
                out.set("boxes_b", boxes(&p["boxes_b"]));
                out.extend(object(p, &["kind", "changed", "score", "note", "edited"]));
                out
            })
            .collect(),
    );
    let mut sizes = Json::object(std::iter::empty::<(&str, Json)>());
    for id in ids.as_array().ok_or_else(|| Error::Invalid("invalid relation ids".into()))? {
        let id = id.as_str().ok_or_else(|| Error::Invalid("invalid relation id".into()))?;
        if let Some(size) = result["sizes"].get(id) {
            sizes.set(id, object(size, &["width", "height"]));
        }
    }
    Ok(Json::object([
        ("pairs", pairs),
        ("same", result["same"].clone().into()),
        ("sizes", sizes),
        ("missing", result["missing"].clone().into()),
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[cfg(windows)]
    fn windows_grey_keeps_numpy_rowwise_dot_gemv_and_tail_bits() {
        use sha2::{Digest, Sha256};
        // 独立的 NumPy 2.5.3 / 固定版本的 Windows wheel，在运行候选之前采集。
        // 图片的每一行各自是一个 (width,3) 矩阵。
        for (width, expected) in [
            (1, "094256784222d3d11ef82a2d76fb6c4274e6f8c5a0ef8a581c4b4548bf49845b"),
            (3, "efa1fbf05614a3ce44e8824636d74ea27f6abcbfd87773e075c1f1b7d651f500"),
            (17, "360fd9fa9eae551b7d04ce31eda3c30f6a9030a0cd8b5df08c5b83a16511d29c"),
            (37, "3a72d39f9b43be83ce811afcab7ca457fe3acf07b2538f61f9a74a2be4db21de"),
            (600, "7724157b714a9e4c5fefcb835df66750abd745873d5d8969ce0dff64889413ee"),
        ] {
            let pixels = (0..3 * width * 3).map(|i| ((i * 71 + 19) % 256) as f32).collect();
            let grey = grey(&Plane::new(width, 3, 3, pixels).unwrap()).unwrap();
            let mut hash = Sha256::new();
            for v in grey.pixels {
                hash.update(v.to_le_bytes());
            }
            assert_eq!(format!("{:x}", hash.finalize()), expected, "width={width}");
        }
    }
    #[test]
    fn flat_templates_are_rejected_and_threshold_cells_are_strict() {
        let p = Plane::new(32, 32, 1, vec![100.; 1024]).unwrap();
        assert!(ncc(&p, &p).unwrap().is_none());
        let mut p = Plane::new(16, 16, 1, vec![28.; 256]).unwrap();
        assert!(!cells(&p).2[0]);
        p.pixels[..25].fill(29.);
        assert!(!cells(&p).2[0]);
        p.pixels[25] = 29.;
        assert!(cells(&p).2[0]);
    }
    #[test]
    fn identical_is_pixel_based_with_source_size_and_stable_groups() {
        let im = Loaded { image: Plane::new(32, 32, 3, vec![70.; 3072]).unwrap(), size: (32, 32) };
        let r = relate_all(&[("z".into(), im.clone()), ("a".into(), im.clone()), ("b".into(), im)]).unwrap();
        assert_eq!(r["same"], json!([["z", "a", "b"]]));
        assert_eq!(r["pairs"][0]["kind"], "identical");
        assert_eq!(r["pairs"][0]["a"], "z");
    }
    #[test]
    fn decimal_round_and_wire_keep_legacy_half_cases_and_field_order() {
        assert_eq!(round(2.675, 2), 2.67);
        let r = json!({"pairs":[{"a":"z","b":"a","label":"重复","kind":"duplicate","changed":0.,"note":"同一张图","boxes_a":[],"boxes_b":[],"edited":null}],"same":[["z","a"]],"sizes":{"a":{"width":32,"height":48},"z":{"width":32,"height":48}},"missing":[]});
        assert_eq!(
            wire_result(&r, &json!(["z", "a"])).unwrap().dump(),
            r#"{"pairs": [{"a": "z", "b": "a", "label": "重复", "boxes_a": [], "boxes_b": [], "kind": "duplicate", "changed": 0.0, "note": "同一张图", "edited": null}], "same": [["z", "a"]], "sizes": {"z": {"width": 32, "height": 48}, "a": {"width": 32, "height": 48}}, "missing": []}"#
        );
    }
}
