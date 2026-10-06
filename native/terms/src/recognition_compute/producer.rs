//! Isolated v1 result production. No source/application database writes.
use super::session::{validate_vector, ModelOutput, ModelSessions};
use crate::{Error, Result};
use image::{ImageReader, RgbImage};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::BufReader,
    path::{Path, PathBuf},
};

fn invalid(message: impl Into<String>) -> Error {
    Error::Invalid(message.into())
}
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
        let mut reader = csv::Reader::from_path(path).map_err(|e| invalid(e.to_string()))?;
        let headers = reader.headers().map_err(|e| invalid(e.to_string()))?;
        let name = headers.iter().position(|v| v == "name").ok_or_else(|| invalid("标签缺少 name"))?;
        let category = headers.iter().position(|v| v == "category").ok_or_else(|| invalid("标签缺少 category"))?;
        let mut rows = Vec::new();
        for row in reader.records() {
            let row = row.map_err(|e| invalid(e.to_string()))?;
            let dim = match row.get(category) {
                Some("0") => Some("general"),
                Some("4") => Some("character"),
                Some("9") => Some("rating"),
                _ => None,
            };
            rows.push((row.get(name).ok_or_else(|| invalid("标签缺少名称"))?.to_owned(), dim));
        }
        if rows.is_empty() {
            return Err(invalid("标签表为空"));
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
            return Err(invalid("WD 概率与标签维度不匹配或非有限概率"));
        }
        Ok(self
            .rows
            .iter()
            .zip(probs)
            .filter_map(|((name, dim), prob)| {
                let dim = (*dim)?;
                // Python casts model probabilities to f64 before >= KEEP and round(x,4).
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
/// First frame, unrotated RGB; drop alpha before interpolation, as Pillow convert('RGB').
pub fn decode(path: &Path) -> Result<RgbImage> {
    let mut reader = ImageReader::new(BufReader::new(fs::File::open(path)?)).with_guessed_format()?;
    // TGA has no dependable magic; retain the filename fallback after sniffing.
    if reader.format().is_none() {
        if let Ok(format) = image::ImageFormat::from_path(path) {
            reader.set_format(format);
        }
    }
    if reader.format() == Some(image::ImageFormat::Jpeg) {
        // Preserve libjpeg conversion instead of image's separately implemented JPEG decoder.
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
        .map_err(|_| invalid("JPEG 解码失败"))??;
        Ok(image)
    } else {
        let image = reader.decode().map_err(|e| invalid(format!("图片解码失败: {e}")))?;
        // Pillow I;16.convert('RGB') clamps integer grayscale to 255; image::to_rgb8
        // instead scales the full 16-bit range. Preserve the former before resize.
        Ok(match image {
            image::DynamicImage::ImageLuma16(gray) => RgbImage::from_fn(gray.width(), gray.height(), |x, y| {
                image::Rgb([gray.get_pixel(x, y)[0].min(255) as u8; 3])
            }),
            other => other.to_rgb8(),
        })
    }
}
/// NPY v1, C-order little-endian f32; raw values are preserved bit for bit.
pub fn npy(rows: &[Vec<f32>], dimension: usize) -> Result<Vec<u8>> {
    for row in rows {
        validate_vector(row, dimension)?;
    }
    let header = format!("{{'descr': '<f4', 'fortran_order': False, 'shape': ({}, {}), }}", rows.len(), dimension);
    let padding = (64 - (10 + header.len() + 1) % 64) % 64;
    let header = format!("{header}{}\n", " ".repeat(padding));
    let length = u16::try_from(header.len()).map_err(|_| invalid("NPY header 过长"))?;
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
        return Err(invalid("识别批次行数不匹配"));
    }
    let library =
        identity["library_id"].as_str().filter(|s| !s.is_empty()).ok_or_else(|| invalid("缺少 library_id"))?;
    let wd = npy(wd, 1024)?;
    let ccip = npy(ccip, 768)?;
    let labels = labels
        .iter()
        .map(serde_json::to_string)
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| invalid(e.to_string()))?
        .join("\n");
    fs::create_dir(path)?;
    let mut files = serde_json::Map::new();
    for (name, bytes) in [("labels.jsonl", labels.as_bytes()), ("wd.npy", wd.as_slice()), ("ccip.npy", ccip.as_slice())]
    {
        fs::write(path.join(name), bytes)?;
        files.insert(name.into(), json!({"bytes":bytes.len(),"sha256":format!("{:x}",Sha256::digest(bytes))}));
    }
    // Manifest is written last; an interrupted batch is not importable.
    fs::write(path.join("manifest.json"),serde_json::to_vec(&json!({"format":"illustash-recognition/1","library_id":library,"assets":ids,"models":models,"files":files})).map_err(|e|invalid(e.to_string()))?)?;
    Ok(())
}
/// Produce WD v1 batches and independently bound character-claim sidecars.
pub fn run(request: Request, resources: &Path, models: &Path, runtime: &Path, shared: &Path) -> Result<Value> {
    if request.format != "illustash-sync/1" {
        return Err(invalid("unsupported recognition request"));
    }
    let identity: crate::app_tasks::TaskIdentity =
        serde_json::from_value(request.identity.clone()).map_err(|e| invalid(format!("识别 identity: {e}")))?;
    #[cfg(target_os = "windows")]
    illustash_platform::platform_research_math::windows_backend_identity().map_err(invalid)?;
    let snapshot = super::anchor_snapshot::Snapshot::load(shared, &identity.package.join("source.sqlite"))?;
    let specs = model_specs(resources, models)?;
    let tags = Tags::load(&resources.join("tags/tags_v3.csv"))?;
    let mut sessions = ModelSessions::load(models, runtime)?;
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
            let image = match decode(&asset.path) {
                Ok(image) => image,
                Err(_) => {
                    failed.push(json!({"uuid":asset.id}));
                    continue;
                }
            };
            let ModelOutput { probabilities, wd: wd_row, ccip: ccip_row } = sessions.infer(&image, tags.len())?;
            labels.extend(tags.labels(&asset.id, &probabilities)?);
            claims.extend(snapshot.claims(&asset.id, &ccip_row)?);
            ids.push(asset.id.clone());
            wd.push(wd_row);
            ccip.push(ccip_row);
        }
        if ids.is_empty() {
            continue;
        }
        let path = request.output.join(format!("batch-{}", batch * 25));
        write_batch(&path, &request.identity, &specs, &ids, &labels, &wd, &ccip)?;
        let sidecar = path.join("character-claims.json");
        let claim =
            super::claims::Claims::new(identity.clone(), ids, specs.clone(), snapshot.fingerprint.clone(), claims);
        fs::write(&sidecar, serde_json::to_vec(&claim).map_err(|e| invalid(e.to_string()))?)?;
        sidecars.push(sidecar);
        packages.push(path);
    }
    Ok(json!({"identity":request.identity,"packages":packages,"failed":failed,"character_claims":sidecars}))
}

