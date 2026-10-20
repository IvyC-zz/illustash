//! tests/reference/web/artists.py（#9 已删除） read contract. Auxiliary provenance stays read-only and shares the
//! request transaction.
use crate::{
    http_json::{Json, Refusal},
    http_read::{flag, rows, strings},
    Error, Result,
};
use rusqlite::{Connection, OptionalExtension};
use sha1::{Digest, Sha1};
use std::collections::{BTreeSet, HashMap, HashSet};

/// An artist's pictures in the core: found by machine or confirmed, not removed. Parameter: the artist key.
const CORE_ASSETS: &str = "
    SELECT a.key
    FROM term_projection p
    JOIN terms t ON t.id=p.term_id AND t.key=? AND t.kind='artist'
    JOIN term_assets a ON a.id=p.asset_id AND a.removed_at IS NULL
    WHERE p.state IN ('machine','confirmed')";
/// The artists projected on one library picture (`p`, `a`, `t`). Parameter: the picture's key.
const ON_ASSET: &str = "
    FROM term_projection p
    JOIN term_assets a ON a.id=p.asset_id AND a.namespace='library' AND a.key=?
    JOIN terms t ON t.id=p.term_id AND t.kind='artist'";

#[derive(Default)]
struct Verdicts {
    by_artist: HashMap<String, HashMap<String, String>>,
    order: Vec<String>,
}
fn has_provenance(c: &Connection) -> Result<bool> {
    Ok(c.prepare("PRAGMA database_list")?
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .iter()
        .any(|s| s == "prov"))
}
fn canonical(c: &Connection, id: &str) -> Result<String> {
    let mut id = id.to_owned();
    for _ in 0..10 {
        let next: Option<Option<String>> =
            c.query_row("SELECT merged_into FROM prov.artists WHERE id=?", [&id], |r| r.get(0)).optional()?;
        match next {
            Some(Some(n)) if !n.is_empty() => id = n,
            _ => break,
        }
    }
    Ok(id)
}
fn nickname(c: &Connection, id: &str, core: bool) -> Result<Option<String>> {
    if core {
        let sql = "
            SELECT n.value
            FROM term_names n
            JOIN terms t ON t.id=n.term_id
            WHERE t.key=? AND n.role='display'
            ORDER BY n.preferred DESC,n.position,n.value
            LIMIT 1";
        return Ok(c.query_row(sql, [id], |r| r.get(0)).optional()?);
    }
    let named = "SELECT EXISTS(SELECT 1 FROM prov.sqlite_master WHERE name='artist_names')";
    if !c.query_row(named, [], |r| r.get::<_, bool>(0))? {
        return Ok(None);
    }
    let sql = "SELECT name FROM prov.artist_names WHERE artist_id=? ORDER BY updated_at DESC LIMIT 1";
    Ok(c.query_row(sql, [id], |r| r.get(0)).optional()?)
}
fn card(c: &Connection, id: &str, core: bool) -> Result<Json> {
    let sql = if core {
        "SELECT l.system,l.key,l.url
         FROM term_links l
         JOIN terms t ON t.id=l.term_id
         WHERE t.key=? AND l.system<>'provenance'
         ORDER BY l.system,l.key"
    } else {
        "SELECT system,key,url FROM prov.artist_links WHERE artist_id=? ORDER BY system,key"
    };
    let mut links = HashMap::new();
    for r in rows(c, sql, &[id])? {
        links.entry(r.field("system").text().to_owned()).or_insert_with(|| r.field("key").text().to_owned());
    }
    let nickname = nickname(c, id, core)?;
    let mut homes = Vec::new();
    let mut home = |site: &str, url: String| {
        homes.push(Json::object([("site", site.into()), ("label", site.into()), ("url", url.into())]))
    };
    if let Some(key) = links.get("x") {
        home(crate::origin::get("x").label(), format!("https://x.com/{key}"));
    } else if let Some(key) = links.get("x_id") {
        home(crate::origin::get("x").label(), format!("https://x.com/i/user/{key}"));
    }
    if let Some(key) = links.get("pixiv") {
        home(crate::origin::get("pixiv").label(), format!("https://www.pixiv.net/users/{key}"));
    }
    if let Some(key) = links.get("danbooru") {
        home("Danbooru", format!("https://danbooru.donmai.us/artists/show_or_new?name={key}"));
    }
    let name = nickname
        .or_else(|| links.get("name").cloned())
        .or_else(|| links.get("x").map(|k| format!("@{k}")))
        .or_else(|| links.get("danbooru").map(|k| k.replace('_', " ")))
        .or_else(|| links.get("pixiv").map(|k| format!("Pixiv {k}")))
        .unwrap_or_else(|| "未知画师".into());
    Ok(Json::object([("id", id.into()), ("name", name.into()), ("homes", Json::Array(homes))]))
}
pub fn name_of(c: &Connection, library: bool, id: &str) -> Result<Json> {
    if !library {
        return Ok(None::<String>.into());
    }
    if flag(c, "terms_artists")? {
        return Ok(card(c, id, true)?.field("name").clone());
    }
    if !has_provenance(c)? {
        return Ok(None::<String>.into());
    }
    Ok(card(c, &canonical(c, id)?, false)?.field("name").clone())
}
fn core_assets(c: &Connection, id: &str) -> Result<HashSet<String>> {
    Ok(strings(c, CORE_ASSETS, &[id])?.into_iter().collect())
}
pub(crate) fn assets_of(c: &Connection, library: bool, id: &str) -> Result<Vec<String>> {
    if !library {
        return Ok(Vec::new());
    }
    if flag(c, "terms_artists")? {
        return strings(c, CORE_ASSETS, &[id]);
    }
    if !has_provenance(c)? {
        return Ok(Vec::new());
    }
    let all = verdicts(c, false)?;
    let rejected = all.by_artist.get(id);
    Ok(legacy_assets(c, id, &all)?
        .into_iter()
        .filter(|u| rejected.and_then(|v| v.get(u)).is_none_or(|v| v != "no"))
        .collect())
}
fn legacy_ids(c: &Connection, id: &str) -> Result<Vec<String>> {
    let mut ids = vec![id.to_owned()];
    ids.extend(strings(c, "SELECT id FROM prov.artists WHERE merged_into=?", &[id])?);
    Ok(ids)
}
/// `?,?,…` for `n` parameters of an `IN (…)` list.
fn marks(n: usize) -> String {
    vec!["?"; n].join(",")
}
fn verdicts(c: &Connection, core: bool) -> Result<Verdicts> {
    let sql = if core {
        "SELECT t.key AS artist,a.key AS uuid,v.verdict
         FROM term_effective_verdicts v
         JOIN term_assets a ON a.id=v.asset_id AND a.namespace='library'
         JOIN terms t ON t.id=v.term_id AND t.kind='artist'"
    } else {
        "SELECT artist,uuid,verdict FROM artist_verdicts"
    };
    let mut out = Verdicts::default();
    for v in rows(c, sql, &[])? {
        let artist = v.field("artist").text().to_owned();
        if !out.by_artist.contains_key(&artist) {
            out.order.push(artist.clone());
        }
        out.by_artist
            .entry(artist)
            .or_default()
            .insert(v.field("uuid").text().into(), v.field("verdict").text().into());
    }
    Ok(out)
}
fn legacy_assets(c: &Connection, id: &str, all: &Verdicts) -> Result<HashSet<String>> {
    let ids = legacy_ids(c, id)?;
    let args: Vec<&str> = ids.iter().map(String::as_str).collect();
    let marks = marks(ids.len());
    let sql = |status: &str| {
        format!("SELECT DISTINCT asset FROM prov.image_artists WHERE status='{status}' AND artist_id IN ({marks})")
    };
    let mut out: HashSet<String> = strings(c, &sql("auto"), &args)?.into_iter().collect();
    let judged = all.by_artist.get(id);
    if judged.is_some_and(|v| v.values().any(|s| s == "yes")) {
        for asset in strings(c, &sql("candidate"), &args)? {
            if judged.and_then(|v| v.get(&asset)).is_some_and(|v| v == "yes") {
                out.insert(asset);
            }
        }
    }
    if let Some(v) = judged {
        out.retain(|a| v.get(a).is_none_or(|s| s != "no"));
    }
    Ok(out)
}
fn legacy_methods(c: &Connection, asset: &str, id: &str) -> Result<Vec<String>> {
    let ids = legacy_ids(c, id)?;
    let args: Vec<&str> = std::iter::once(asset).chain(ids.iter().map(String::as_str)).collect();
    let sql = format!(
        "SELECT method
         FROM prov.image_artists
         WHERE asset=? AND status IN ('auto','candidate') AND artist_id IN ({})",
        marks(ids.len())
    );
    strings(c, &sql, &args)
}
fn py_string(v: &Json) -> String {
    match v {
        Json::Scalar(serde_json::Value::String(s)) => s.clone(),
        Json::Scalar(serde_json::Value::Bool(b)) => if *b { "True" } else { "False" }.into(),
        Json::Scalar(serde_json::Value::Null) => "None".into(),
        Json::Array(v) => format!("[{}]", v.iter().map(py_repr).collect::<Vec<_>>().join(", ")),
        Json::Object(v) => format!(
            "{{{}}}",
            v.iter()
                .map(|(k, v)| format!("{}: {}", crate::http_read::python_repr(k), py_repr(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Json::Float(v) if v.is_nan() => "nan".into(),
        Json::Float(v) if *v == f64::INFINITY => "inf".into(),
        Json::Float(v) if *v == f64::NEG_INFINITY => "-inf".into(),
        _ => v.dump(),
    }
}
fn py_repr(v: &Json) -> String {
    if let Json::Scalar(serde_json::Value::String(s)) = v {
        crate::http_read::python_repr(s)
    } else {
        py_string(v)
    }
}
fn reason(method: &str, data: &str) -> Result<Json> {
    let value = match crate::http_json::parse_python_refusal(if data.is_empty() { "{}" } else { data }) {
        Ok(v) => v,
        Err((Refusal::LoneSurrogate | Refusal::TooDeep, e)) => return Err(e),
        Err(_) => Json::Object(Vec::new()),
    };
    if !matches!(value, Json::Object(_)) {
        return Err(Error::Invalid("候选证据不是对象".into()));
    }
    let n = value.get("count").cloned().unwrap_or(1i64.into());
    let n = py_string(&n);
    Ok(match method {
        "group_oc" => format!("同一原创角色里有 {n} 张是 TA 画的"),
        "group_cluster" => format!("相似的未识别图里有 {n} 张是 TA 画的"),
        "group_run" => format!("同一段保存的图里有 {n} 张是 TA 画的"),
        "ocr" => "图上读到了 TA 的账号".into(),
        _ => "有线索指向 TA".into(),
    }
    .into())
}
/// A candidate artist's card with why they are suggested.
fn suggested(mut info: Json, method: &str, data: &str) -> Result<Json> {
    info.set("reason", reason(method, data)?);
    Ok(info)
}
/// An artist's card with how many of `present` (all of `assets` when there is none) are theirs, the verdict on
/// this picture, and the methods that found them on it.
fn credited(
    mut info: Json,
    assets: &HashSet<String>,
    present: Option<&HashSet<String>>,
    (confirmed, rejected): (bool, bool),
    methods: Vec<String>,
) -> Json {
    let count = present.map(|p| assets.intersection(p).count()).unwrap_or(assets.len());
    info.set("count", (count as i64).into());
    info.set("confirmed", confirmed.into());
    info.set("rejected", rejected.into());
    let methods: BTreeSet<String> = methods.into_iter().collect();
    info.set("methods", Json::Array(methods.into_iter().map(Into::into).collect()));
    info
}
fn posts(c: &Connection, asset: &str, core: bool, rejected: &HashSet<String>) -> Result<Json> {
    if !has_provenance(c)? {
        return Ok(Json::Array(Vec::new()));
    }
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    let origins = crate::origin::all().iter().filter(|o| o.tracks_original_posts()).collect::<Vec<_>>();
    let cases = (0..origins.len()).map(|i| format!("WHEN ? THEN {i}")).collect::<Vec<_>>().join(" ");
    let sql = format!(
        "SELECT site,url,artist_id,method
         FROM prov.image_sources
         WHERE asset=?
         ORDER BY CASE site {cases} ELSE {} END, post_id",
        origins.len()
    );
    let mut args = vec![asset];
    args.extend(origins.iter().map(|o| o.id()));
    for r in rows(c, &sql, &args)? {
        let artist = r.field("artist_id").text();
        if r.field("method").text() != "pixel_hash"
            && !artist.is_empty()
            && rejected.contains(&if core { artist.to_owned() } else { canonical(c, artist)? })
        {
            continue;
        }
        let site = r.field("site").text();
        if seen.insert(site.to_owned()) {
            let label = crate::origin::find(site)
                .filter(|o| o.tracks_original_posts())
                .map(|o| o.label())
                .unwrap_or(if site == "danbooru" { "Danbooru" } else { site });
            out.push(Json::object([("site", label.into()), ("url", r.field("url").clone())]));
        }
    }
    Ok(Json::Array(out))
}
fn core_for_asset(c: &Connection, asset: &str, present: Option<&HashSet<String>>, keep_rejected: bool) -> Result<Json> {
    let states = if keep_rejected { "'machine','confirmed','rejected'" } else { "'machine','confirmed'" };
    let sql = format!(
        "SELECT t.key,p.state,p.term_id,p.asset_id
         {ON_ASSET}
         WHERE p.state IN ({states})
         ORDER BY t.key"
    );
    let mut artists = Vec::new();
    for r in rows(c, &sql, &[asset])? {
        let id = r.field("key").text();
        let state = r.field("state").text();
        let info = card(c, id, true)?;
        let mine = core_assets(c, id)?;
        let methods = strings(
            c,
            "SELECT DISTINCT method FROM term_current_evidence WHERE asset_id=? AND term_id=?",
            &[&r.field("asset_id").integer().to_string(), &r.field("term_id").integer().to_string()],
        )?;
        artists.push(credited(info, &mine, present, (state == "confirmed", state == "rejected"), methods));
    }
    let sql = format!("SELECT t.key {ON_ASSET} WHERE p.state='rejected'");
    let rejected: HashSet<String> = strings(c, &sql, &[asset])?.into_iter().collect();
    let sql = format!(
        "SELECT t.key,e.method,e.data,e.score
         {ON_ASSET}
         JOIN term_current_evidence e ON e.asset_id=p.asset_id AND e.term_id=p.term_id
         WHERE p.state='maybe'
         ORDER BY coalesce(e.score,0) DESC,t.key"
    );
    let mut maybe = Vec::new();
    let mut seen = HashSet::new();
    for r in rows(c, &sql, &[asset])? {
        let id = r.field("key").text();
        if seen.insert(id.to_owned()) {
            maybe.push(suggested(card(c, id, true)?, r.field("method").text(), r.field("data").text())?);
        }
    }
    Ok(Json::object([
        ("artists", Json::Array(artists)),
        ("posts", posts(c, asset, true, &rejected)?),
        ("maybe", Json::Array(maybe)),
    ]))
}
fn legacy_for_asset(
    c: &Connection,
    asset: &str,
    present: Option<&HashSet<String>>,
    all: &Verdicts,
    keep_rejected: bool,
) -> Result<Json> {
    let mut mine = HashMap::new();
    for id in &all.order {
        let v = &all.by_artist[id];
        if let Some(v) = v.get(asset) {
            mine.insert(canonical(c, id)?, v.clone());
        }
    }
    let mut artists = Vec::new();
    let mut maybe = Vec::new();
    let mut seen = HashSet::new();
    let mut maybe_seen = HashSet::new();
    let sql = "SELECT artist_id,status,method,evidence
               FROM prov.image_artists
               WHERE asset=? AND status IN ('auto','candidate')
               ORDER BY status='candidate',score DESC";
    for r in rows(c, sql, &[asset])? {
        let id = canonical(c, r.field("artist_id").text())?;
        let verdict = mine.get(&id).map(String::as_str);
        if seen.contains(&id) || (verdict == Some("no") && !keep_rejected) {
            continue;
        }
        if r.field("status").text() == "candidate" && verdict != Some("yes") {
            if verdict != Some("no") && maybe_seen.insert(id.clone()) {
                maybe.push(suggested(card(c, &id, false)?, r.field("method").text(), r.field("evidence").text())?);
            }
            continue;
        }
        seen.insert(id.clone());
        let info = card(c, &id, false)?;
        let assets = legacy_assets(c, &id, all)?;
        let methods = legacy_methods(c, asset, &id)?;
        artists.push(credited(info, &assets, present, (verdict == Some("yes"), verdict == Some("no")), methods));
    }
    let rejected = mine.into_iter().filter_map(|(k, v)| (v == "no").then_some(k)).collect();
    Ok(Json::object([
        ("artists", Json::Array(artists)),
        ("posts", posts(c, asset, false, &rejected)?),
        ("maybe", Json::Array(maybe)),
    ]))
}
fn no_artists() -> Json {
    Json::object([("artists", Json::Array(Vec::new())), ("posts", Json::Array(Vec::new()))])
}
// The same card/evidence/candidate/posts assembly is available to the later detail migration.
pub(crate) fn for_asset(
    c: &Connection,
    library: bool,
    asset: &str,
    present: Option<&HashSet<String>>,
    keep_rejected: bool,
) -> Result<Json> {
    if !library {
        return Ok(no_artists());
    }
    if flag(c, "terms_artists")? {
        return core_for_asset(c, asset, present, keep_rejected);
    }
    if !has_provenance(c)? {
        return Ok(no_artists());
    }
    legacy_for_asset(c, asset, present, &verdicts(c, false)?, keep_rejected)
}
pub fn audit(c: &Connection, library: bool, n: usize) -> Result<Json> {
    let empty = || Json::object([("items", Json::Array(Vec::new())), ("pool", 0i64.into())]);
    if !library {
        return Ok(empty());
    }
    let core = flag(c, "terms_artists")?;
    if !core && !has_provenance(c)? {
        return Ok(empty());
    }
    let present: HashSet<String> = strings(c, "SELECT uuid FROM assets", &[])?.into_iter().collect();
    let sql = if core {
        "SELECT DISTINCT a.key
         FROM term_current_evidence e
         JOIN term_assets a ON a.id=e.asset_id AND a.removed_at IS NULL
         JOIN terms t ON t.id=e.term_id AND t.kind='artist'
         WHERE e.method IN ('pixel_hash','gallery_match','iqdb','same_picture')"
    } else {
        "SELECT DISTINCT asset FROM prov.image_artists WHERE status='auto'"
    };
    let mut pool: Vec<String> = strings(c, sql, &[])?.into_iter().filter(|a| present.contains(a)).collect();
    pool.sort_by_cached_key(|a| Sha1::digest(format!("artist-audit-v1:{a}").as_bytes()).to_vec());
    let count = pool.len();
    let mut items = Vec::new();
    let all = if core { Verdicts::default() } else { verdicts(c, false)? };
    for a in pool.into_iter().take(n) {
        let mut item = Json::object([("uuid", a.as_str().into())]);
        item.extend(if core {
            for_asset(c, library, &a, Some(&present), true)?
        } else {
            legacy_for_asset(c, &a, Some(&present), &all, true)?
        });
        items.push(item);
    }
    Ok(Json::object([("items", Json::Array(items)), ("pool", (count as i64).into())]))
}
