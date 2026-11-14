//! 识别吞吐基准的几种模式（#213），由 tools/bench_recognition.py 驱动。
//! 只用合成输入；模型和运行库用显式路径给出，从不在用户数据里查找。
//! `bench-generate` 按种子生成图片和锚点；`bench-stages` 重复 worker 对每张图的处理步骤（producer::run），
//! 每一步单独计时。worker 本身不变。
use illustash_terms::recognition_compute::{
    anchor_snapshot::Snapshot, backend, claims::Claims, preprocess, producer, session,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    time::Instant,
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// 按序号轮流使用的图片规格：常见插画尺寸，JPEG 质量或 PNG。
const SPECS: [(u32, u32, Option<u8>); 8] = [
    (1200, 1700, Some(90)),
    (2048, 2048, None),
    (1920, 1080, Some(92)),
    (900, 1200, None),
    (2480, 3508, Some(90)),
    (1000, 1000, Some(85)),
    (1600, 900, None),
    (750, 1334, Some(90)),
];

struct Rng(u64);
impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn unit(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }
    fn byte(&mut self) -> u8 {
        (self.next() >> 56) as u8
    }
}

/// 柔和渐变上叠平涂形状，再加轻微噪声：压缩特性像插画而不像纯噪声，
/// 文件大小和解码工作量都在接近真实的范围内。
fn picture(width: u32, height: u32, seed: u64) -> image::RgbImage {
    let mut rng = Rng::new(seed);
    let a = [rng.byte(), rng.byte(), rng.byte()].map(f32::from);
    let b = [rng.byte(), rng.byte(), rng.byte()].map(f32::from);
    let angle = rng.unit() * std::f32::consts::TAU;
    let (dx, dy) = (angle.cos() / width as f32, angle.sin() / height as f32);
    let mut pixels = image::RgbImage::from_fn(width, height, |x, y| {
        let t = ((x as f32 * dx + y as f32 * dy) * 0.5 + 0.5).clamp(0., 1.);
        image::Rgb([0, 1, 2].map(|c| (a[c] + (b[c] - a[c]) * t) as u8))
    });
    for _ in 0..24 {
        let (cx, cy) = (rng.unit() * width as f32, rng.unit() * height as f32);
        let (rx, ry) = (8. + rng.unit() * width as f32 / 4., 8. + rng.unit() * height as f32 / 4.);
        let color = [rng.byte(), rng.byte(), rng.byte()];
        let (x0, x1) = ((cx - rx).max(0.) as u32, ((cx + rx) as u32).min(width));
        let (y0, y1) = ((cy - ry).max(0.) as u32, ((cy + ry) as u32).min(height));
        for y in y0..y1 {
            for x in x0..x1 {
                let (u, v) = ((x as f32 - cx) / rx, (y as f32 - cy) / ry);
                let d = u * u + v * v;
                if d <= 1. {
                    let shade = 1. - 0.25 * d;
                    pixels.put_pixel(x, y, image::Rgb(color.map(|c| (f32::from(c) * shade) as u8)));
                }
            }
        }
    }
    for p in pixels.pixels_mut() {
        let noise = rng.next();
        for (c, value) in p.0.iter_mut().enumerate() {
            *value = value.saturating_add(((noise >> (c * 8)) & 3) as u8);
        }
    }
    pixels
}

fn encode(path: &Path, pixels: &image::RgbImage, quality: Option<u8>) -> Result<()> {
    match quality {
        None => pixels.save_with_format(path, image::ImageFormat::Png)?,
        Some(q) => {
            let mut encoder = mozjpeg::Compress::new(mozjpeg::ColorSpace::JCS_RGB);
            encoder.set_size(pixels.width() as usize, pixels.height() as usize);
            encoder.set_quality(f32::from(q));
            let mut encoder = encoder.start_compress(Vec::new())?;
            encoder.write_scanlines(pixels.as_raw())?;
            fs::write(path, encoder.finish()?)?;
        }
    }
    Ok(())
}

pub fn generate(request: &Value) -> Result<Value> {
    let folder = PathBuf::from(request["folder"].as_str().ok_or("folder missing")?);
    let count = request["count"].as_u64().ok_or("count missing")?;
    let seed = request["seed"].as_u64().unwrap_or(213);
    fs::create_dir_all(&folder)?;
    let mut files = Vec::new();
    for i in 0..count {
        let (width, height, quality) = SPECS[(i % SPECS.len() as u64) as usize];
        let path = folder.join(format!("synthetic-{i:05}.{}", if quality.is_some() { "jpg" } else { "png" }));
        encode(&path, &picture(width, height, seed ^ (i + 1)), quality)?;
        files.push(json!({"path":path,"width":width,"height":height,"bytes":fs::metadata(&path)?.len()}));
    }
    // 随机的 CCIP 形状原始向量：认领的开销取决于锚点数量，与向量含义无关。
    let shared = PathBuf::from(request["shared"].as_str().ok_or("shared missing")?);
    // 每个角色一个锚点：快照要求锚点 id 唯一。
    let anchors = request["anchors"].as_u64().ok_or("anchors missing")? as usize;
    fs::create_dir_all(shared.join("char-index"))?;
    let mut rng = Rng::new(seed ^ 0xA5A5);
    let rows: Vec<Vec<f32>> = (0..anchors).map(|_| (0..768).map(|_| rng.unit() - 0.5).collect()).collect();
    fs::write(shared.join("char-anchors.npy"), producer::npy(&rows, 768)?)?;
    let ids: Vec<String> = (0..anchors).map(|i| format!("synthetic-character-{i:05}")).collect();
    fs::write(shared.join("char-anchors.txt"), ids.join("\n") + "\n")?;
    Ok(json!({"files":files,"characters":ids}))
}

