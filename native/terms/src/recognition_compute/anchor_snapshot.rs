//! One authorised task snapshot. No feature cache or quadratic work in write/receive locks.
use super::{
    anchors::{self, AnchorIndex, Claim, Metadata},
    models::hash,
};
use crate::{Error, Result};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::Read,
    path::Path,
};
fn invalid(message: impl Into<String>) -> Error {
    Error::Invalid(message.into())
}
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
fn anchored(shared: &Path) -> Result<bool> {
    let a = shared.join("char-anchors.npy").try_exists()?;
    let b = shared.join("char-anchors.txt").try_exists()?;
    if a != b {
        return Err(invalid("角色锚点必须同时有向量和ID文件"));
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
    pub(crate) fn load_checked(shared: &Path, source: &Path, check: &dyn Fn() -> Result<()>) -> Result<Self> {
        check()?;
        if !anchored(shared)? {
            return Ok(Self {
                fingerprint: absent(),
                vectors: vec![],
                index: AnchorIndex { entities: vec![], p90: vec![], p50: vec![], p75: vec![] },
                limits: vec![],
            });
        }
        let id_bytes = read(&shared.join("char-anchors.txt"), check)?;
        let ids = std::str::from_utf8(&id_bytes)
            .map_err(|e| invalid(e.to_string()))?
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
        let source = crate::sqlite::open_with_flags(source, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let (metadata, metadata_hash) = metadata(&source, check)?;
        let (patched, patch_hash) = patches(shared, check)?;
        let fingerprint = Fingerprint {
            vectors: Some(digest),
            ids: Some(crate::strict_json::sha256_hex(&id_bytes)),
            metadata: Some(metadata_hash),
            patches: Some(patch_hash),
        };
        let index = anchors::anchor_index_checked(&matrix, &ids, &metadata, check)?;
        let limits = anchors::claim_thresholds(&ids, &index, &patched)?;
        Ok(Self { fingerprint, vectors: matrix, index, limits })
    }
    /// Linear authoritative recheck under the source lease: no matrix/index construction.
    pub(crate) fn fingerprint(
        shared: &Path,
        source: &Connection,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<Fingerprint> {
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
                path.file_stem().and_then(|s| s.to_str()).ok_or_else(|| invalid("角色索引文件名不是UTF8"))?.to_owned(),
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
            return Err(invalid("角色锚点元数据不是有限值"));
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
