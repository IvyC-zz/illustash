//! 隔离的 v1 结果生成。不写源库和应用数据库。
use super::session::{validate_vector, ModelOutput, ModelSessions};
use crate::{Error, Result};
use image::{ImageReader, RgbImage};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Deserialize)]
pub struct Request {
    pub format: String,
    pub identity: Value,
    pub assets: Vec<Asset>,
    pub output: PathBuf,
}
#[derive(Deserialize)]
pub struct Asset {
    pub id: String,
    pub path: PathBuf,
}
#[derive(Serialize)]
pub struct Label {
    pub asset: String,
    pub dim: String,
    pub value: String,
    pub confidence: f64,
}
pub struct Tags {
    rows: Vec<(String, Option<&'static str>)>,
}
impl Tags {
    pub fn load(path: &Path) -> Result<Self> {
        let mut reader = csv::Reader::from_path(path).map_err(|e| Error::invalid(e.to_string()))?;
        let headers = reader.headers().map_err(|e| Error::invalid(e.to_string()))?;
        let name = headers.iter().position(|v| v == "name").ok_or_else(|| Error::invalid("标签缺少 name"))?;
        let category =
            headers.iter().position(|v| v == "category").ok_or_else(|| Error::invalid("标签缺少 category"))?;
        let mut rows = Vec::new();
        for row in reader.records() {
            let row = row.map_err(|e| Error::invalid(e.to_string()))?;
            let dim = match row.get(category) {
                Some("0") => Some("general"),
                Some("4") => Some("character"),
                Some("9") => Some("rating"),
                _ => None,
            };
            rows.push((row.get(name).ok_or_else(|| Error::invalid("标签缺少名称"))?.to_owned(), dim));
        }
        if rows.is_empty() {
            return Err(Error::invalid("标签表为空"));
        }
        Ok(Self { rows })
    }
    pub fn len(&self) -> usize {
        self.rows.len()
    }
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
    pub fn labels(&self, asset: &str, probs: &[f32]) -> Result<Vec<Label>> {
        if probs.len() != self.rows.len() || probs.iter().any(|v| !v.is_finite() || !(0.0..=1.0).contains(v)) {
            return Err(Error::invalid("WD 概率与标签维度不匹配或非有限概率"));
        }
        Ok(self
            .rows
            .iter()
            .zip(probs)
            .filter_map(|((name, dim), prob)| {
                let dim = (*dim)?;
                // Python 在做 >= KEEP 判断和 round(x,4) 之前先把模型概率转成 f64。
                let value = f64::from(*prob);
                (value >= 0.001).then(|| Label {
                    asset: asset.to_owned(),
                    dim: dim.into(),
                    value: name.clone(),
                    confidence: (value * 10000.).round_ties_even() / 10000.,
                })
            })
            .collect())
    }
}
pub fn model_specs(resources: &Path, models: &Path) -> Result<Value> {
    super::models::ModelFiles::at(resources, models)?.specs(&|| Ok(()))
}
/// 取第一帧，不旋转的 RGB；插值前丢掉 alpha，与 Pillow 的 convert('RGB') 相同。
pub fn decode(path: &Path) -> Result<RgbImage> {
    decode_bytes(&fs::read(path)?, path)
}
/// 同步请求里的一张图片：先完整读入，这样失败可以按发生的位置归类。读取可能暂时失败（权限、iCloud 占位文件、
/// 拔掉的磁盘）；之后解码器只看内存里的字节，所以它的每个错误都关于内容本身。
pub(crate) fn load(path: &Path) -> std::result::Result<RgbImage, crate::worker_protocol::FailureReason> {
    use crate::worker_protocol::FailureReason;
    let bytes = fs::read(path).map_err(|_| FailureReason::Unreadable)?;
    decode_bytes(&bytes, path).map_err(|_| FailureReason::Undecodable)
}
/// 只在字节本身认不出格式时才用 `path` 指定格式；不从它读取任何内容。
pub fn decode_bytes(bytes: &[u8], path: &Path) -> Result<RgbImage> {
    let mut reader = ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format()?;
    // TGA 没有可靠的 magic；嗅探之后保留按文件名回退。
    if reader.format().is_none() {
        if let Ok(format) = image::ImageFormat::from_path(path) {
            reader.set_format(format);
        }
    }
    if reader.format() == Some(image::ImageFormat::Jpeg) {
        // 保留 libjpeg 的转换，不用 image 另行实现的 JPEG 解码器。
        let image = std::panic::catch_unwind(|| -> std::io::Result<RgbImage> {
            let decoder = mozjpeg::Decompress::new_reader(reader.into_inner())?;
            let cmyk = matches!(decoder.color_space(), mozjpeg::ColorSpace::JCS_CMYK | mozjpeg::ColorSpace::JCS_YCCK);
            let mut decoder =
                if cmyk { decoder.to_colorspace(mozjpeg::ColorSpace::JCS_CMYK)? } else { decoder.rgb()? };
            let (width, height) = (decoder.width() as u32, decoder.height() as u32);
            let pixels = decoder.read_scanlines::<u8>()?;
            let pixels = if cmyk {
                pixels
                    .chunks_exact(4)
                    .flat_map(|p| [0, 1, 2].map(|c| ((u32::from(p[c]) * u32::from(p[3]) + 127) / 255) as u8))
                    .collect()
            } else {
                pixels
            };
            decoder.finish()?;
            RgbImage::from_raw(width, height, pixels).ok_or_else(|| std::io::Error::other("JPEG 像素长度不匹配"))
        })
        .map_err(|_| Error::invalid("JPEG 解码失败"))??;
        Ok(image)
    } else {
        let image = reader.decode().map_err(|e| Error::invalid(format!("图片解码失败: {e}")))?;
        // Pillow 的 I;16.convert('RGB') 把整数灰度截断到 255；image::to_rgb8
        // 则按完整的 16 位范围缩放。缩放之前保持前者的行为。
        Ok(match image {
            image::DynamicImage::ImageLuma16(gray) => RgbImage::from_fn(gray.width(), gray.height(), |x, y| {
                image::Rgb([gray.get_pixel(x, y)[0].min(255) as u8; 3])
            }),
            other => other.to_rgb8(),
        })
    }
}
/// NPY v1，C 顺序小端 f32；原始值逐位保留。
pub fn npy(rows: &[Vec<f32>], dimension: usize) -> Result<Vec<u8>> {
    for row in rows {
        validate_vector(row, dimension)?;
    }
    let header = format!("{{'descr': '<f4', 'fortran_order': False, 'shape': ({}, {}), }}", rows.len(), dimension);
    let padding = (64 - (10 + header.len() + 1) % 64) % 64;
    let header = format!("{header}{}\n", " ".repeat(padding));
    let length = u16::try_from(header.len()).map_err(|_| Error::invalid("NPY header 过长"))?;
    let mut output = b"\x93NUMPY\x01\x00".to_vec();
    output.extend(length.to_le_bytes());
    output.extend(header.as_bytes());
    for row in rows {
        for value in row {
            output.extend(value.to_le_bytes());
        }
    }
    Ok(output)
}
pub fn write_batch(
    path: &Path,
    identity: &Value,
    models: &Value,
    ids: &[String],
    labels: &[Label],
    wd: &[Vec<f32>],
    ccip: &[Vec<f32>],
) -> Result<()> {
    if ids.is_empty() || ids.len() != wd.len() || ids.len() != ccip.len() {
        return Err(Error::invalid("识别批次行数不匹配"));
    }
    let library =
        identity["library_id"].as_str().filter(|s| !s.is_empty()).ok_or_else(|| Error::invalid("缺少 library_id"))?;
    let wd = npy(wd, 1024)?;
    let ccip = npy(ccip, 768)?;
    let labels = labels
        .iter()
        .map(serde_json::to_string)
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| Error::invalid(e.to_string()))?
        .join("\n");
    fs::create_dir(path)?;
    let mut files = serde_json::Map::new();
    for (name, bytes) in [("labels.jsonl", labels.as_bytes()), ("wd.npy", wd.as_slice()), ("ccip.npy", ccip.as_slice())]
    {
        fs::write(path.join(name), bytes)?;
        files.insert(name.into(), json!({"bytes":bytes.len(),"sha256":format!("{:x}",Sha256::digest(bytes))}));
    }
    // manifest 最后写；中断的批次不能导入。
    fs::write(path.join("manifest.json"),serde_json::to_vec(&json!({"format":"illustash-recognition/1","library_id":library,"assets":ids,"models":models,"files":files})).map_err(|e|Error::invalid(e.to_string()))?)?;
    Ok(())
}
/// 给等待中的宿主的进度，写在宿主通过 ILLUSTASH_RECOGNITION_PROGRESS 传入的路径。
/// 计数只是本任务已完成的尝试（`processed` 包含 `failed`）；这里的数字都不表示已导入。
/// 仅供参考：写入失败从不让识别失败，宿主会校验每个值。用改名替换，所以宿主从不读到写了一半的文件。
pub struct Progress {
    path: Option<PathBuf>,
    task_id: String,
    total: usize,
    processed: usize,
    failed: usize,
}
impl Progress {
    pub fn new(path: Option<PathBuf>, task_id: &str, total: usize) -> Self {
        Self { path, task_id: task_id.to_owned(), total, processed: 0, failed: 0 }
    }
    fn from_env(task_id: &str, total: usize) -> Self {
        Self::new(std::env::var_os("ILLUSTASH_RECOGNITION_PROGRESS").map(PathBuf::from), task_id, total)
    }
    pub fn finished(&mut self, ok: bool) {
        self.processed += 1;
        self.failed += usize::from(!ok);
    }
    pub fn write(&self, stage: &str) {
        let Some(path) = &self.path else { return };
        let value = json!({"format":"illustash-recognition-progress/1","task_id":self.task_id,"stage":stage,
            "total":self.total,"processed":self.processed,"failed":self.failed});
        let temporary = path.with_extension("tmp");
        if fs::write(&temporary, value.to_string()).is_ok() {
            let _ = fs::rename(&temporary, path);
        }
    }
}
/// 批次的最后一个文件：这个文件存在后宿主才可以导入该批次，所以从不读到写了一半的包或 sidecar。
/// 摘要把两者固定为 worker 完成时的确切内容。
pub fn write_complete(batch: &Path, task_id: &str, ids: &[String], sidecar: &[u8]) -> Result<()> {
    let manifest = fs::read(batch.join("manifest.json"))?;
    let value = json!({"format":"illustash-recognition-batch/1","task_id":task_id,"assets":ids,
        "manifest_sha256":format!("{:x}", Sha256::digest(&manifest)),
        "sidecar_sha256":format!("{:x}", Sha256::digest(sidecar))});
    let temporary = batch.join("complete.tmp");
    fs::write(&temporary, serde_json::to_vec(&value).map_err(|e| Error::invalid(e.to_string()))?)?;
    fs::rename(temporary, batch.join("complete.json"))?;
    Ok(())
}
/// 生成 WD v1 批次，以及独立绑定的角色认领 sidecar。
pub fn run(request: Request, resources: &Path, models: &Path, runtime: &Path, shared: &Path) -> Result<Value> {
    if request.format != "illustash-sync/1" {
        return Err(Error::invalid("unsupported recognition request"));
    }
    let identity: crate::app_tasks::TaskIdentity =
        serde_json::from_value(request.identity.clone()).map_err(|e| Error::invalid(format!("识别 identity: {e}")))?;
    let mut progress = Progress::from_env(&identity.task_id, request.assets.len());
    progress.write("preparing");
    #[cfg(target_os = "windows")]
    illustash_platform::platform_research_math::windows_backend_identity().map_err(Error::invalid)?;
    let snapshot = super::anchor_snapshot::Snapshot::load(shared, &identity.package.join("source.sqlite"))?;
    let specs = model_specs(resources, models)?;
    let tags = Tags::load(&resources.join("tags/tags_v3.csv"))?;
    let acceleration = super::backend::Acceleration::from_env(Some(shared));
    let mut sessions = ModelSessions::load(models, runtime, &acceleration)?;
    progress.write("recognising");
    let mut packages = Vec::new();
    let mut sidecars = Vec::new();
    let mut failed = Vec::new();
    for (batch, assets) in request.assets.chunks(25).enumerate() {
        let mut ids = Vec::new();
        let mut labels = Vec::new();
        let mut wd = Vec::new();
        let mut ccip = Vec::new();
        let mut claims = Vec::new();
        for asset in assets {
            let image = match load(&asset.path) {
                Ok(image) => image,
                Err(reason) => {
                    failed.push(json!({"uuid":asset.id,"reason":reason}));
                    progress.finished(false);
                    progress.write("recognising");
                    continue;
                }
            };
            let ModelOutput { probabilities, wd: wd_row, ccip: ccip_row } = sessions.infer(&image, tags.len())?;
            labels.extend(tags.labels(&asset.id, &probabilities)?);
            claims.extend(snapshot.claims(&asset.id, &ccip_row)?);
            ids.push(asset.id.clone());
            wd.push(wd_row);
            ccip.push(ccip_row);
            progress.finished(true);
            progress.write("recognising");
        }
        if ids.is_empty() {
            continue;
        }
        let path = request.output.join(format!("batch-{}", batch * 25));
        write_batch(&path, &request.identity, &specs, &ids, &labels, &wd, &ccip)?;
        let sidecar = path.join("character-claims.json");
        let claim = super::claims::Claims::new(
            identity.clone(),
            ids.clone(),
            specs.clone(),
            snapshot.fingerprint.clone(),
            claims,
        );
        let sidecar_bytes = serde_json::to_vec(&claim).map_err(|e| Error::invalid(e.to_string()))?;
        fs::write(&sidecar, &sidecar_bytes)?;
        write_complete(&path, &identity.task_id, &ids, &sidecar_bytes)?;
        sidecars.push(sidecar);
        packages.push(path);
    }
    Ok(json!({"identity":request.identity,"packages":packages,"failed":failed,"character_claims":sidecars}))
}

