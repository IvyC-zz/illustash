//! 基础角色索引包（#607）：DeepGHS Character Index 固定版本算出的锚点、阈值和角色元数据。
//!
//! 包随识别模型的下载清单分发（`native/terms/models.json` 的 `character_index`），和 WD 加速文件一样是可选文件：
//! 缺它不挡识别。包由 `tools/build_character_index.py` 制作，内容是不压缩的 ZIP：`provenance.json`、
//! `characters.json`（按 short_tag 排序）、`anchors.f32`（与角色同序的 768 维小端 f32）和许可文本。
//! 索引任务把核对过的包复制进任务目录（`JOB_FILE`），构建时把其中的角色当作精选条目并进共享索引；
//! 共享目录里同名的 `char-index/*.json` 优先。识别读到的仍是共享锚点和图库角色表，没有另一条路径。
use crate::{Error, Result};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::{
    collections::HashSet,
    fs,
    io::{Cursor, Read, Write},
    path::Path,
};

/// 任务目录里的包文件名。
pub(crate) const JOB_FILE: &str = "character-index.bin";
/// 包格式。
pub(crate) const FORMAT: &str = "illustash-character-index/1";
const DIMENSION: usize = 768;

/// 清单里登记的版本：大小、SHA-256、角色数和兼容的 CCIP 模型。
#[derive(Deserialize, Clone, Debug)]
pub struct Release {
    pub revision: u32,
    pub format: String,
    /// 写进候选来源 `model_ver` 的上游版本（与已有图库的 `character_index@2025-09-25` 相同）。
    pub version: String,
    pub characters: usize,
    pub ccip_sha256: String,
    pub license: String,
}

/// 包里的一个角色：共享索引 `char-index/<id>.json` 同样的元数据，加上预先算好的锚点和阈值。
pub(crate) struct Entry {
    pub id: String,
    /// 元数据对象（不含 gender，gender 的原顺序见 `gender`）。
    pub meta: Map<String, Value>,
    /// 原文件里的顺序：并列最大时取哪个是可观察的（index_compute/build.rs）。
    pub gender: Vec<(String, f64)>,
    pub n_ref: i64,
    pub thresholds: [f64; 3],
    pub anchor: Vec<f32>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEntry {
    short_tag: String,
    tag: String,
    copyright: Option<String>,
    #[serde(default)]
    copyrights: Vec<String>,
    #[serde(default)]
    core_tags: Vec<String>,
    gender: Gender,
    post_count: i64,
    n_ref: i64,
    anchor_p50: f64,
    anchor_p75: f64,
    anchor_p90: f64,
}

struct Gender(Vec<(String, f64)>);
impl<'de> Deserialize<'de> for Gender {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = Gender;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("gender object")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(self, mut a: A) -> std::result::Result<Self::Value, A::Error> {
                let mut values = vec![];
                while let Some(pair) = a.next_entry::<String, f64>()? {
                    values.push(pair);
                }
                Ok(Gender(values))
            }
        }
        d.deserialize_map(Visitor)
    }
}

fn bad() -> Error {
    Error::invalid("基础角色索引包与支持的版本不符")
}

fn safe(id: &str) -> bool {
    !id.is_empty() && id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
}

/// 清单（或测试覆盖后）登记的版本和文件。
pub(crate) fn release() -> Option<(&'static Release, &'static crate::app_models::Entry)> {
    crate::app_models::manifest().character_index.as_ref().map(|spec| (&spec.release, &spec.file))
}

/// 模型根目录下包的位置；清单不带基础索引时为 None。
pub(crate) fn package_path(resources: &Path) -> Option<std::path::PathBuf> {
    release().map(|(_, file)| crate::app_models::root(resources).join(&file.path))
}

/// 包已在模型目录里且大小对得上（不算哈希，供轮询用）；真正使用前由 `copy_verified` 核对 SHA-256。
pub(crate) fn present(resources: &Path) -> bool {
    match (release(), package_path(resources)) {
        (Some((_, file)), Some(path)) => fs::symlink_metadata(path).is_ok_and(|m| m.is_file() && m.len() == file.size),
        _ => false,
    }
}

