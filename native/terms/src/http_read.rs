//! Deterministic HTTP reads, sharing one read transaction and the existing names rules.
use crate::{
    http::LibraryContext,
    http_json::Json,
    names_view::{Choice, View},
    Error, Result,
};
use rusqlite::{params, params_from_iter, types::ValueRef, Connection, OpenFlags, OptionalExtension};
use std::{collections::HashMap, sync::Arc};
use unicode_normalization::UnicodeNormalization;

#[cfg(test)]
pub const CORE_ROUTES: &[&str] = &[
    "/api/assets",
    "/api/assets/count",
    "/api/ids",
    "/api/detail",
    "/api/home",
    "/api/fold",
    "/api/review-pools",
    "/api/characters",
    "/api/tagged",
    "/api/character/profile",
    "/api/character/activity",
    "/api/character/stats",
    "/api/identity/count",
    "/api/identity/queue",
    "/api/bootstrap",
    "/api/user-tags",
    "/api/general-tags",
    "/api/artists",
    "/api/artist",
    "/api/artist-audit",
    "/api/entities",
    "/api/works",
    "/api/ask",
    "/api/similar",
    "/api/clusters",
];
pub const VISIBLE: &str = "NOT EXISTS(SELECT 1 FROM duplicates h WHERE h.uuid=m.uuid AND h.hidden=1) AND NOT EXISTS(SELECT 1 FROM excluded x WHERE x.uuid=m.uuid)";
pub const TREE: &str = "WITH RECURSIVE tree(id) AS (SELECT id FROM entities WHERE id=? AND kind='work' UNION SELECT e.id FROM entities e JOIN tree t ON e.parent_id=t.id WHERE e.kind='work')";

pub(crate) use crate::compat_text::{python_repr, trim, whitespace};
fn cut(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}
fn like(s: &str) -> String {
    format!("%{}%", s.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"))
}
pub(crate) fn normalize(s: &str) -> String {
    crate::casefold::casefold(
        &s.nfkc().filter(|c| !whitespace(*c) && !['・', '·', '･'].contains(c)).collect::<String>(),
    )
}
fn decode(s: &str) -> String {
    let mut out = Vec::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                let hex = |x: u8| (x as char).to_digit(16).map(|v| v as u8);
                if let (Some(a), Some(z)) = (hex(b[i + 1]), hex(b[i + 2])) {
                    out.push(a * 16 + z);
                    i += 2;
                } else {
                    out.push(b[i]);
                }
            }
            v => out.push(v),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}
