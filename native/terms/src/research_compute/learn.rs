//! 现有的原型和弱认领外圈。人工决定和归属由 Core 发布。
use super::{
    discriminate,
    features::{self, Matrix},
    numeric,
    snapshot::{self, Snapshot},
};
use crate::{Error, Result};
use rusqlite::{Connection, OptionalExtension};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs,
    path::Path,
};

const MIN_SEEDS: usize = 5;
const FLOOR: f64 = 0.03;
#[derive(Debug)]
pub struct Prototype {
    pub vector: Vec<f32>,
    pub threshold: f64,
    pub seeds: usize,
    pub hint: f64,
}
pub fn prototypes(seeds: &[Vec<f32>], negatives: &[Vec<f32>]) -> Result<Vec<Prototype>> {
    let seeds: Vec<_> = numeric::sample600(seeds.len()).into_iter().map(|i| seeds[i].clone()).collect();
    let mut out = vec![];
    for members in
        numeric::groups(&seeds, 0.06, 3.max((0.05 * seeds.len() as f64) as usize), Some(8)).map_err(Error::Invalid)?
    {
        let centre = numeric::centre(&seeds, &members).map_err(Error::Invalid)?;
        let selected: Vec<_> = members.iter().map(|&i| seeds[i].clone()).collect();
        let mut spread: Vec<_> = numeric::distances(&selected, std::slice::from_ref(&centre))
            .map_err(Error::Invalid)?
            .into_iter()
            .map(|r| r[0])
            .collect();
        let mut threshold = numeric::percentile90(&mut spread).clamp(FLOOR, 0.08);
        let mut hint = (1.5 * threshold).min(0.11);
        if !negatives.is_empty() {
            let nearest = 0.9
                * numeric::distances(negatives, std::slice::from_ref(&centre))
                    .map_err(Error::Invalid)?
                    .iter()
                    .map(|r| r[0] as f64)
                    .fold(f64::INFINITY, f64::min);
            threshold = threshold.min(nearest);
            hint = hint.min(nearest);
        }
        if threshold >= FLOOR {
            out.push(Prototype { vector: centre, threshold, seeds: members.len(), hint: hint.max(threshold) });
        }
    }
    Ok(out)
}

