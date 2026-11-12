use super::*;
use crate::recognition_compute::anchors::Metadata;
use std::collections::{BTreeMap, HashMap};
#[derive(serde::Deserialize, Default)]
struct OrderedMeta {
    gender: Option<OrderedGender>,
}
struct OrderedGender(Vec<(String, f64)>);
impl<'de> serde::Deserialize<'de> for OrderedGender {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = OrderedGender;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("gender object")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(self, mut a: A) -> std::result::Result<Self::Value, A::Error> {
                let mut values = vec![];
                while let Some(pair) = a.next_entry::<String, f64>()? {
                    values.push(pair);
                }
                Ok(OrderedGender(values))
            }
        }
        d.deserialize_map(Visitor)
    }
}
pub(super) fn ordered_gender(path: &Path) -> Result<Vec<(String, f64)>> {
    let ordered: OrderedMeta = serde_json::from_slice(&fs::read(path)?).map_err(|e| Error::invalid(e.to_string()))?;
    Ok(ordered.gender.map(|g| g.0).unwrap_or_default())
}
pub type Built =
    (Vec<Value>, Vec<(String, String, String)>, Vec<Vec<f32>>, Vec<String>, HashMap<String, Metadata>, HashSet<String>);
pub fn split_tag(tag: &str) -> (String, Option<String>, Option<String>) {
    let re = regex::Regex::new(r"\(([^()]+)\)").expect("fixed tag regex");
    let parts: Vec<_> = re.captures_iter(tag).map(|c| c[1].to_owned()).collect();
    let re = regex::Regex::new(r"_?\([^()]+\)").expect("fixed tag regex");
    let base = re.replace_all(tag, "").trim_matches('_').to_owned();
    let skin = (parts.len() > 1).then(|| parts[..parts.len() - 1].join("_"));
    (base, skin, parts.last().cloned())
}
fn game(copyright: &str) -> &str {
    match copyright {
        "blue_archive" => "bluearchive",
        "arknights" => "arknights",
        "azur_lane" => "azurlane",
        "fate_grand_order" | "fate/grand_order" => "fgo",
        "genshin_impact" => "genshin",
        "girls_frontline" => "girlsfrontline",
        "goddess_of_victory:_nikke" | "nikke" => "nikke",
        "path_to_nowhere" => "pathtonowhere",
        "honkai:_star_rail" => "starrail",
        "neural_cloud" => "neuralcloud",
        _ => "",
    }
}
fn games(review: &Path) -> Result<HashMap<String, HashMap<String, Value>>> {
    let mut all = HashMap::new();
    for path in files(&review.join("game-chars"), "json")? {
        let Some(name) = path.file_name().and_then(|s| s.to_str()).and_then(|s| s.strip_suffix("_index.json")) else {
            continue;
        };
        let Ok(data) = read(&path) else { continue };
        let rows = data["data"].as_array().or_else(|| data.as_array());
        let mut map = HashMap::new();
        for row in rows.into_iter().flatten() {
            for n in row["ennames"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                map.entry(n.trim().to_lowercase()).or_insert_with(|| row.clone());
                for part in n.split_whitespace() {
                    map.entry(part.trim().to_lowercase()).or_insert_with(|| row.clone());
                }
            }
        }
        all.insert(name.into(), map);
    }
    Ok(all)
}
pub(super) fn build(review: &Path, tags: &Path) -> Result<Built> {
    let mut source = BTreeMap::new();
    let mut gender_order = HashMap::new();
    for path in files(&review.join("char-index"), "json")? {
        let Ok(meta) = read(&path) else { continue };
        let cid = meta["short_tag"]
            .as_str()
            .filter(|s| !s.is_empty())
            .unwrap_or(path.file_stem().and_then(|s| s.to_str()).ok_or_else(|| Error::invalid("索引文件名不是UTF8"))?)
            .to_owned();
        if !safe(&cid) {
            return Err(Error::invalid("索引角色ID不正确"));
        }
        gender_order.insert(cid.clone(), ordered_gender(&path)?);
        source.insert(cid, meta);
    }
    let mut wd = HashSet::new();
    let mut csv = csv::Reader::from_path(tags).map_err(|e| Error::invalid(e.to_string()))?;
    let header = csv.headers().map_err(|e| Error::invalid(e.to_string()))?.clone();
    let category = header.iter().position(|s| s == "category").ok_or_else(|| Error::invalid("标签category缺失"))?;
    let name = header.iter().position(|s| s == "name").ok_or_else(|| Error::invalid("标签name缺失"))?;
    for r in csv.records() {
        let r = r.map_err(|e| Error::invalid(e.to_string()))?;
        if r.get(category) == Some("4") {
            wd.insert(r.get(name).ok_or_else(|| Error::invalid("标签name缺失"))?.to_owned());
        }
    }
    let games = games(review)?;
    let amap = read(&review.join("album-char-map.json"))?;
    let tag_for = |cid: &str| source[cid]["tag"].as_str().filter(|s| !s.is_empty()).unwrap_or(cid).to_owned();
    let mut plain = HashMap::new();
    for cid in source.keys() {
        let tag = tag_for(cid);
        let (base, _, cr) = split_tag(&tag);
        if tag == base {
            plain.insert(tag, cr.or_else(|| source[cid]["copyright"].as_str().map(String::from)));
        }
    }
    let (mut rows, mut aliases, mut vectors, mut ids, mut metadata, mut patched) =
        (vec![], vec![], vec![], vec![], HashMap::new(), HashSet::new());
    for (cid, d) in &source {
        let tag = tag_for(cid);
        let (base, mut skin, mut cr) = split_tag(&tag);
        if skin.is_none() && cr.is_some() && tag != base {
            if let Some(c) = plain.get(&base) {
                skin = cr;
                cr = c.clone();
            }
        }
        let cr = cr.or_else(|| d["copyright"].as_str().map(String::from));
        let rec = games.get(game(cr.as_deref().unwrap_or(""))).and_then(|g| g.get(&base.to_lowercase()));
        let (mut nref, mut p50, mut p75, mut p90, mut arow) =
            (Value::Null, Value::Null, Value::Null, Value::Null, Value::Null);
        let fp = review.join(format!("char-index/{cid}.npy"));
        if fp.try_exists()? {
            if let Some(a) = optional_reference(&fp)? {
                if a.len() >= 3 {
                    let (anchor, p) = a.built()?;
                    nref = json!(a.len());
                    p50 = json!(p[0]);
                    p75 = json!(p[1]);
                    p90 = json!(p[2]);
                    arow = json!(vectors.len());
                    vectors.push(anchor);
                    ids.push(cid.clone());
                }
            }
        }
        let gender = gender_order[cid]
            .iter()
            .filter(|(_, n)| *n > 0.)
            .enumerate()
            .max_by(|(i, (_, x)), (j, (_, y))| x.total_cmp(y).then(j.cmp(i)))
            .map(|(_, p)| p);
        let (mut row, gender_name, gender_conf) = (
            json!({}),
            gender.map(|(k, _)| json!(k)).unwrap_or(Value::Null),
            gender.map(|(_, n)| json!(n)).unwrap_or(Value::Null),
        );
        for (k, v) in [
            ("char_id", json!(cid)),
            ("danbooru_tag", json!(tag)),
            ("wd_tag", if wd.contains(&tag) { json!(tag) } else { Value::Null }),
            ("copyright", json!(cr)),
            ("base_tag", json!(base)),
            ("skin", json!(skin)),
            ("post_count", d["post_count"].clone()),
            ("gender", gender_name),
            ("gender_conf", gender_conf),
            (
                "core_tags",
                json!(serde_json::to_string(&d["core_tags"].as_array().cloned().unwrap_or_default())
                    .map_err(|e| Error::invalid(e.to_string()))?),
            ),
            ("n_ref", nref),
            ("anchor_p50", p50),
            ("anchor_p75", p75),
            ("anchor_p90", p90),
            ("anchor_row", arow),
        ] {
            row[k] = v;
        }
        for (key, names) in [("name_en", "ennames"), ("name_ja", "jpnames"), ("name_zh", "cnnames")] {
            row[key] = rec.and_then(|r| r[names].as_array()).and_then(|a| a.first()).cloned().unwrap_or(Value::Null);
        }
        aliases.push((cid.clone(), cid.clone(), "short".into()));
        aliases.push((cid.clone(), tag.clone(), "danbooru".into()));
        if wd.contains(&tag) {
            aliases.push((cid.clone(), tag, "wd".into()));
        }
        if !base.is_empty() {
            aliases.push((cid.clone(), base, "base".into()));
        }
        if let Some(rec) = rec {
            for (names, kind) in [("ennames", "en"), ("jpnames", "ja"), ("cnnames", "zh")] {
                for n in rec[names].as_array().into_iter().flatten() {
                    aliases.push((
                        cid.clone(),
                        n.as_str().ok_or_else(|| Error::invalid("官方名字不是文本"))?.into(),
                        kind.into(),
                    ));
                }
            }
        }
        metadata.insert(
            cid.clone(),
            Metadata {
                p90: row["anchor_p90"].as_f64().unwrap_or(0.) as f32,
                p50: row["anchor_p50"].as_f64().unwrap_or(0.) as f32,
                p75: row["anchor_p75"].as_f64().unwrap_or(0.) as f32,
                skin: row["skin"].as_str().is_some_and(|s| !s.is_empty()),
                post_count: row["post_count"].as_i64().unwrap_or(0),
                gender_conf: row["gender_conf"].as_f64().unwrap_or(0.),
            },
        );
        if d["source"] == "danbooru-patch" {
            patched.insert(cid.clone());
        }
        rows.push(row);
    }
    for (album, info) in amap["map"].as_object().into_iter().flatten() {
        let mut cids = vec![info["char_id"].clone()];
        cids.extend(info["also"].as_array().into_iter().flatten().cloned());
        for cid in cids {
            aliases.push((
                cid.as_str().ok_or_else(|| Error::invalid("相册角色ID不是文本"))?.into(),
                album.clone(),
                "album".into(),
            ));
        }
    }
    let mut seen = HashSet::new();
    aliases.retain(|a| seen.insert(a.clone()));
    // 空的重建同样替换掉过期锚点，因此删掉最后一个补丁不会留下旧认领。
    save(&review.join("char-anchors.npy"), &vectors)?;
    fs::write(review.join("char-anchors.txt"), ids.join("\n"))?;
    Ok((rows, aliases, vectors, ids, metadata, patched))
}
