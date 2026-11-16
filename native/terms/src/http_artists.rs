//! tests/reference/web/artists.py（#9 已删除）的读取契约。辅助的溯源库保持只读，并与请求共用同一个事务。
use crate::{
    http_json::{Json, Refusal},
    http_read::{flag, python_int, rows, strings, trim},
    sql_fragments::{rating, visible},
    Error, Result,
};
use rusqlite::{Connection, OptionalExtension};
use sha1::{Digest, Sha1};
use std::collections::{BTreeSet, HashMap, HashSet};

/// 核心里某位画师的图片：机器找到或已确认、且未移除的。参数：画师的 key。
const CORE_ASSETS: &str = "
    SELECT a.key
    FROM term_projection p
    JOIN terms t ON t.id=p.term_id AND t.key=? AND t.kind='artist'
    JOIN term_assets a ON a.id=p.asset_id AND a.removed_at IS NULL
    WHERE p.state IN ('machine','confirmed')";
/// 投影到某张图库图片上的画师（`p`、`a`、`t`）。参数：图片的 key。
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
// 以 t.id 为参数；详情和目录都用首选的显示名。
const CORE_NICKNAME: &str = "(SELECT n.value FROM term_names n
    WHERE n.term_id=t.id AND n.role='display'
    ORDER BY n.preferred DESC,n.position,n.value LIMIT 1)";