// Python json.dumps(..., ensure_ascii=True) 的分隔符是持久化学习签名的一部分。
fn python_json(value: &Value) -> String {
    match value {
        Value::Array(values) => format!("[{}]", values.iter().map(python_json).collect::<Vec<_>>().join(", ")),
        Value::String(text) => {
            let quoted = serde_json::to_string(text).expect("JSON string");
            let mut out = String::new();
            for c in quoted.chars() {
                if c.is_ascii() && c != '\u{7f}' {
                    out.push(c);
                } else {
                    for unit in c.encode_utf16(&mut [0; 2]).iter() {
                        out.push_str(&format!("\\u{unit:04x}"));
                    }
                }
            }
            out
        }
        _ => value.to_string(),
    }
}
fn artwork_groups(c: &Connection) -> Result<HashMap<String, String>> {
    let mut parent: HashMap<_, _> =
        snapshot::strings(c, "SELECT uuid FROM assets", &[])?.into_iter().map(|u| (u.clone(), u)).collect();
    fn find(parent: &HashMap<String, String>, u: &str) -> String {
        let mut at = u;
        while parent[at] != at {
            at = &parent[at];
        }
        at.to_owned()
    }
    let mut union = |a: String, b: String| {
        if parent.contains_key(&a) && parent.contains_key(&b) {
            let a = find(&parent, &a);
            let b = find(&parent, &b);
            let (lo, hi) = if a < b { (a, b) } else { (b, a) };
            parent.insert(hi, lo);
        }
    };
    let mut first = HashMap::<String, String>::new();
    for r in c
        .prepare("SELECT uuid,grp FROM duplicates")?
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
    {
        let (u, g) = r?;
        union(u.clone(), first.entry(g).or_insert(u).clone());
    }
    for r in c
        .prepare("SELECT a,b,kind FROM relations")?
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?
    {
        let (a, b, kind) = r?;
        if ["identical", "duplicate", "subtle", "crop", "variant", "edit"].contains(&kind.as_str()) {
            union(a, b);
        }
    }
    Ok(parent.keys().map(|u| (u.clone(), find(&parent, u))).collect())
}
pub fn signatures(c: &Connection, f: &Matrix) -> Result<BTreeMap<String, String>> {
    let policy: Option<String> =
        c.query_row("SELECT value FROM meta WHERE key='identity_policy'", [], |r| r.get(0)).optional()?;
    let hybrid = policy.as_deref() == Some("hybrid");
    let mut hashes = BTreeMap::<String, Sha256>::new();
    if hybrid {
        let mut members = HashMap::<String, Vec<Value>>::new();
        for r in c
            .prepare("SELECT id,character_id,canonical_id FROM identity_nodes ORDER BY id")?
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?
        {
            let (id, family, canonical) = r?;
            members.entry(family).or_default().push(json!([id, canonical]));
        }
        let groups = artwork_groups(c)?;
        let sql="SELECT s.character_id,s.raw_entity,s.uuid,s.status,s.origin,EXISTS(SELECT 1 FROM single_assets a WHERE a.uuid=s.uuid) FROM effective_assertions s ORDER BY 1,2,3";
        let mut q = c.prepare(sql)?;
        let mut rows = q.query([])?;
        while let Some(r) = rows.next()? {
            let family: String = r.get(0)?;
            let uuid: String = r.get(2)?;
            let digest = hashes.entry(family.clone()).or_insert_with(|| {
                Sha256::new_with_prefix(python_json(&json!(members.get(&family).cloned().unwrap_or_default())))
            });
            digest.update(python_json(&json!([
                r.get::<_, String>(1)?,
                uuid,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, i64>(5)?,
                groups.get(&uuid).ok_or_else(|| Error::Invalid("missing research asset".into()))?
            ])));
            digest.update(f.versions.get(&uuid).map(Vec::as_slice).unwrap_or(b"missing"));
        }
    } else {
        let sql = format!(
            "SELECT coalesce(x.parent_id,x.id),s.entity,s.uuid,s.status,a.uuid IS NOT NULL
             FROM assertions s
             JOIN {ENTITIES} x ON x.id=s.entity
             LEFT JOIN assets a ON a.uuid=s.uuid
             ORDER BY 1,2,3",
            ENTITIES = crate::character_entities::ENTITIES
        );
        let mut q = c.prepare(&sql)?;
        let mut rows = q.query([])?;
        while let Some(r) = rows.next()? {
            let family: String = r.get(0)?;
            let uuid: String = r.get(2)?;
            let digest = hashes.entry(family).or_default();
            digest.update(format!(
                "{}\n",
                python_json(&json!([r.get::<_, String>(1)?, uuid, r.get::<_, String>(3)?, r.get::<_, i64>(4)?]))
            ));
            digest.update(f.versions.get(&uuid).map(Vec::as_slice).unwrap_or(b"missing"));
        }
    }
    Ok(hashes
        .into_iter()
        .map(|(e, h)| (e, format!("{}{:x}", if hybrid { "identity-v3:" } else { "v4:" }, h.finalize())))
        .collect())
}

