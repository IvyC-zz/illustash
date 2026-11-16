//! 一份经授权的任务快照。写入锁和接收锁内不做特征缓存，也不做平方级的计算。
use super::{
    anchors::{self, AnchorIndex, Claim, Metadata},
    models::hash,
};
use crate::{Error, Result};
use illustash_core::ccip_longtail::Hit;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::Read,
    path::Path,
};
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fingerprint {
    pub vectors: Option<String>,
    pub ids: Option<String>,
    pub metadata: Option<String>,
    pub patches: Option<String>,
}
fn absent() -> Fingerprint {
    Fingerprint { vectors: None, ids: None, metadata: None, patches: None }
}
pub struct Snapshot {
    pub fingerprint: Fingerprint,
    pub vectors: Vec<Vec<f32>>,
    pub index: AnchorIndex,
    pub limits: Vec<f32>,
}
pub(crate) struct Reference {
    pub id: String,
    pub tag: String,
    pub vector: Vec<f32>,
    pub metadata: Metadata,
}

struct Input {
    fingerprint: Fingerprint,
    vectors: Vec<Vec<f32>>,
    ids: Vec<String>,
    metadata: HashMap<String, Metadata>,
    patched: HashSet<String>,
}

impl Input {
    fn build(self, check: &dyn Fn() -> Result<()>) -> Result<Snapshot> {
        let index = anchors::anchor_index_checked(&self.vectors, &self.ids, &self.metadata, check)?;
        let limits = anchors::claim_thresholds(&self.ids, &index, &self.patched)?;
        Ok(Snapshot { fingerprint: self.fingerprint, vectors: self.vectors, index, limits })
    }
}

pub(crate) struct LongtailSnapshot {
    pub anchors: Snapshot,
    tags: HashMap<String, String>,
}