fn nickname(c: &Connection, id: &str, core: bool) -> Result<Option<String>> {
    if core {
        let sql = format!("SELECT {CORE_NICKNAME} FROM terms t WHERE t.key=?");
        return Ok(c.query_row(&sql, [id], |r| r.get(0)).optional()?.flatten());
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
    let link = |system| links.get(system).map(String::as_str);
    let name = display_name(nickname, link("name"), link("x"), link("danbooru"), link("pixiv"));
    Ok(Json::object([("id", id.into()), ("name", name.into()), ("homes", Json::Array(homes))]))
}

fn display_name(
    nickname: Option<String>,
    linked_name: Option<&str>,
    x: Option<&str>,
    danbooru: Option<&str>,
    pixiv: Option<&str>,
) -> String {
    nickname
        .or_else(|| linked_name.map(str::to_owned))
        .or_else(|| x.map(|k| format!("@{k}")))
        .or_else(|| danbooru.map(|k| k.replace('_', " ")))
        .or_else(|| pixiv.map(|k| format!("Pixiv {k}")))
        .unwrap_or_else(|| "未知画师".into())
}

/// 目录排序和查询使用详情卡片的显示名；账号子串查询还会匹配同一画师的其他账号。
fn directory_name(c: &Connection) -> Result<String> {
    use rusqlite::functions::FunctionFlags;
    let flags = FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC;
    c.create_scalar_function("casefold", 1, flags, |ctx| Ok(crate::casefold::casefold(ctx.get_raw(0).as_str()?)))?;
    c.create_scalar_function("artist_display_name", 5, flags, |ctx| {
        let linked_name: Option<String> = ctx.get(1)?;
        let x: Option<String> = ctx.get(2)?;
        let danbooru: Option<String> = ctx.get(3)?;
        let pixiv: Option<String> = ctx.get(4)?;
        Ok(display_name(ctx.get(0)?, linked_name.as_deref(), x.as_deref(), danbooru.as_deref(), pixiv.as_deref()))
    })?;
    let first_link = |system| {
        format!("(SELECT l.key FROM term_links l WHERE l.term_id=t.id AND l.system='{system}' ORDER BY l.key LIMIT 1)")
    };
    Ok(format!(
        "casefold(artist_display_name({CORE_NICKNAME},{},{},{},{}))",
        first_link("name"),
        first_link("x"),
        first_link("danbooru"),
        first_link("pixiv"),
    ))
}

pub(crate) fn directory(c: &Connection, library: bool, q: &HashMap<String, String>) -> Result<Json> {
    let value = |key, default| q.get(key).map(String::as_str).unwrap_or(default);
    let page_number = |key, default| python_int(value(key, default)).map_err(|_| Error::invalid("分页参数不正确"));
    let limit = page_number("limit", "60")?;
    let offset = page_number("offset", "0")?;
    if !(1..=200).contains(&limit) || offset < 0 {
        return illustash_core::error::invalid("分页参数不正确");
    }
    let (order, sort_by_name) = match value("order", "") {
        "" | "count" => ("count DESC,key", false),
        "recent" => ("last DESC,key", false),
        "name" => ("sort_name,key", true),
        _ => return illustash_core::error::invalid("未知排序"),
    };
    let rating = rating("a", value("nsfw", ""))?;
    let reply = |items, total: i64| {
        Json::object([("items", Json::Array(items)), ("total", total.into()), ("offset", offset.into())])
    };
    if !library || !flag(c, "terms_artists")? {
        return Ok(reply(Vec::new(), 0i64));
    }
    let needle: String = trim(value("q", "")).chars().take(80).collect();
    let needle = crate::casefold::casefold(&needle);
    let name = if sort_by_name || !needle.is_empty() { directory_name(c)? } else { String::new() };
    let search = if needle.is_empty() {
        String::new()
    } else {
        format!(
            "AND t.id IN (
                SELECT t.id FROM terms t WHERE t.kind='artist' AND (
                    instr({name},?3)>0 OR EXISTS (
                        SELECT 1 FROM term_links l WHERE l.term_id=t.id
                        AND l.system IN ('x','x_id','pixiv','danbooru') AND instr(casefold(l.key),?3)>0)))"
        )
    };
    let sort_name = if sort_by_name { name.as_str() } else { "NULL" };
    // 从画师词条按 term_projection_term 取投影；不能从全库资产出发扫描每张图的题材投影。
    // 日期相同取 UUID 最小的那张，与图库默认排序一致。LEFT JOIN 让空页仍返回总数。
    let sql = format!(
        "WITH ranked AS (
             SELECT t.id,t.key,a.uuid,a.added_at AS last,
                    count(*) OVER (PARTITION BY t.id) AS count,
                    row_number() OVER (PARTITION BY t.id ORDER BY a.added_at DESC,a.uuid) AS pos
             FROM terms t
             CROSS JOIN term_projection p ON p.term_id=t.id AND p.state IN ('machine','confirmed')
             CROSS JOIN term_assets ta ON ta.id=p.asset_id AND ta.namespace='library' AND ta.removed_at IS NULL
             CROSS JOIN assets a ON a.uuid=ta.key
             WHERE t.kind='artist' AND {}{rating} {search}
         ), summary AS (
             SELECT id,key,uuid,last,count FROM ranked WHERE pos=1
         )
         SELECT total,key,uuid,last,count FROM (SELECT count(*) AS total FROM summary)
         LEFT JOIN (
             SELECT t.key AS key,s.uuid,s.last,s.count,{sort_name} AS sort_name
             FROM summary s JOIN terms t ON t.id=s.id
             ORDER BY {order} LIMIT ?1 OFFSET ?2
         ) AS page ON 1
         ORDER BY {order}",
        visible("a"),
    );
    let mut stmt = c.prepare(&sql)?;
    let mut rows = if needle.is_empty() {
        stmt.query(rusqlite::params![limit, offset])?
    } else {
        stmt.query(rusqlite::params![limit, offset, needle])?
    };
    let mut items = Vec::new();
    let mut total = 0i64;
    while let Some(row) = rows.next()? {
        total = row.get(0)?;
        let Some(key): Option<String> = row.get(1)? else { break };
        let mut item = card(c, &key, true)?;
        item.set("count", row.get::<_, i64>(4)?.into());
        item.set("last", row.get::<_, Option<String>>(3)?.into());
        item.set("cover", row.get::<_, String>(2)?.into());
        items.push(item);
    }
    Ok(reply(items, total))
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
/// 为 `IN (…)` 列表的 `n` 个参数生成 `?,?,…`。
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
    let n = value
        .get("count")
        .filter(|count| match count {
            Json::Scalar(serde_json::Value::Number(n)) => n.as_u64().is_some_and(|n| n > 0),
            Json::Integer(n) => !n.starts_with('-') && n.bytes().any(|digit| digit != b'0'),
            _ => false,
        })
        .map(py_string);
    Ok(match (method, n) {
        ("group_oc", Some(n)) => format!("同一原创角色里有 {n} 张是 TA 画的"),
        ("group_cluster", Some(n)) => format!("相似的未识别图里有 {n} 张是 TA 画的"),
        ("group_run", Some(n)) => format!("同一段保存的图里有 {n} 张是 TA 画的"),
        ("ocr", _) => "图上读到了 TA 的账号".into(),
        _ => "有线索指向 TA".into(),
    }
    .into())
}
/// 候选画师的卡片，附上推荐的理由。
fn suggested(mut info: Json, method: &str, data: &str) -> Result<Json> {
    info.set("reason", reason(method, data)?);
    Ok(info)
}
/// 画师的卡片，附上 `present`（没有时取全部 `assets`）中有几张是 TA 的、这张图上的裁决，
/// 以及在这张图上找到 TA 的方法。
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
// 同一套卡片、证据、候选和帖子的组装逻辑，留给之后的详情迁移使用。
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_reasons_on_core_and_legacy_paths_only_display_positive_integers() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE meta(key TEXT PRIMARY KEY,value TEXT);
            CREATE TABLE terms(id INTEGER PRIMARY KEY,key TEXT,kind TEXT);
            CREATE TABLE term_assets(id INTEGER PRIMARY KEY,key TEXT,namespace TEXT,removed_at TEXT);
            CREATE TABLE term_projection(asset_id INTEGER,term_id INTEGER,state TEXT);
            CREATE TABLE term_current_evidence(asset_id INTEGER,term_id INTEGER,method TEXT,data TEXT,score REAL);
            CREATE TABLE term_names(term_id INTEGER,value TEXT,role TEXT,preferred INTEGER,position INTEGER);
            CREATE TABLE term_links(term_id INTEGER,system TEXT,key TEXT,url TEXT);
            CREATE TABLE artist_verdicts(artist TEXT,uuid TEXT,verdict TEXT);
            INSERT INTO terms VALUES (1,'synthetic-artist','artist');
            INSERT INTO term_assets VALUES (1,'synthetic-image','library',NULL);
            INSERT INTO term_projection VALUES (1,1,'maybe');
            INSERT INTO term_current_evidence VALUES (1,1,'group_oc','{}',0.9);
            ATTACH DATABASE ':memory:' AS prov;
            CREATE TABLE prov.artists(id TEXT PRIMARY KEY,merged_into TEXT);
            CREATE TABLE prov.artist_links(artist_id TEXT,system TEXT,key TEXT,url TEXT);
            CREATE TABLE prov.image_artists(asset TEXT,artist_id TEXT,status TEXT,method TEXT,evidence TEXT,score REAL);
            CREATE TABLE prov.image_sources(asset TEXT,site TEXT,url TEXT,artist_id TEXT,method TEXT,post_id TEXT);
            INSERT INTO prov.artists VALUES ('synthetic-artist',NULL);
            INSERT INTO prov.image_artists VALUES ('synthetic-image','synthetic-artist','candidate','group_oc','{}',0.9);").unwrap();
        for core in [false, true] {
            c.execute("INSERT OR REPLACE INTO meta VALUES ('terms_artists',?)", [if core { "core" } else { "legacy" }])
                .unwrap();
            for method in ["group_oc", "group_cluster", "group_run"] {
                for count in [
                    None,
                    Some("true"),
                    Some("false"),
                    Some("null"),
                    Some("0"),
                    Some("-1"),
                    Some("1.0"),
                    Some("0.5"),
                    Some("[]"),
                    Some("{}"),
                    Some(r#""3""#),
                    Some("NaN"),
                    Some("Infinity"),
                    Some("1e400"),
                    Some("1"),
                    Some("123456789012345678901234567890"),
                ] {
                    let raw = count.map_or_else(|| "{}".to_owned(), |count| format!("{{\"count\":{count}}}"));
                    c.execute("UPDATE term_current_evidence SET method=?,data=?", [method, &raw]).unwrap();
                    c.execute("UPDATE prov.image_artists SET method=?,evidence=?", [method, &raw]).unwrap();
                    let before = c.total_changes();
                    let result = for_asset(&c, true, "synthetic-image", None, false).unwrap();
                    let Json::Array(maybe) = result.get("maybe").unwrap() else { panic!("候选必须是数组") };
                    assert_eq!(maybe.len(), 1);
                    let expected = match (method, count) {
                        ("group_oc", Some(n @ ("1" | "123456789012345678901234567890"))) => {
                            format!("同一原创角色里有 {n} 张是 TA 画的")
                        }
                        ("group_cluster", Some(n @ ("1" | "123456789012345678901234567890"))) => {
                            format!("相似的未识别图里有 {n} 张是 TA 画的")
                        }
                        ("group_run", Some(n @ ("1" | "123456789012345678901234567890"))) => {
                            format!("同一段保存的图里有 {n} 张是 TA 画的")
                        }
                        _ => "有线索指向 TA".into(),
                    };
                    assert_eq!(
                        maybe[0].field("reason").text(),
                        expected,
                        "core={core},method={method},count={count:?}"
                    );
                    assert_eq!(c.total_changes(), before, "理由读取不能改证据或裁决");
                    assert_eq!(
                        c.query_row("SELECT data FROM term_current_evidence", [], |r| r.get::<_, String>(0)).unwrap(),
                        raw
                    );
                    assert_eq!(
                        c.query_row("SELECT evidence FROM prov.image_artists", [], |r| r.get::<_, String>(0)).unwrap(),
                        raw
                    );
                }
            }
        }
    }
}