fn ms(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1000.
}

pub fn stages(request: &Value) -> Result<Value> {
    let path = |key: &str| -> Result<PathBuf> { Ok(PathBuf::from(request[key].as_str().ok_or(key.to_owned())?)) };
    let (models, runtime, resources, shared, source, output) =
        (path("models")?, path("runtime")?, path("resources")?, path("shared")?, path("source")?, path("output")?);
    let images: Vec<PathBuf> = serde_json::from_value(request["images"].clone())?;
    let identity = request["identity"].clone();
    let task: illustash_terms::app_tasks::TaskIdentity = serde_json::from_value(identity.clone())?;
    fs::create_dir_all(&output)?;

    let started = Instant::now();
    session::initialise(&runtime)?;
    // WD 和 CCIP 使用 worker 会选的后端（看 ILLUSTASH_CPU_ONLY 和设置），与生产环境相同。
    // 第一张图与 ModelSessions::infer 逐位比较，生产配置一旦偏离，基准直接失败，而不是去计时别的东西。
    let acceleration = backend::Acceleration::from_env(Some(&shared));
    let mut wd = session::WdSession::load(&models.join("wd_swinv2_v3_emb.onnx"), &runtime, &acceleration)?;
    let mut ccip = session::CcipSession::load(&models.join("ccip/model_feat.onnx"), &runtime, &acceleration)?;
    let tags = producer::Tags::load(&resources.join("tags/tags_v3.csv"))?;
    let specs = producer::model_specs(&resources, &models)?;
    let load_ms = ms(started);
    let started = Instant::now();
    let snapshot = Snapshot::load(&shared, &source)?;
    let snapshot_ms = ms(started);

    {
        let first = producer::decode(images.first().ok_or("images empty")?)?;
        let expected = session::ModelSessions::load(&models, &runtime, &acceleration)?.infer(&first, tags.len())?;
        let (probabilities, embedding) = wd.run(&preprocess::wd_input(&first, 448), tags.len())?;
        let feature = ccip.features(&first)?;
        let bits = |v: &[f32]| v.iter().map(|x| x.to_bits()).collect::<Vec<_>>();
        if bits(&probabilities) != bits(&expected.probabilities)
            || bits(&embedding) != bits(&expected.wd)
            || bits(&feature) != bits(&expected.ccip)
        {
            return Err("计时会话与生产 ModelSessions 的输出不一致".into());
        }
    }

    let mut rows = Vec::new();
    let mut packages = Vec::new();
    for (batch, chunk) in images.chunks(25).enumerate() {
        let (mut ids, mut labels, mut wd_rows, mut ccip_rows, mut claims) = (vec![], vec![], vec![], vec![], vec![]);
        let mut inputs = Vec::new();
        for (offset, image_path) in chunk.iter().enumerate() {
            let asset = format!("asset-{:05}", batch * 25 + offset);
            let t = Instant::now();
            let bytes = fs::read(image_path)?;
            let input_sha256 = format!("{:x}", Sha256::digest(&bytes));
            let image = producer::decode_bytes(&bytes, image_path)?;
            let decode_ms = ms(t);
            let t = Instant::now();
            let input = preprocess::wd_input(&image, 448);
            let wd_pre_ms = ms(t);
            let (probabilities, embedding) = wd.run(&input, tags.len())?;
            let wd_ms = ms(t);
            let t = Instant::now();
            let feature = ccip.features(&image)?;
            let ccip_ms = ms(t);
            let t = Instant::now();
            labels.extend(tags.labels(&asset, &probabilities)?);
            let labels_ms = ms(t);
            let t = Instant::now();
            claims.extend(snapshot.claims(&asset, &feature)?);
            let claims_ms = ms(t);
            rows.push(json!({"decode":decode_ms,"wd":wd_ms,"wd_preprocess":wd_pre_ms,"ccip":ccip_ms,
                "labels":labels_ms,"claims":claims_ms,"pixels":image.width()*image.height()}));
            inputs.push((asset.clone(), input_sha256));
            ids.push(asset);
            wd_rows.push(embedding);
            ccip_rows.push(feature);
        }
        let t = Instant::now();
        let path = output.join(format!("batch-{}", batch * 25));
        producer::write_batch(&path, &identity, &specs, &inputs, &labels, &wd_rows, &ccip_rows)?;
        let claim = Claims::new(task.clone(), ids.clone(), specs.clone(), snapshot.fingerprint.clone(), claims);
        let sidecar = serde_json::to_vec(&claim)?;
        fs::write(path.join("character-claims.json"), &sidecar)?;
        producer::write_complete(&path, &task.task_id, &ids, &sidecar)?;
        packages.push(json!({"assets":ids.len(),"ms":ms(t)}));
    }
    Ok(
        json!({"load_ms":load_ms,"snapshot_ms":snapshot_ms,"anchors":snapshot.vectors.len(),"images":rows,"packages":packages}),
    )
}