/// Adapter integration point. The shared worker binary calls this for recognition only;
/// no Python interpreter is needed. Resource paths share the existing app_resources policy.
pub fn worker(arguments: &[String]) -> Result<()> {
    let resources = crate::app_resources::default_root();
    let model_files = super::models::ModelFiles::resolve(&resources)?;
    let models = &model_files.root;
    let library = if cfg!(windows) {
        "onnxruntime.dll"
    } else if cfg!(target_os = "macos") {
        "libonnxruntime.dylib"
    } else {
        "libonnxruntime.so"
    };
    let runtime = std::env::var_os("ILLUSTASH_ORT_DYLIB")
        .map(PathBuf::from)
        .unwrap_or_else(|| resources.join("onnxruntime").join(library));
    if arguments == ["--check"] {
        #[cfg(target_os = "windows")]
        illustash_platform::platform_research_math::windows_backend_identity().map_err(invalid)?;
        let tags = Tags::load(&resources.join("tags/tags_v3.csv"))?;
        let _ = model_specs(&resources, models)?;
        let _ = ModelSessions::load(models, &runtime)?;
        if tags.is_empty() {
            return Err(invalid("标签表为空"));
        }
        return Ok(());
    }
    if arguments.len() != 2 {
        return Err(invalid("需要 request.json response.json 或 --check"));
    }
    let request: Request = crate::worker_protocol::read(Path::new(&arguments[0]))?;
    let _: crate::app_tasks::TaskIdentity =
        serde_json::from_value(request.identity.clone()).map_err(|e| invalid(format!("识别 identity: {e}")))?;
    let shared = crate::app_resources::shared_path(&resources)?;
    let result = run(request, &resources, models, &runtime, &shared)?;
    fs::write(&arguments[1], serde_json::to_vec(&result).map_err(|e| invalid(e.to_string()))?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
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