/// 适配器的接入点。共用的 worker 程序只在识别时调用它；不需要 Python 解释器。
/// 资源路径沿用现有的 app_resources 规则。
pub fn worker(arguments: &[String]) -> Result<()> {
    let resources = crate::app_resources::default_root();
    let model_files = super::models::ModelFiles::resolve(&resources)?;
    let models = &model_files.root;
    let runtime = crate::app_resources::ort_runtime(&resources);
    if arguments == ["--check"] {
        #[cfg(target_os = "windows")]
        illustash_platform::platform_research_math::windows_backend_identity().map_err(Error::invalid)?;
        let tags = Tags::load(&resources.join("tags/tags_v3.csv"))?;
        let _ = model_specs(&resources, models)?;
        // 同时建出加速会话，这样安装后的第一次同步就能用上已编译好的 CoreML 模型。
        let shared = crate::app_resources::shared_path(&resources).ok();
        let _ = ModelSessions::load(models, &runtime, &super::backend::Acceleration::from_env(shared.as_deref()))?;
        if tags.is_empty() {
            return Err(Error::invalid("标签表为空"));
        }
        return Ok(());
    }
    if arguments.len() != 2 {
        return Err(Error::invalid("需要 request.json response.json 或 --check"));
    }
    let request: Request = crate::worker_protocol::read(Path::new(&arguments[0]))?;
    let _: crate::app_tasks::TaskIdentity =
        serde_json::from_value(request.identity.clone()).map_err(|e| Error::invalid(format!("识别 identity: {e}")))?;
    let shared = crate::app_resources::shared_path(&resources)?;
    let result = run(request, &resources, models, &runtime, &shared)?;
    fs::write(&arguments[1], serde_json::to_vec(&result).map_err(|e| Error::invalid(e.to_string()))?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sync_pictures_fail_as_unreadable_or_undecodable_by_where_the_error_happened() {
        use crate::worker_protocol::FailureReason::{Undecodable, Unreadable};
        let root = std::env::temp_dir().join(format!("illustash-load-{}", crate::store::new_key().unwrap()));
        fs::create_dir(&root).unwrap();
        let png = root.join("good.png");
        image::RgbImage::from_fn(32, 24, |x, y| image::Rgb([x as u8 * 7, y as u8 * 9, 128])).save(&png).unwrap();
        let jpeg = root.join("good.jpg");
        let rgb = image::RgbImage::from_fn(32, 24, |x, y| image::Rgb([x as u8 * 7, y as u8 * 9, 128]));
        let mut encoder = mozjpeg::Compress::new(mozjpeg::ColorSpace::JCS_RGB);
        encoder.set_size(32, 24);
        let mut encoder = encoder.start_compress(Vec::new()).unwrap();
        encoder.write_scanlines(rgb.as_raw()).unwrap();
        fs::write(&jpeg, encoder.finish().unwrap()).unwrap();
        assert_eq!(load(&png).unwrap(), decode(&png).unwrap());
        assert_eq!(load(&jpeg).unwrap(), decode(&jpeg).unwrap());
        let bytes = fs::read(&png).unwrap();
        let cases = [
            ("truncated.png", bytes[..bytes.len() / 2].to_vec()),
            ("header-only.jpg", fs::read(&jpeg).unwrap()[..20].to_vec()),
            ("truncated.jpg", {
                let b = fs::read(&jpeg).unwrap();
                b[..b.len() / 2].to_vec()
            }),
            ("text.jpg", b"not an image".to_vec()),
            ("empty.png", Vec::new()),
            ("unknown.webp", b"RIFF\0\0\0\0WEBPVP8 ".to_vec()),
        ];
        for (name, content) in cases {
            fs::write(root.join(name), content).unwrap();
            assert_eq!(load(&root.join(name)).err(), Some(Undecodable), "{name}");
        }
        assert_eq!(load(&root.join("missing.png")).err(), Some(Unreadable));
        assert_eq!(load(&root).err(), Some(Unreadable), "a directory cannot be read as a file");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let locked = root.join("locked.png");
            fs::copy(&png, &locked).unwrap();
            fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
            // root 不受权限位限制；在 root 下这个文件照样可读。
            if fs::read(&locked).is_err() {
                assert_eq!(load(&locked).err(), Some(Unreadable));
            }
            fs::set_permissions(&locked, fs::Permissions::from_mode(0o600)).unwrap();
        }
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn wd_keep_cast_and_round_match_python_boundaries() {
        let tags = Tags {
            rows: vec![
                ("below".into(), Some("general")),
                ("boundary".into(), Some("character")),
                ("tie".into(), Some("rating")),
                ("ignored".into(), None),
            ],
        };
        let boundary = 0.001f32;
        let labels = tags.labels("asset", &[f32::from_bits(boundary.to_bits() - 1), boundary, 0.03125, 0.5]).unwrap();
        assert_eq!(labels.len(), 2);
        assert_eq!(labels[0].value, "boundary");
        assert_eq!(labels[0].confidence, 0.001);
        assert_eq!(labels[1].confidence, 0.0312);
        assert!(tags.labels("asset", &[1.]).is_err());
        assert!(tags.labels("asset", &[f32::NAN; 4]).is_err());
    }
    #[test]
    fn raw_vector_npy_preserves_bits_and_refuses_invalid_rows() {
        let values = vec![vec![0.03125, -0., 123.456]];
        let bytes = npy(&values, 3).unwrap();
        let length = u16::from_le_bytes([bytes[8], bytes[9]]) as usize;
        assert_eq!((length + 10) % 64, 0);
        assert_eq!(&bytes[length + 10..], values[0].iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<_>>());
        assert!(npy(&[vec![0.; 3]], 3).is_err());
        assert!(npy(&[vec![f32::INFINITY; 3]], 3).is_err());
        assert!(npy(&values, 2).is_err());
    }
    #[test]
    fn progress_counts_attempts_and_replaces_the_whole_file() {
        let root = std::env::temp_dir().join(format!("illustash-recognition-progress-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("progress.json");
        let mut progress = Progress::new(Some(path.clone()), "task", 3);
        progress.write("preparing");
        let read = || serde_json::from_slice::<Value>(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(read()["stage"], "preparing");
        progress.finished(true);
        progress.finished(false);
        progress.write("recognising");
        let value = read();
        assert_eq!(
            (value["task_id"].as_str(), value["total"].as_u64(), value["processed"].as_u64(), value["failed"].as_u64()),
            (Some("task"), Some(3), Some(2), Some(1))
        );
        assert!(!root.join("progress.tmp").exists());
        // 没有宿主路径，或路径不可写时，进度直接跳过，不报错。
        Progress::new(None, "task", 1).write("preparing");
        Progress::new(Some(root.join("missing/progress.json")), "task", 1).write("preparing");
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn unsupported_format_fails_before_model_or_output_access() {
        let root = std::env::temp_dir().join(format!("illustash-native-recognition-unit-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let request =
            || Request { format: "bad".into(), identity: json!({}), assets: Vec::new(), output: root.join("output") };
        assert!(run(request(), &root, &root, &root, &root).unwrap_err().to_string().contains("unsupported"));
        assert!(!root.join("output").exists());
        fs::remove_dir_all(root).unwrap();
    }
}