/// 读出并核对模型目录里的包，写到任务目录 `destination`；缺失或对不上时返回说明原因的错误，不写任何东西。
pub(crate) fn copy_verified(resources: &Path, destination: &Path) -> Result<String> {
    let (_, file) = release().ok_or_else(|| Error::invalid("这个版本没有基础角色索引"))?;
    let path = package_path(resources).ok_or_else(|| Error::invalid("这个版本没有基础角色索引"))?;
    let bytes = match crate::app_recognition::file(&path, file.size) {
        Ok(bytes) => bytes,
        Err(_) => return Err(Error::Conflict(MISSING.into())),
    };
    if bytes.len() as u64 != file.size || crate::strict_json::sha256_hex(&bytes) != file.sha256 {
        return Err(Error::Conflict(MISSING.into()));
    }
    let tmp = destination.with_extension(format!("{}.tmp", crate::store::new_key()?));
    let result = (|| {
        let mut out = fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        out.write_all(&bytes)?;
        out.sync_all()?;
        drop(out);
        fs::rename(&tmp, destination)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result.map(|()| file.sha256.clone())
}

/// 识别模型目录里的基础索引缺失或损坏时的说明。
pub(crate) const MISSING: &str = "基础角色索引文件缺失或损坏，请在「设置 › 数据 › 识别模型」里下载或修复";

/// 读任务目录里的包（索引 worker 用）；没有包时为 None。
pub(crate) fn load_job(review: &Path) -> Result<Option<Vec<Entry>>> {
    let path = review.join(JOB_FILE);
    if !path.try_exists()? {
        return Ok(None);
    }
    let (release, file) = release().ok_or_else(bad)?;
    let bytes = crate::app_recognition::file(&path, file.size).map_err(|_| bad())?;
    if bytes.len() as u64 != file.size || crate::strict_json::sha256_hex(&bytes) != file.sha256 {
        return Err(bad());
    }
    parse(&bytes, release).map(Some)
}

/// 解析并逐项校验包内容。
pub(crate) fn parse(bytes: &[u8], release: &Release) -> Result<Vec<Entry>> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|_| bad())?;
    let mut member = |name: &str, maximum: u64| -> Result<Vec<u8>> {
        let entry = archive.by_name(name).map_err(|_| bad())?;
        if entry.size() > maximum {
            return Err(bad());
        }
        let mut contents = Vec::new();
        entry.take(maximum + 1).read_to_end(&mut contents)?;
        if contents.len() as u64 > maximum {
            return Err(bad());
        }
        Ok(contents)
    };
    let provenance: Value = serde_json::from_slice(&member("provenance.json", 64 * 1024)?).map_err(|_| bad())?;
    if provenance["format"] != FORMAT
        || release.format != FORMAT
        || provenance["version"] != release.version.as_str()
        || provenance["ccip_sha256"] != release.ccip_sha256.as_str()
        || provenance["characters"] != release.characters
        || provenance["dimension"] != DIMENSION
    {
        return Err(bad());
    }
    let characters = member("characters.json", 64 << 20)?;
    let packed = member("anchors.f32", (release.characters * DIMENSION * size_of::<f32>()) as u64)?;
    for (name, value) in [("characters.json", &characters), ("anchors.f32", &packed)] {
        if provenance["files"][name] != crate::strict_json::sha256_hex(value) {
            return Err(bad());
        }
    }
    let raw: Vec<RawEntry> = serde_json::from_slice(&characters).map_err(|_| bad())?;
    if raw.len() != release.characters || packed.len() != release.characters * DIMENSION * size_of::<f32>() {
        return Err(bad());
    }
    let mut seen = HashSet::new();
    let mut previous: Option<String> = None;
    raw.into_iter()
        .zip(packed.chunks_exact(DIMENSION * size_of::<f32>()))
        .map(|(entry, bytes)| {
            let [p50, p75, p90] = [entry.anchor_p50, entry.anchor_p75, entry.anchor_p90];
            if !safe(&entry.short_tag)
                || !seen.insert(entry.short_tag.clone())
                || previous.as_ref().is_some_and(|p| p >= &entry.short_tag)
                || entry.tag.is_empty()
                || entry.post_count < 0
                || entry.n_ref < 3
                || ![p50, p75, p90].iter().all(|p| p.is_finite() && (0.0..=1.0).contains(p))
                || !(p50 <= p75 && p75 <= p90 && p90 > 0.)
                || entry.gender.0.iter().any(|(_, v)| !v.is_finite())
            {
                return Err(bad());
            }
            previous = Some(entry.short_tag.clone());
            let anchor = bytes
                .chunks_exact(size_of::<f32>())
                .map(|b| f32::from_le_bytes(b.try_into().expect("每个浮点数为四字节")))
                .collect::<Vec<_>>();
            if anchor.iter().any(|v| !v.is_finite()) || anchor.iter().all(|v| *v == 0.) {
                return Err(bad());
            }
            let mut meta = Map::new();
            meta.insert("short_tag".into(), json!(entry.short_tag));
            meta.insert("tag".into(), json!(entry.tag));
            meta.insert("copyright".into(), json!(entry.copyright));
            meta.insert("copyrights".into(), json!(entry.copyrights));
            meta.insert("core_tags".into(), json!(entry.core_tags));
            meta.insert("post_count".into(), json!(entry.post_count));
            meta.insert("source".into(), json!("character-index"));
            Ok(Entry {
                id: entry.short_tag,
                meta,
                gender: entry.gender.0,
                n_ref: entry.n_ref,
                thresholds: [p50, p75, p90],
                anchor,
            })
        })
        .collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// 合成包：`n` 个角色，第 i 个锚点在第 i 维上。与制包脚本同样的成员和格式。
    pub(crate) fn synthetic(n: usize, mutate: impl Fn(&mut Value, &mut Vec<u8>)) -> (Vec<u8>, Release) {
        let release = Release {
            revision: 1,
            format: FORMAT.into(),
            version: "character_index@2025-09-25".into(),
            characters: n,
            ccip_sha256: "c".repeat(64),
            license: "MIT".into(),
        };
        let mut rows = vec![];
        let mut packed = vec![];
        for i in 0..n {
            rows.push(
                json!({"short_tag":format!("base_{i:03}"),"tag":format!("base_{i:03}_(work)"),"copyright":"work",
                "copyrights":["work"],"core_tags":["synthetic_hair"],"gender":{"boy":0.0,"girl":1.0},"post_count":300+i,
                "n_ref":5,"anchor_p50":0.02,"anchor_p75":0.03,"anchor_p90":0.05}),
            );
            for d in 0..DIMENSION {
                packed.extend_from_slice(&(if d == i { 1f32 } else { 0. }).to_le_bytes());
            }
        }
        let mut characters = json!(rows);
        mutate(&mut characters, &mut packed);
        let characters = serde_json::to_vec(&characters).expect("合成元数据可序列化");
        let provenance = json!({"format":FORMAT,"version":release.version,"characters":n,"dimension":DIMENSION,
            "ccip_sha256":release.ccip_sha256,"files":{"characters.json":crate::strict_json::sha256_hex(&characters),
            "anchors.f32":crate::strict_json::sha256_hex(&packed)}});
        let mut out = Cursor::new(Vec::new());
        let mut zip = zip::ZipWriter::new(&mut out);
        let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for (name, data) in [
            ("provenance.json", serde_json::to_vec(&provenance).expect("合成来源可序列化")),
            ("characters.json", characters),
            ("anchors.f32", packed),
        ] {
            zip.start_file(name, options).expect("写入合成包");
            zip.write_all(&data).expect("写入合成包");
        }
        zip.finish().expect("写入合成包");
        (out.into_inner(), release)
    }

    #[test]
    fn parses_synthetic_package_in_order() {
        let (bytes, release) = synthetic(3, |_, _| {});
        let entries = parse(&bytes, &release).unwrap();
        assert_eq!(entries.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), ["base_000", "base_001", "base_002"]);
        assert_eq!(entries[1].anchor[1], 1.);
        assert_eq!(entries[0].gender, vec![("boy".into(), 0.), ("girl".into(), 1.)]);
        assert_eq!(entries[2].thresholds, [0.02, 0.03, 0.05]);
        assert_eq!(entries[0].meta["source"], "character-index");
    }

    #[test]
    fn rejects_tampered_or_mismatched_packages() {
        let (bytes, release) = synthetic(2, |_, _| {});
        let other = Release { characters: 3, ..release.clone() };
        assert!(parse(&bytes, &other).is_err());
        let other = Release { ccip_sha256: "d".repeat(64), ..release.clone() };
        assert!(parse(&bytes, &other).is_err());
        let other = Release { version: "character_index@2026-01-01".into(), ..release.clone() };
        assert!(parse(&bytes, &other).is_err());
        assert!(parse(&bytes[..bytes.len() - 10], &release).is_err());
        // 成员内容改动后与来源记录的 SHA-256 对不上。
        let mut tampered = bytes.clone();
        let at = tampered.windows(4).position(|w| w == 1f32.to_le_bytes()).unwrap();
        tampered[at] ^= 1;
        assert!(parse(&tampered, &release).is_err());
        for broken in [
            synthetic(2, |c, _| c[0]["anchor_p90"] = json!(0.01)).0,
            synthetic(2, |c, _| c[1]["short_tag"] = json!("base_000")).0,
            synthetic(2, |c, _| c[0]["short_tag"] = json!("../x")).0,
            synthetic(2, |c, _| c[0]["n_ref"] = json!(2)).0,
            synthetic(2, |_, p| p[0..4].copy_from_slice(&f32::NAN.to_le_bytes())).0,
            synthetic(2, |_, p| p.iter_mut().for_each(|b| *b = 0)).0,
        ] {
            assert!(parse(&broken, &release).is_err());
        }
    }
}