#[derive(Default)]
pub struct Anchors {
    pub owners: Vec<String>,
    pub vectors: Vec<Vec<f32>>,
    pub lo: Vec<f32>,
    pub hi: Vec<f32>,
}
fn hex_decode(s: &str) -> Result<Vec<u8>> {
    if !s.is_ascii() || s.len() % 2 != 0 {
        return Err(Error::Invalid("Invalid prototype bytes".into()));
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|_| Error::Invalid("Invalid prototype bytes".into())))
        .collect()
}
/// 原型认领加上判别式的 `strong` 认领（分数高的胜出），以及弱认领外圈。
fn claims(
    f: &Matrix,
    uuids: &[String],
    protos: &[Value],
    anchors: &Anchors,
    strong: &BTreeMap<(String, String), f64>,
) -> Result<(Vec<Value>, Vec<Value>)> {
    let (ids, rows) = f.matrix(uuids);
    let mut claims = BTreeMap::<(String, String), f64>::new();
    let mut weak = BTreeMap::<(String, String, String), f64>::new();
    let parsed = protos
        .iter()
        .map(|r| {
            Ok((
                r[0].as_str().ok_or_else(|| Error::Invalid("Invalid prototype owner".into()))?,
                features::decode(&hex_decode(
                    r[2].as_str().ok_or_else(|| Error::Invalid("Invalid prototype".into()))?,
                )?)?,
                r[3].as_f64().ok_or_else(|| Error::Invalid("Invalid prototype threshold".into()))? as f32,
                r[5].as_f64().ok_or_else(|| Error::Invalid("Invalid prototype hint".into()))? as f32,
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    let proto_vectors: Vec<_> = parsed.iter().map(|(_, v, _, _)| v.clone()).collect();
    for start in (0..rows.len()).step_by(8192) {
        let end = (start + 8192).min(rows.len());
        let distances = numeric::distances(&rows[start..end], &proto_vectors).map_err(Error::Invalid)?;
        let anchor_distances = numeric::distances(&rows[start..end], &anchors.vectors).map_err(Error::Invalid)?;
        for (offset, u) in ids[start..end].iter().enumerate() {
            for (j, (owner, _, threshold, hint)) in parsed.iter().enumerate() {
                let distance = distances[offset][j];
                let score = 1.0 - distance as f64;
                if distance > -1.0 && distance <= *threshold {
                    claims.entry((u.clone(), (*owner).into())).and_modify(|s| *s = s.max(score)).or_insert(score);
                }
                if distance > *threshold && distance <= *hint {
                    weak.entry((u.clone(), (*owner).into(), "PROTO".into()))
                        .and_modify(|s| *s = s.max(score))
                        .or_insert(score);
                }
            }
            for (i, _) in anchors.owners.iter().enumerate() {
                let distance = anchor_distances[offset][i];
                let score = 1.0 - distance as f64;
                if distance > anchors.lo[i] && distance <= anchors.hi[i] {
                    weak.entry((u.clone(), anchors.owners[i].clone(), "CCIP".into()))
                        .and_modify(|s| *s = s.max(score))
                        .or_insert(score);
                }
            }
        }
    }
    for (key, &score) in strong {
        claims.entry(key.clone()).and_modify(|s| *s = s.max(score)).or_insert(score);
    }
    weak.retain(|(u, e, source), _| source != "PROTO" || !claims.contains_key(&(u.clone(), e.clone())));
    Ok((
        claims.into_iter().map(|((u, e), score)| json!([u, e, score])).collect(),
        weak.into_iter().map(|((u, e, s), score)| json!([u, e, s, score])).collect(),
    ))
}

pub fn learn(
    snapshot: &Snapshot,
    f: &Matrix,
    token: &Value,
    reclaim: bool,
    anchors: &Anchors,
) -> Result<(Value, Option<Value>)> {
    let c = snapshot.read()?;
    c.execute_batch("BEGIN")?;
    let marks: BTreeSet<_> = snapshot::strings(
        &c,
        &format!(
            "SELECT id FROM {ENTITIES} WHERE label_only=1 OR mascot=1",
            ENTITIES = crate::character_entities::ENTITIES
        ),
        &[],
    )?
    .into_iter()
    .collect();
    let mut current = signatures(&c, f)?;
    current.retain(|e, _| !marks.contains(e));
    let old_states = snapshot::table(&c, "learn_state")?;
    let known: BTreeMap<_, _> = old_states
        .iter()
        .map(|r| (r[0].as_str().unwrap_or("").to_owned(), r[1].as_str().unwrap_or("").to_owned()))
        .collect();
    let mut dirty: Vec<_> =
        current.iter().filter(|(e, sig)| known.get(*e) != Some(*sig)).map(|(e, _)| e.clone()).collect();
    dirty.extend(known.keys().filter(|e| !current.contains_key(*e)).cloned());
    if dirty.is_empty() && !reclaim {
        return Ok((json!({"relearned":0,"changed":0}), None));
    }
    let dirty_set: BTreeSet<_> = dirty.iter().cloned().collect();
    let mut protos: Vec<_> = snapshot::table(&c, "prototypes")?
        .into_iter()
        .filter(|r| !dirty_set.contains(r[0].as_str().unwrap_or("")))
        .collect();
    for entity in &dirty {
        if marks.contains(entity) {
            continue;
        }
        let confirmed = snapshot::strings(
            &c,
            "SELECT DISTINCT uuid FROM family_confirmed WHERE entity=? ORDER BY uuid",
            &[entity],
        )?;
        let removed=snapshot::strings(&c,"SELECT s.uuid FROM assertions s JOIN assets a ON a.uuid=s.uuid WHERE s.entity=? AND s.status='rejected' ORDER BY s.uuid",&[entity])?;
        let (_, seeds) = f.matrix(&confirmed);
        let (_, negatives) = f.matrix(&removed);
        if seeds.len() < MIN_SEEDS {
            continue;
        }
        for (k, p) in prototypes(&seeds, &negatives)?.into_iter().enumerate() {
            let hex = features::bytes(&p.vector).iter().map(|b| format!("{b:02x}")).collect::<String>();
            protos.push(json!([entity, k, hex, p.threshold, p.seeds, p.hint]));
        }
    }
    let stamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let mut states: Vec<_> =
        old_states.into_iter().filter(|r| !dirty_set.contains(r[0].as_str().unwrap_or(""))).collect();
    for entity in &dirty {
        if let Some(sig) = current.get(entity) {
            states.push(json!([entity, sig, stamp]));
        }
    }
    let uuids = snapshot::strings(&c, "SELECT uuid FROM assets", &[])?;
    // 仅供测试：`ILLUSTASH_TEST_PROTOTYPES_ONLY` 去掉判别式认领（#156），让按只有原型的算法录制的固定答案
    // （tests/test_native_research_compute.py）仍然适用。产品从不设置它。
    let strong = if std::env::var_os("ILLUSTASH_TEST_PROTOTYPES_ONLY").is_some() {
        BTreeMap::new()
    } else {
        discriminate::claims(&c, f, &marks, &artwork_groups(&c)?, &uuids)?
    };
    let (claims, weak) = claims(f, &uuids, &protos, anchors, &strong)?;
    let batch = json!({"v":1,"token":token,"relearned":dirty.len(),"prototypes":protos,"learn_state":states,"proto_claims":claims,"weak_claims":weak});
    drop(c);
    let result = snapshot.publish_learning(&batch)?;
    Ok((result, Some(batch)))
}

/// 补丁锚点的中间档，包括不变的直接近邻 / 上升方向别名规则。
pub fn patch_anchors(snapshot: &Snapshot, root: &Path) -> Result<Anchors> {
    let path = root.join("char-anchors.npy");
    if !path.exists() {
        return Ok(Anchors::default());
    }
    let raw = features::read_npy(&path)?;
    let ids: Vec<_> = fs::read_to_string(root.join("char-anchors.txt"))?
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect();
    if raw.len() != ids.len() {
        return Err(Error::Invalid("anchor vectors and ids differ in length".into()));
    }
    let (aliases, selected) = raw.into_anchor_matrices(ids.clone())?;
    let vectors = aliases.rows;
    let source = crate::sqlite::open_with_flags(&snapshot.source, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    struct Meta {
        p90: f64,
        p75: f64,
        skin: bool,
        pop: i64,
        gender: f64,
    }
    let meta: HashMap<_, _> = source
        .prepare("SELECT char_id,anchor_p90,anchor_p75,skin,post_count,gender_conf FROM characters")?
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                Meta {
                    p90: r.get::<_, Option<f64>>(1)?.unwrap_or(0.0),
                    p75: r.get::<_, Option<f64>>(2)?.unwrap_or(0.0),
                    skin: r.get::<_, Option<String>>(3)?.is_some_and(|s| !s.is_empty()),
                    pop: r.get::<_, Option<i64>>(4)?.unwrap_or(0),
                    gender: r.get::<_, Option<f64>>(5)?.unwrap_or(0.0),
                },
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let rank = |id: &String| {
        let m = meta.get(id);
        (m.is_some_and(|m| m.gender >= 0.9), !m.is_some_and(|m| m.skin), m.map(|m| m.pop).unwrap_or(0), id.clone())
    };
    let mut alias: Vec<_> = (0..ids.len()).collect();
    for start in (0..ids.len()).step_by(1024) {
        let right = vectors.clone();
        let distances =
            numeric::distances(&vectors[start..(start + 1024).min(ids.len())], &right).map_err(Error::Invalid)?;
        for (offset, row) in distances.iter().enumerate() {
            let i = start + offset;
            for j in 0..ids.len() {
                if row[j] <= 0.01 && rank(&ids[j]) > rank(&ids[alias[i]]) {
                    alias[i] = j;
                }
            }
        }
    }
    for i in 0..ids.len() {
        let mut j = alias[i];
        while alias[j] != j {
            j = alias[j];
        }
        alias[i] = j;
    }
    let mut patched = BTreeSet::new();
    match fs::read_dir(root.join("char-index")) {
        Ok(paths) => {
            for p in paths {
                let p = p?.path();
                if p.extension().is_some_and(|s| s == "json") {
                    // Python 跳过解码和 JSON 的 ValueError，但 I/O 失败会往外抛。
                    let contents = fs::read(&p)?;
                    let Ok(contents) = std::str::from_utf8(&contents) else { continue };
                    if serde_json::from_str::<Value>(contents).is_ok_and(|v| v["source"] == "danbooru-patch") {
                        if let Some(s) = p.file_stem().and_then(|s| s.to_str()) {
                            patched.insert(s.to_owned());
                        }
                    }
                }
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    let c = snapshot.read()?;
    let links: HashMap<String, String> = c
        .prepare(&format!(
            "SELECT l.key, l.entity_id FROM {LINKS} l WHERE l.core_system = 'ccip'",
            LINKS = crate::character_links::LINKS
        ))?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut out = Anchors::default();
    for i in 0..ids.len() {
        let m = meta.get(&ids[i]);
        if !patched.contains(&ids[i]) || m.is_none_or(|m| m.p90 == 0.0) {
            continue;
        }
        if let Some(entity) = links.get(&ids[alias[i]]) {
            out.owners.push(Snapshot::resolve(&c, entity)?);
            out.vectors.push(selected.rows[i].clone());
            out.lo.push(m.expect("known metadata").p75 as f32);
            out.hi.push(m.expect("known metadata").p90 as f32);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn approved_float64_percentile_preserves_fixed_weak_boundary() {
        let mut spread = [0x3d6fd6f3, 0x3d4e22d6, 0x3d66a5d9, 0x3d015b47, 0x3d715049].map(f32::from_bits);
        let threshold = numeric::percentile90(&mut spread);
        assert_eq!(threshold, 0.05877051427960396);
        let hint = (threshold * 1.5).min(0.11);
        assert_eq!((hint as f32).to_bits(), 0x3db48b03);
        let cosine = f32::from_bits(0x3f52dd3f);
        let f = Matrix::new(vec!["SYNTHETIC".into()], vec![vec![cosine, (1.0 - cosine * cosine).sqrt()]], 2).unwrap();
        let encoded: String = features::bytes(&[1.0, 0.0]).iter().map(|b| format!("{b:02x}")).collect();
        let old = json!(["E", 0, encoded, threshold, 5, hint]);
        let (strong, weak) = claims(&f, &f.uuids, &[old], &Anchors::default(), &BTreeMap::new()).unwrap();
        assert!(strong.is_empty() && weak.is_empty());
        // 旧版本漂移的证据保持显式；不加容差，也不重写 golden。
        let promoted = json!(["E", 0, encoded, threshold, 5, 0.08815577626228333]);
        let (strong, weak) = claims(&f, &f.uuids, &[promoted], &Anchors::default(), &BTreeMap::new()).unwrap();
        assert!(strong.is_empty());
        assert_eq!(weak, vec![json!(["SYNTHETIC", "E", "PROTO", 0.9118442237377167])]);
    }
    #[test]
    fn negative_margin_and_floor() {
        let seeds = vec![vec![1.0, 0.0]; 6];
        let p = prototypes(&seeds, &[]).unwrap();
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].threshold, 0.03);
        assert_eq!(p[0].hint, 0.045);
        assert!(prototypes(&seeds, &[vec![1.0, 0.0]]).unwrap().is_empty());
    }
    #[test]
    fn signature_json_preserves_python_spacing_and_unicode() {
        assert_eq!(python_json(&json!(["A", "中文😀", 1])), r#"["A", "\u4e2d\u6587\ud83d\ude00", 1]"#);
    }
}