pub fn query(url: &str) -> HashMap<String, String> {
    let mut result = HashMap::new();
    let url = url.split('#').next().unwrap_or(url);
    if let Some((_, q)) = url.split_once('?') {
        for part in q.split('&') {
            let (k, v) = part.split_once('=').unwrap_or((part, ""));
            if !v.is_empty() {
                result.insert(decode(k), decode(v));
            }
        }
    }
    result
}
pub fn remote(url: &str) -> bool {
    let path = url.split('?').next().unwrap_or(url);
    ["/api/entities", "/api/works"].contains(&path) && query(url).get("remote").is_some_and(|v| !v.is_empty())
}
pub fn flag(c: &Connection, key: &str) -> Result<bool> {
    Ok(c.query_row("SELECT value FROM meta WHERE key=?", [key], |r| r.get::<_, String>(0)).optional()?.as_deref()
        == Some("core"))
}
pub fn rows(c: &Connection, sql: &str, args: &[&str]) -> Result<Vec<Json>> {
    query_rows(c, sql, params_from_iter(args.iter()))
}
pub(crate) fn typed_rows(c: &Connection, sql: &str, args: &[rusqlite::types::Value]) -> Result<Vec<Json>> {
    query_rows(c, sql, params_from_iter(args.iter()))
}
fn query_rows(c: &Connection, sql: &str, args: impl rusqlite::Params) -> Result<Vec<Json>> {
    let mut s = c.prepare(sql)?;
    let columns: Vec<String> = s.column_names().iter().map(|n| n.to_string()).collect();
    let result = s
        .query_map(args, |r| {
            let mut fields = Vec::new();
            for (i, k) in columns.iter().enumerate() {
                let v = match r.get_ref(i)? {
                    ValueRef::Null => Json::from(None::<String>),
                    ValueRef::Integer(v) => v.into(),
                    ValueRef::Real(v) => Json::Float(v),
                    ValueRef::Text(v) => String::from_utf8_lossy(v).into_owned().into(),
                    ValueRef::Blob(_) => {
                        return Err(rusqlite::Error::InvalidColumnType(i, k.clone(), rusqlite::types::Type::Blob))
                    }
                };
                fields.push((k.clone(), v));
            }
            Ok(Json::Object(fields))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(result)
}
pub fn strings(c: &Connection, sql: &str, args: &[&str]) -> Result<Vec<String>> {
    Ok(c.prepare(sql)?.query_map(params_from_iter(args.iter()), |r| r.get(0))?.collect::<rusqlite::Result<_>>()?)
}
fn choice(v: Choice) -> Json {
    Json::object([
        ("name", v.name.into()),
        ("name_source", v.source.into()),
        (
            "names",
            Json::object([
                ("zh", v.names[0].clone().into()),
                ("ja", v.names[1].clone().into()),
                ("en", v.names[2].clone().into()),
            ]),
        ),
    ])
}
pub(crate) fn register_names(c: &Connection) -> Result<()> {
    if flag(c, "terms_names")? {
        return register_projected_names(c);
    }
    let fallback = Arc::new(View::load(c)?);
    let mut shown = HashMap::new();
    // The snapshot already holds every identity; no second read of entities (#102).
    for id in fallback.ids().cloned().collect::<Vec<_>>() {
        for skin in [0, 1] {
            if let Some((v, tag)) = fallback.entity(&id, skin != 0)? {
                let mut info = choice(v);
                info.set("tag", tag.into());
                shown.insert((id.clone(), skin), info);
            }
        }
    }
    let shown = Arc::new(shown);
    let names = shown.clone();
    let f = rusqlite::functions::FunctionFlags::SQLITE_UTF8;
    c.create_scalar_function("entity_name", 1, f, move |ctx| {
        let id = ctx.get::<Option<String>>(0)?.unwrap_or_default();
        Ok(names.get(&(id, 0)).and_then(|v| v.get("name")).map(|v| v.text().to_owned()).unwrap_or_default())
    })?;
    c.create_scalar_function("entity_info", 2, f, move |ctx| {
        let id = ctx.get::<Option<String>>(0)?.unwrap_or_default();
        let skin = ctx.get::<i64>(1)?;
        Ok(shown.get(&(id, i64::from(skin != 0))).cloned().unwrap_or(Json::Object(Vec::new())).dump())
    })?;
    c.create_scalar_function("work_name", 1, f, move |ctx| {
        let key = ctx.get::<String>(0)?;
        fallback.choose("work", &key).map(|v| v.name).map_err(|e| rusqlite::Error::UserFunctionError(Box::new(e)))
    })?;
    Ok(())
}
// Render only names actually requested, memoized for this connection/read transaction.
// No library-wide snapshots or cross-request caches: writes and library switches cannot
// reuse stale names, and an empty page does not load every identity's legacy names.
fn register_projected_names(c: &Connection) -> Result<()> {
    let f = rusqlite::functions::FunctionFlags::SQLITE_UTF8;
    let mut names: HashMap<String, String> = HashMap::new();
    c.create_scalar_function("entity_name", 1, f, move |ctx| {
        let id = ctx.get::<Option<String>>(0)?.unwrap_or_default();
        if let Some(name) = names.get(&id) {
            return Ok(name.clone());
        }
        // SAFETY: HTTP owns the connection on one thread. The non-owning handle
        // stays within this callback and reads the same request transaction.
        let c = unsafe { ctx.get_connection()? };
        let name: String = c
            .query_row("SELECT name FROM term_name_display WHERE entity=? AND skin_only=0", [&id], |r| r.get(0))
            .optional()?
            .unwrap_or_default();
        names.insert(id, name.clone());
        Ok(name)
    })?;
    let mut shown: HashMap<(String, i64), String> = HashMap::new();
    c.create_scalar_function("entity_info", 2, f, move |ctx| {
        let id = ctx.get::<Option<String>>(0)?.unwrap_or_default();
        let skin = i64::from(ctx.get::<i64>(1)? != 0);
        let key = (id, skin);
        if let Some(info) = shown.get(&key) {
            return Ok(info.clone());
        }
        // SAFETY: as above; no connection reference escapes the callback.
        let c = unsafe { ctx.get_connection()? };
        let info = c
            .query_row(
                "SELECT name,name_source,zh,ja,en,tag FROM term_name_display WHERE entity=? AND skin_only=?",
                params![&key.0, skin],
                |r| {
                    Ok(Json::object([
                        ("name", r.get::<_, String>(0)?.into()),
                        ("name_source", r.get::<_, String>(1)?.into()),
                        (
                            "names",
                            Json::object([
                                ("zh", r.get::<_, Option<String>>(2)?.into()),
                                ("ja", r.get::<_, Option<String>>(3)?.into()),
                                ("en", r.get::<_, Option<String>>(4)?.into()),
                            ]),
                        ),
                        ("tag", r.get::<_, String>(5)?.into()),
                    ])
                    .dump())
                },
            )
            .optional()?
            .unwrap_or_else(|| "{}".into());
        shown.insert(key, info.clone());
        Ok(info)
    })?;
    let mut chosen: HashMap<String, String> = HashMap::new();
    c.create_scalar_function("work_name", 1, f, move |ctx| {
        let key = ctx.get::<String>(0)?;
        if let Some(name) = chosen.get(&key) {
            return Ok(name.clone());
        }
        // SAFETY: as above; the fallback also reads this transaction, never a new connection.
        let c = unsafe { ctx.get_connection()? };
        let name: Option<String> = c
            .query_row("SELECT name FROM term_name_choice WHERE kind='work' AND key=?", [&key], |r| r.get(0))
            .optional()?;
        let name = match name {
            Some(name) => name,
            // A key the projection does not hold: chosen from that key's own rows (#105), no library-wide load.
            None => crate::names_view::choose_key(&c, "work", &key)
                .map(|v| v.name)
                .map_err(|e| rusqlite::Error::UserFunctionError(Box::new(e)))?,
        };
        chosen.insert(key, name.clone());
        Ok(name)
    })?;
    Ok(())
}
pub(crate) fn display(c: &Connection, v: Json) -> Result<Json> {
    display_skin(c, v, false)
}
pub(crate) fn display_skin(c: &Connection, mut v: Json, skin: bool) -> Result<Json> {
    let id = v.get("id").expect("http_read internal invariant").text().to_owned();
    let info: String = c.query_row("SELECT entity_info(?,?)", params![&id, skin as i64], |r| r.get(0))?;
    v.extend(serde_json::from_str(&info).map_err(|e| Error::Invalid(e.to_string()))?);
    let entity:Option<(String,bool,Option<String>)>=c.query_row("SELECT e.kind,e.original,coalesce(p.work_id,e.work_id) FROM entities e LEFT JOIN entities p ON p.id=e.parent_id AND e.kind='skin' WHERE e.id=?",[&id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
    let mut work = None;
    if let Some((kind, original, w)) = entity {
        if v.get("kind").is_some() && kind != "work" {
            v.set("kind", if original { "oc" } else { "" }.into());
        }
        v.set("work_id", w.clone().into());
        work = w;
    }
    if v.get("work").is_some() {
        let name = if let Some(w) = work {
            c.query_row("SELECT entity_name(?)", [w], |r| r.get::<_, String>(0))?
        } else {
            String::new()
        };
        v.set("work_name", name.into());
    }
    if c.query_row("SELECT value FROM meta WHERE key='identity_policy'", [], |r| r.get::<_, String>(0))
        .optional()?
        .as_deref()
        == Some("hybrid")
    {
        if let Some(node) =
            rows(c, "SELECT node_type,character_id,variant_id,canonical_id FROM identity_nodes WHERE id=?", &[&id])?
                .into_iter()
                .next()
        {
            v.extend(node);
        }
    }
    Ok(v)
}
fn entities(c: &Connection, q: &str) -> Result<Json> {
    let q = cut(trim(q), 160);
    c.execute_batch("CREATE TEMP TABLE IF NOT EXISTS name_matches(entity TEXT PRIMARY KEY,rank INTEGER NOT NULL); DELETE FROM name_matches;")?;
    if !q.is_empty() {
        let mut hits: HashMap<String, i64> = HashMap::new();
        for (id, rank) in crate::names_search::hits(c, &q)? {
            let id = crate::character_commands::resolve(c, &id)?;
            let v = hits.entry(id).or_insert(-1);
            *v = (*v).max(rank);
        }
        for (id, rank) in hits {
            c.execute("INSERT INTO name_matches VALUES (?,?)", params![id, rank])?;
        }
    }
    let matched = if q.is_empty() { "" } else { "JOIN name_matches sm ON sm.entity=e.id" };
    let ranking = if q.is_empty() { "" } else { "sm.rank DESC," };
    let sql=format!("SELECT e.id,entity_name(e.id) AS name,e.work,e.kind,e.artist,e.label_only,e.mascot,e.parent_id,
      (SELECT count(*) FROM (SELECT uuid FROM memberships WHERE entity=e.id UNION ALL SELECT uuid FROM tag_memberships WHERE entity=e.id) m WHERE {VISIBLE}) AS count,
      (SELECT group_concat(value,' · ') FROM (SELECT DISTINCT n.value FROM names n WHERE n.entity_id=e.id AND n.value LIKE ? ESCAPE '\\' AND n.value<>entity_name(e.id) LIMIT 2)) AS matched
      FROM entities e {matched} WHERE e.parent_id IS NULL AND e.kind<>'work' AND e.merged_into IS NULL ORDER BY {ranking}count DESC,entity_name(e.id) LIMIT 30");
    let result = rows(c, &sql, &[&like(&q)])?.into_iter().map(|v| display(c, v)).collect::<Result<Vec<_>>>()?;
    c.execute("DELETE FROM name_matches", [])?;
    Ok(Json::Array(result))
}
pub(crate) fn works(c: &Connection, q: &str) -> Result<Json> {
    let needle = normalize(trim(q));
    let mut result = Vec::new();
    for mut v in rows(c,"SELECT e.id,e.parent_id,entity_name(e.id) AS name,(SELECT key FROM links WHERE entity_id=e.id AND system='danbooru' AND current=1 LIMIT 1) AS tag,(SELECT key FROM links WHERE entity_id=e.id AND system='qualifier' LIMIT 1) AS qualifier,coalesce(w.status,'pending') AS status,w.note FROM entities e LEFT JOIN work_review w ON w.entity_id=e.id WHERE e.kind='work' AND e.merged_into IS NULL",&[])? {
        let id=v.get("id").expect("http_read internal invariant").text();
        let count:i64=c.query_row(&format!("{TREE} SELECT count(DISTINCT m.uuid) FROM memberships m JOIN entities e ON e.id=m.entity WHERE e.work_id IN (SELECT id FROM tree) AND {VISIBLE}"),[id],|r|r.get(0))?;
        let mut values=vec![v.get("name").expect("http_read internal invariant").text().to_owned(),v.get("tag").expect("http_read internal invariant").text().to_owned(),v.get("qualifier").expect("http_read internal invariant").text().to_owned()];
        values.extend(strings(c,"SELECT value FROM names WHERE entity_id=?",&[id])?);
        let watchable=!v.get("tag").expect("http_read internal invariant").text().is_empty();v.set("count",count.into());v.set("watchable",watchable.into());
        if needle.is_empty() || values.iter().any(|s|normalize(s).contains(&needle)||normalize(&s.replace('_'," ")).contains(&needle)){result.push(v);}
    }
    result.sort_by(|a, b| {
        b.get("count")
            .expect("http_read internal invariant")
            .integer()
            .cmp(&a.get("count").expect("http_read internal invariant").integer())
            .then(
                a.get("name")
                    .expect("http_read internal invariant")
                    .text()
                    .cmp(b.get("name").expect("http_read internal invariant").text()),
            )
            .then(
                a.get("id")
                    .expect("http_read internal invariant")
                    .text()
                    .cmp(b.get("id").expect("http_read internal invariant").text()),
            )
    });
    Ok(Json::Array(result))
}
pub(crate) fn sqlite_uri(path: &std::path::Path) -> String {
    let path = crate::sqlite::path(path);
    let path = path.to_string_lossy();
    let encoded: String = path
        .as_bytes()
        .iter()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"/-._~:".contains(b) {
                (*b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect();
    format!("file:{encoded}?mode=ro")
}

// Python Store loads this immutable CSV once per startup. Cache only that vocabulary,
// keyed by the private startup generation and CSV path; never retain a library connection.
type TagVocabulary = (String, std::path::PathBuf, Arc<Vec<String>>);
pub(crate) fn tag_vocabulary(context: &LibraryContext) -> Result<Arc<Vec<String>>> {
    static TAGS: std::sync::Mutex<Option<TagVocabulary>> = std::sync::Mutex::new(None);
    let mut cached = TAGS.lock().map_err(|_| Error::Invalid("CSV 缓存锁损坏".into()))?;
    if let Some((generation, path, tags)) = &*cached {
        if generation == &context.generation && path == &context.tags {
            return Ok(tags.clone());
        }
    }
    let mut csv = csv::Reader::from_path(&context.tags).map_err(|e| Error::Invalid(e.to_string()))?;
    let headers = csv.headers().map_err(|e| Error::Invalid(e.to_string()))?.clone();
    let name = headers.iter().position(|v| v == "name").ok_or_else(|| Error::Invalid("CSV 缺少 name".into()))?;
    let category =
        headers.iter().position(|v| v == "category").ok_or_else(|| Error::Invalid("CSV 缺少 category".into()))?;
    let mut tags = Vec::new();
    for r in csv.records() {
        let r = r.map_err(|e| Error::Invalid(e.to_string()))?;
        if r.get(category) == Some("0") {
            tags.push(r.get(name).unwrap_or("").to_owned());
        }
    }
    let tags = Arc::new(tags);
    *cached = Some((context.generation.clone(), context.tags.clone(), tags.clone()));
    Ok(tags)
}

pub fn read(context: &LibraryContext, url: &str) -> Result<Option<(Json, u16)>> {
    let q = query(url);
    let value = |key: &str| q.get(key).map(String::as_str).unwrap_or("");
    let path = url.split('?').next().unwrap_or(url);
    if path == "/api/general-tags" {
        let q = cut(&crate::casefold::casefold(trim(value("q"))), 80);
        let vocabulary = tag_vocabulary(context)?;
        let tags = vocabulary
            .iter()
            .filter(|n| crate::casefold::casefold(n).contains(&q))
            .take(30)
            .map(|n| n.as_str().into())
            .collect();
        return Ok(Some((Json::Array(tags), 200)));
    }
    let audit_n = if path == "/api/artist-audit" {
        let n = q.get("n").map(String::as_str).unwrap_or("50");
        let n = match python_int(n) {
            Ok(n) => n,
            Err(message) => return Ok(Some((Json::object([("error", message.into())]), 400))),
        };
        if !(1..=500).contains(&n) {
            return Ok(Some((Json::object([("error", "抽查张数要在 1–500 之间".into())]), 400)));
        }
        n as usize
    } else {
        50
    };
    let local = crate::http_local::ROUTES.contains(&path);
    let browse =
        ["/api/assets", "/api/assets/count", "/api/ids", "/api/detail", "/api/home", "/api/fold", "/api/review-pools"]
            .contains(&path);
    let characters = crate::http_characters::ROUTES.contains(&path);
    let validation = if browse {
        crate::http_browse::validate(path, &q)
    } else if characters {
        crate::http_characters::validate(path, &q)
    } else {
        Ok(())
    };
    match validation {
        Ok(()) => {}
        Err(Error::Invalid(m)) => return Ok(Some((Json::object([("error", m.into())]), 400))),
        Err(e) => return Err(e),
    }
    if path == "/api/detail" && context.source.is_none() {
        return Ok(None);
    }
    if path == "/api/bootstrap" && context.token.is_none() {
        return Ok(None);
    }
    let mut c =
        crate::sqlite::open_with_flags(&context.db, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI)?;
    c.busy_timeout(std::time::Duration::from_secs(10))?;
    let tx = c.transaction()?;
    // Startup publishes readiness before the existing background cutover finishes. Its
    // failure reason lives in Python and can change after publication. Until all four
    // modules are ready, bootstrap must use that live refusal, not a stale startup value.
    if path == "/api/bootstrap"
        && context.read_only.is_none()
        && !tx.query_row(
            "SELECT count(*)=4 FROM meta WHERE key IN ('terms_artists','terms_labels','terms_characters','terms_names') AND value='core'",
            [],
            |r| r.get::<_, bool>(0),
        )?
    {
        return Ok(None);
    }
    let auxiliary_needed = path == "/api/artist-audit" || (path == "/api/artist" && !flag(&tx, "terms_artists")?);
    if let Some(library) = context.library.as_ref().filter(|_| auxiliary_needed) {
        let prov = library.join("provenance.sqlite");
        if prov.exists() {
            tx.execute("ATTACH DATABASE ? AS prov", [sqlite_uri(&prov)])?;
        }
    }
    if characters || browse || local || path == "/api/bootstrap" {
        // Store.connect probes the names switch before yielding, even without a name renderer.
        flag(&tx, "terms_names")?;
        // Counts, IDs and raw identity/prefs reads do not render names. Keep their query path local.
        if [
            "/api/assets",
            "/api/detail",
            "/api/home",
            "/api/characters",
            "/api/tagged",
            "/api/character/profile",
            "/api/identity/queue",
            "/api/bootstrap",
        ]
        .contains(&path)
        {
            register_names(&tx)?;
        }
        let result = if local {
            register_names(&tx)?;
            crate::http_local::read(&tx, path, &q)
        } else if characters {
            crate::http_characters::read(&tx, context, path, &q)
        } else if browse {
            crate::http_browse::read(&tx, context, path, &q)
        } else {
            crate::http_app::read(&tx, context, &q)
        };
        return match result {
            Ok(v) => {
                tx.commit()?;
                Ok(Some(v))
            }
            Err(Error::Invalid(m)) => Ok(Some((Json::object([("error", m.into())]), 400))),
            Err(e) => Err(e),
        };
    }
    let result=match path {
        "/api/user-tags"=>Json::Array(rows(&tx,&format!("SELECT tag,count(*) AS count FROM asset_tags m JOIN assets USING(uuid) WHERE {VISIBLE} AND tag LIKE ? ESCAPE '\\' GROUP BY tag ORDER BY count DESC,tag LIMIT 60"),&[&like(&cut(trim(value("q")),60))])?),
        "/api/artists"=>Json::Array(rows(&tx,"SELECT artist,count(*) AS characters FROM entities WHERE original=1 AND artist<>'' AND merged_into IS NULL GROUP BY artist ORDER BY characters DESC,artist",&[])?),
        "/api/entities"=>{register_names(&tx)?;entities(&tx,value("q"))?},
        "/api/works"=>{register_names(&tx)?;works(&tx,value("q"))?},
        "/api/artist"=>Json::object([("id",value("id").into()),("name",crate::http_artists::name_of(&tx,context.library.is_some(),value("id"))?)]),
        "/api/artist-audit"=>crate::http_artists::audit(&tx,context.library.is_some(),audit_n)?,
        _=>return Err(Error::Invalid("未迁移接口".into())),
    };
    tx.commit()?;
    Ok(Some((result, 200)))
}
pub(crate) fn python_int(s: &str) -> std::result::Result<i64, String> {
    let mut converted = String::new();
    for c in s.trim_matches(|c| whitespace(c) && !('\u{1c}'..='\u{1f}').contains(&c)).chars() {
        converted.push(match crate::compat_unicode::decimal(c) {
            Some(n) => (b'0' + n) as char,
            None => c,
        });
    }
    let digits = converted.trim_start_matches(['+', '-']);
    let valid = !digits.is_empty()
        && digits.as_bytes().first().is_some_and(u8::is_ascii_digit)
        && digits.as_bytes().last().is_some_and(u8::is_ascii_digit)
        && !digits.contains("__")
        && digits.bytes().all(|c| c.is_ascii_digit() || c == b'_')
        && converted.chars().filter(|c| *c == '+' || *c == '-').count() <= 1;
    if valid {
        let digits = digits.bytes().filter(u8::is_ascii_digit).count();
        if digits > 4300 {
            return Err(format!("Exceeds the limit (4300 digits) for integer string conversion: value has {digits} digits; use sys.set_int_max_str_digits() to increase the limit"));
        }
        return Ok(converted.replace('_', "").parse::<i64>().unwrap_or(if converted.starts_with('-') {
            i64::MIN
        } else {
            i64::MAX
        }));
    }
    Err(format!("invalid literal for int() with base 10: {}", python_repr(s)))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(c: &Connection) {
        c.execute_batch("CREATE TABLE meta(key TEXT PRIMARY KEY,value TEXT);
          INSERT INTO meta VALUES ('terms_names','core');
          CREATE TABLE entities(id TEXT PRIMARY KEY,kind TEXT,parent_id TEXT,variant TEXT,merged_into TEXT,original INTEGER,work TEXT,artist TEXT,label_only INTEGER,mascot INTEGER,work_id TEXT);
          INSERT INTO entities VALUES ('x','character',NULL,'',NULL,0,'','',0,0,NULL);
          CREATE TABLE links(system TEXT,key TEXT,entity_id TEXT,current INTEGER,added_at TEXT);
          CREATE TABLE names(entity_id TEXT,lang TEXT,value TEXT,source TEXT,preferred INTEGER,role TEXT);
          INSERT INTO names VALUES ('x','zh','x','user',1,'display');
          CREATE TABLE display_names(kind TEXT,key TEXT,lang TEXT,source TEXT,value TEXT);
          INSERT INTO display_names VALUES ('work','unprojected','zh','user','未投影🙂');
          CREATE TABLE name_aliases(kind TEXT,key TEXT,value TEXT);
          CREATE TABLE assets(uuid TEXT PRIMARY KEY);
          CREATE TABLE memberships(uuid TEXT,entity TEXT);
          CREATE TABLE tag_memberships(uuid TEXT,entity TEXT);
          CREATE TABLE duplicates(uuid TEXT,hidden INTEGER);
          CREATE TABLE excluded(uuid TEXT);") .unwrap();
        c.execute_batch(crate::names_view::TABLES).unwrap();
        c.execute("INSERT INTO term_name_display VALUES ('x',0,'before','user','中','日',NULL,'tag')", []).unwrap();
        c.execute("INSERT INTO term_name_choice VALUES ('work','covered','projected','user',NULL,NULL,NULL)", [])
            .unwrap();
    }
    #[test]
    fn projected_names_do_not_load_legacy_tables() {
        let c = Connection::open_in_memory().unwrap();
        fixture(&c);
        // Projected entity/covered-work lookups have no reason to build View.
        c.execute_batch("DROP TABLE links; DROP TABLE names; DROP TABLE display_names;").unwrap();
        register_names(&c).unwrap();
        let values: (String, String, String, String) = c
            .query_row(
                "SELECT entity_name('x'),entity_info('x',0),entity_info('absent',1),work_name('covered')",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(values.0, "before");
        assert_eq!(
            values.1,
            r#"{"name": "before", "name_source": "user", "names": {"zh": "中", "ja": "日", "en": null}, "tag": "tag"}"#
        );
        assert_eq!(values.2, "{}");
        assert_eq!(values.3, "projected");
        // A work key outside the projection reads only its own display names (#105).
        let c = Connection::open_in_memory().unwrap();
        fixture(&c);
        c.execute_batch("DROP TABLE entities; DROP TABLE links; DROP TABLE names;").unwrap();
        register_names(&c).unwrap();
        let name: String = c.query_row("SELECT work_name('unprojected')", [], |r| r.get(0)).unwrap();
        assert_eq!(name, "未投影🙂");
    }
    #[test]
    fn lazy_projected_names_keep_snapshot_and_refresh_on_next_request() {
        let path = std::env::temp_dir().join(format!(
            "illustash-http-lazy-names-{}-{}.sqlite",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        let mut c = crate::sqlite::open(&path).unwrap();
        c.pragma_update(None, "journal_mode", "WAL").unwrap();
        fixture(&c);
        {
            let tx = c.transaction().unwrap();
            register_names(&tx).unwrap();
            let writer = crate::sqlite::open(&path).unwrap();
            writer.execute_batch("UPDATE term_name_display SET name='after'; UPDATE term_name_choice SET name='new work'; UPDATE display_names SET value='新🙂';").unwrap();
            // First use is AFTER the concurrent commit, including the legacy fallback.
            for _ in 0..2 {
                let values: (String, String, String, String) = tx
                    .query_row(
                        "SELECT entity_name('x'),entity_info('x',0),work_name('covered'),work_name('unprojected')",
                        [],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                    )
                    .unwrap();
                assert_eq!(values.0, "before");
                assert!(values.1.contains(r#""name": "before""#));
                assert_eq!(values.2, "projected");
                assert_eq!(values.3, "未投影🙂");
            }
            tx.commit().unwrap();
        }
        {
            let tx = c.transaction().unwrap();
            register_names(&tx).unwrap();
            let values: (String, String, String, String) = tx
                .query_row(
                    "SELECT entity_name('x'),entity_info('x',0),work_name('covered'),work_name('unprojected')",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .unwrap();
            assert_eq!(values.0, "after");
            assert!(values.1.contains(r#""name": "after""#));
            assert_eq!(values.2, "new work");
            assert_eq!(values.3, "新🙂");
        }
        drop(c);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn names_business_queries_share_snapshot_and_missing_key_fallbacks() {
        let path = std::env::temp_dir().join(format!(
            "illustash-http-tx-{}-{}.sqlite",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        let mut c = crate::sqlite::open(&path).unwrap();
        c.pragma_update(None, "journal_mode", "WAL").unwrap();
        fixture(&c);
        {
            let tx = c.transaction().unwrap();
            register_names(&tx).unwrap();
            let writer = crate::sqlite::open(&path).unwrap();
            writer.execute("UPDATE term_name_display SET name='after' WHERE entity='x'", []).unwrap();
            let values: (String, String) = tx
                .query_row("SELECT entity_name('x'),(SELECT name FROM term_name_display WHERE entity='x')", [], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })
                .unwrap();
            assert_eq!(values, ("before".into(), "before".into()));
            let missing:(String,String,String,String)=tx.query_row("SELECT entity_name('absent'),entity_info('absent',0),work_name('unprojected'),work_name('covered')",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).unwrap();
            assert_eq!(missing, ("".into(), "{}".into(), "未投影🙂".into(), "projected".into()));
            assert_ne!(entities(&tx, "x").unwrap().dump(), "[]");
            assert_eq!(tx.query_row("SELECT count(*) FROM name_matches", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
            assert_eq!(entities(&tx, "absent").unwrap().dump(), "[]");
            assert_ne!(entities(&tx, "").unwrap().dump(), "[]");
            tx.commit().unwrap();
        }
        {
            let tx = c.transaction().unwrap();
            register_names(&tx).unwrap();
            assert_eq!(tx.query_row("SELECT entity_name('x')", [], |r| r.get::<_, String>(0)).unwrap(), "after");
            tx.commit().unwrap();
        }
        drop(c);
        std::fs::remove_file(&path).unwrap();
    }
    #[test]
    fn python_query_and_integer_contract() {
        assert_eq!(query("/api/entities?q=a&q=&q=%FF%20b&remote=&n=2#n=3").get("q").unwrap(), "� b");
        assert!(!remote("/api/entities?remote="));
        assert!(remote("/api/entities?remote=0"));
        assert_eq!(python_int(" １２_３ ").unwrap(), 123);
        assert_eq!(python_int("\u{1c}2\u{1f}").unwrap_err(), "invalid literal for int() with base 10: '\\x1c2\\x1f'");
        assert_eq!(python_int("\u{200b}").unwrap_err(), "invalid literal for int() with base 10: '\\u200b'");
        assert_eq!(normalize(" Ｓｅｒｉｅｓ・Ｑ "), "seriesq");
        assert_eq!(unicode_normalization::UNICODE_VERSION, (15, 0, 0));
    }
}