impl LongtailSnapshot {
    pub(crate) fn load(
        shared: &Path,
        source: &Connection,
        references: &[Reference],
        check: &dyn Fn() -> Result<()>,
    ) -> Result<Self> {
        crate::index_generation::stable(shared, || {
            let mut input = load_input(shared, source, check)?;
            if input.ids.is_empty() {
                return Err(Error::Conflict("请先安装基础角色索引".into()));
            }
            let mut known =
                source.prepare_cached("SELECT danbooru_tag FROM characters WHERE danbooru_tag IS NOT NULL")?;
            let roles = known.query_map([], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<HashSet<_>>>()?;
            let mut tags = HashMap::new();
            for reference in references {
                check()?;
                if input.metadata.contains_key(&reference.id) || roles.contains(&reference.tag) {
                    continue;
                }
                input.ids.push(reference.id.clone());
                input.vectors.push(reference.vector.clone());
                input.metadata.insert(reference.id.clone(), reference.metadata.clone());
                input.patched.insert(reference.id.clone());
                tags.insert(reference.id.clone(), reference.tag.clone());
            }
            if !tags.is_empty() {
                let mut ordered = input.ids.into_iter().zip(input.vectors).collect::<Vec<_>>();
                ordered.sort_unstable_by(|a, b| a.0.cmp(&b.0));
                (input.ids, input.vectors) = ordered.into_iter().unzip();
            }
            Ok(Self { anchors: input.build(check)?, tags })
        })
    }

    pub(crate) fn scan(
        &self,
        rows: &[(String, Vec<f32>)],
        check: &dyn Fn() -> Result<()>,
        progress: &dyn Fn(usize),
    ) -> Result<Vec<Hit>> {
        let mut hits = Vec::new();
        if self.tags.is_empty() {
            progress(rows.len());
            return Ok(hits);
        }
        // 与全索引重扫使用同一分块尺寸和最近九项。
        for (part, batch) in rows.chunks(4000).enumerate() {
            let vectors = batch.iter().map(|(_, vector)| vector.clone()).collect::<Vec<_>>();
            let distances = anchors::cross_checked(&self.anchors.vectors, &vectors, check)?;
            for (position, (uuid, _)) in batch.iter().enumerate() {
                check()?;
                let column = distances.iter().map(|vector| vector[position]).collect::<Vec<_>>();
                let mut seen = HashSet::new();
                for i in crate::index_compute::numeric::nearest_nine(&column) {
                    let entity = &self.anchors.index.entities[i];
                    if !seen.insert(entity) {
                        continue;
                    }
                    let distance = column[i];
                    let limit = self.anchors.limits[i];
                    if limit > 0. && distance <= limit {
                        if let Some(tag) = self.tags.get(entity) {
                            let rounded = format!("{:.4}", f64::from(distance));
                            let score = 1. - rounded.parse::<f64>().map_err(|e| Error::invalid(e.to_string()))?;
                            hits.push(Hit { uuid: uuid.clone(), tag: tag.clone(), score });
                        }
                    }
                }
            }
            progress(part * 4000 + batch.len());
        }
        Ok(hits)
    }
}

fn anchored(shared: &Path) -> Result<bool> {
    let a = shared.join("char-anchors.npy").try_exists()?;
    let b = shared.join("char-anchors.txt").try_exists()?;
    if a != b {
        return Err(Error::invalid("角色锚点必须同时有向量和ID文件"));
    }
    Ok(a)
}
fn read(path: &Path, check: &dyn Fn() -> Result<()>) -> Result<Vec<u8>> {
    let mut file = fs::File::open(path)?;
    let mut out = Vec::new();
    let mut buffer = [0; 65536];
    loop {
        check()?;
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        out.extend_from_slice(&buffer[..n]);
    }
    Ok(out)
}
impl Snapshot {
    pub fn load(shared: &Path, source: &Path) -> Result<Self> {
        Self::load_checked(shared, source, &|| Ok(()))
    }
    /// 来自同一次完整索引发布（index_generation）的锚点、id 和逐角色文件。
    pub(crate) fn load_checked(shared: &Path, source: &Path, check: &dyn Fn() -> Result<()>) -> Result<Self> {
        crate::index_generation::stable(shared, || {
            check()?;
            if !anchored(shared)? {
                return Ok(Snapshot {
                    fingerprint: absent(),
                    vectors: vec![],
                    index: AnchorIndex { entities: vec![], p90: vec![], p50: vec![], p75: vec![] },
                    limits: vec![],
                });
            }
            let source = crate::sqlite::open_with_flags(source, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
            load_input(shared, &source, check)?.build(check)
        })
    }
    /// 在源库租约下做线性的权威复查：不构建矩阵或索引。
    pub(crate) fn fingerprint(
        shared: &Path,
        source: &Connection,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<Fingerprint> {
        crate::index_generation::stable(shared, || Self::fingerprint_once(shared, source, check))
    }
    fn fingerprint_once(shared: &Path, source: &Connection, check: &dyn Fn() -> Result<()>) -> Result<Fingerprint> {
        check()?;
        if !anchored(shared)? {
            return Ok(absent());
        }
        let vectors = hash(&shared.join("char-anchors.npy"), check)?;
        let ids = hash(&shared.join("char-anchors.txt"), check)?;
        let (_, metadata) = metadata(source, check)?;
        let (_, patches) = patches(shared, check)?;
        Ok(Fingerprint { vectors: Some(vectors), ids: Some(ids), metadata: Some(metadata), patches: Some(patches) })
    }
    pub fn claims(&self, asset: &str, feature: &[f32]) -> Result<Vec<Claim>> {
        self.claims_checked(asset, feature, &|| Ok(()))
    }
    pub(crate) fn claims_checked(
        &self,
        asset: &str,
        feature: &[f32],
        check: &dyn Fn() -> Result<()>,
    ) -> Result<Vec<Claim>> {
        anchors::claims_checked(asset, &self.vectors, feature, &self.index, &self.limits, check)
    }
}
fn load_input(shared: &Path, source: &Connection, check: &dyn Fn() -> Result<()>) -> Result<Input> {
    check()?;
    if !anchored(shared)? {
        return Ok(Input {
            fingerprint: absent(),
            vectors: vec![],
            ids: vec![],
            metadata: HashMap::new(),
            patched: HashSet::new(),
        });
    }
    let id_bytes = read(&shared.join("char-anchors.txt"), check)?;
    let ids = std::str::from_utf8(&id_bytes)
        .map_err(|e| Error::invalid(e.to_string()))?
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect::<Vec<_>>();
    let bytes = read(&shared.join("char-anchors.npy"), check)?;
    let digest = crate::strict_json::sha256_hex(&bytes);
    let raw = crate::app_recognition::vectors_checked(bytes, ids.len(), 768, check)?;
    let matrix = raw
        .chunks_exact(768 * 4)
        .map(|row| {
            check()?;
            Ok(row.chunks_exact(4).map(|b| f32::from_le_bytes(b.try_into().expect("four-byte chunk"))).collect())
        })
        .collect::<Result<Vec<Vec<f32>>>>()?;
    let (metadata, metadata_hash) = metadata(source, check)?;
    let (patched, patch_hash) = patches(shared, check)?;
    let fingerprint = Fingerprint {
        vectors: Some(digest),
        ids: Some(crate::strict_json::sha256_hex(&id_bytes)),
        metadata: Some(metadata_hash),
        patches: Some(patch_hash),
    };
    Ok(Input { fingerprint, vectors: matrix, ids, metadata, patched })
}
fn patches(shared: &Path, check: &dyn Fn() -> Result<()>) -> Result<(HashSet<String>, String)> {
    let mut paths = if shared.join("char-index").try_exists()? {
        fs::read_dir(shared.join("char-index"))?.map(|r| r.map(|r| r.path())).collect::<std::io::Result<Vec<_>>>()?
    } else {
        vec![]
    };
    paths.retain(|p| p.extension().is_some_and(|e| e == "json"));
    paths.sort();
    let mut patched = HashSet::new();
    let mut records = vec![];
    for path in paths {
        check()?;
        let value: serde_json::Value = match serde_json::from_slice(&read(&path, check)?) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if value["source"] == "danbooru-patch" {
            patched.insert(
                path.file_stem()
                    .and_then(|s| s.to_str())
                    .ok_or_else(|| Error::invalid("角色索引文件名不是UTF8"))?
                    .to_owned(),
            );
        }
        records.push(serde_json::json!([path.file_name().and_then(|s| s.to_str()), value["source"]]));
    }
    Ok((patched, crate::strict_json::sha256_hex(crate::strict_json::canonical(&serde_json::json!(records)).as_bytes())))
}
fn metadata(source: &Connection, check: &dyn Fn() -> Result<()>) -> Result<(HashMap<String, Metadata>, String)> {
    let mut statement=source.prepare("SELECT char_id,anchor_p90,copyright,wd_tag,base_tag,skin,post_count,gender_conf,anchor_p50,anchor_p75 FROM characters ORDER BY char_id")?;
    let mut values = vec![];
    let mut metadata = HashMap::new();
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        check()?;
        let id: String = row.get(0)?;
        let p90: Option<f64> = row.get(1)?;
        let copyright: Option<String> = row.get(2)?;
        let wd: Option<String> = row.get(3)?;
        let base: Option<String> = row.get(4)?;
        let skin: Option<String> = row.get(5)?;
        let posts: Option<i64> = row.get(6)?;
        let gender: Option<f64> = row.get(7)?;
        let p50: Option<f64> = row.get(8)?;
        let p75: Option<f64> = row.get(9)?;
        if [p90, gender, p50, p75].into_iter().flatten().any(|v| !v.is_finite()) {
            return Err(Error::invalid("角色锚点元数据不是有限值"));
        }
        values.push(serde_json::json!([id, p90, copyright, wd, base, skin, posts, gender, p50, p75]));
        metadata.insert(
            id,
            Metadata {
                p90: p90.unwrap_or(0.) as f32,
                p50: p50.unwrap_or(0.) as f32,
                p75: p75.unwrap_or(0.) as f32,
                skin: skin.is_some_and(|s| !s.is_empty()),
                post_count: posts.unwrap_or(0),
                gender_conf: gender.unwrap_or(0.),
            },
        );
    }
    Ok((metadata, crate::strict_json::sha256_hex(crate::strict_json::canonical(&serde_json::json!(values)).as_bytes())))
}
