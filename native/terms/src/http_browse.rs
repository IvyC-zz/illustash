//! Browsing reads preserve the Store SQL, field overwrite order and request-local snapshot.
use crate::{
    http::LibraryContext,
    http_json::Json,
    http_read::{display, flag, python_int, rows, strings, trim, typed_rows},
    Error, Result,
};
use rusqlite::{types::Value, Connection, OptionalExtension};
use std::collections::{HashMap, HashSet};
const HINTS: &str =
    "(SELECT uuid,entity,score,0 AS weak FROM family_evidence UNION ALL SELECT uuid,entity,score,1 FROM weak_claims)";
const MIXED:&str="EXISTS(SELECT 1 FROM duplicates y WHERE y.grp=d.grp AND y.uuid<>d.uuid AND (EXISTS(SELECT entity FROM memberships WHERE uuid=d.uuid EXCEPT SELECT entity FROM memberships WHERE uuid=y.uuid) OR EXISTS(SELECT entity FROM memberships WHERE uuid=y.uuid EXCEPT SELECT entity FROM memberships WHERE uuid=d.uuid)))";
fn leads() -> String {
    format!("(SELECT v.* FROM {HINTS} v WHERE NOT EXISTS(SELECT 1 FROM assertions s WHERE s.uuid=v.uuid AND s.entity=v.entity AND s.status='rejected'))")
}
fn unfolded(a: &str) -> String {
    format!("NOT EXISTS(SELECT 1 FROM duplicates h WHERE h.uuid={a}.uuid AND h.hidden=1)")
}
fn visible(a: &str) -> String {
    format!("{} AND NOT EXISTS(SELECT 1 FROM excluded x WHERE x.uuid={a}.uuid)", unfolded(a))
}
fn audit_open(a: &str) -> String {
    format!("EXISTS(SELECT 1 FROM audit_sample u WHERE u.uuid={a}.uuid) AND NOT EXISTS(SELECT 1 FROM asked k JOIN audit_sample u ON u.uuid=k.uuid AND u.entity=k.entity WHERE k.uuid={a}.uuid AND k.answer<>'later') AND NOT EXISTS(SELECT 1 FROM audit_sample u JOIN assertions s ON s.uuid=u.uuid JOIN entities e ON e.id=s.entity WHERE u.uuid={a}.uuid AND (coalesce(e.parent_id,e.id)=u.entity OR (s.status='confirmed' AND u.solo=1)))")
}
pub(crate) fn rating(a: &str, nsfw: &str) -> Result<String> {
    match nsfw{""=>Ok(String::new()),"hide"|"only"=>Ok(format!(" AND {}coalesce({a}.uuid IN (SELECT uuid FROM asset_rating WHERE rating IN ('questionable', 'explicit')),0)",if nsfw=="hide"{"NOT "}else{""})),_=>Err(Error::Invalid("未知内容筛选".into()))}
}
pub(crate) fn day_bounds(day: &str) -> Result<(String, String)> {
    let bad = || Error::Invalid("日期不正确".into());
    let parts: Vec<_> = day.split('-').collect();
    if parts.len() != 3 {
        return Err(bad());
    }
    let decimal = |s: &str| -> Option<i32> {
        s.chars().try_fold(0i32, |n, c| crate::compat_unicode::decimal(c).map(|d| n * 10 + i32::from(d)))
    };
    if parts[0].chars().count() != 4 || parts[1].len() > 2 || parts[2].len() > 2 {
        return Err(bad());
    }
    if !parts[1].bytes().all(|c| c.is_ascii_digit())
        || !parts[2].strip_prefix(' ').unwrap_or(parts[2]).bytes().all(|c| c.is_ascii_digit())
    {
        return Err(bad());
    }
    let y = decimal(parts[0]).ok_or_else(bad)?;
    let m = decimal(parts[1]).ok_or_else(bad)?;
    let d = decimal(parts[2].strip_prefix(' ').unwrap_or(parts[2])).ok_or_else(bad)?;
    let leap = |y: i32| y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let size = match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if leap(y) {
                29
            } else {
                28
            }
        }
        _ => return Err(bad()),
    };
    if y == 0 || d < 1 || d > size {
        return Err(bad());
    }
    // Use the host calendar conversion, including its representable-year limits,
    // exactly where Python day_bounds calls mktime/localtime.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    tm.tm_year = y - 1900;
    tm.tm_mon = m - 1;
    tm.tm_mday = d + 1;
    tm.tm_hour = 12;
    tm.tm_isdst = -1;
    let stamp = crate::platform_time::local_timestamp(&mut tm);
    if stamp == -1 {
        return Err(Error::Io(std::io::Error::other("mktime argument out of range")));
    }
    Ok((day.to_owned(), format!("{:04}-{:02}-{:02}", tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday)))
}
fn value<'a>(q: &'a HashMap<String, String>, k: &str) -> &'a str {
    q.get(k).map(String::as_str).unwrap_or("")
}
fn null() -> Json {
    None::<String>.into()
}
fn get(v: &Json, k: &str) -> Json {
    v.get(k).cloned().unwrap_or_else(null)
}
fn count(c: &Connection, sql: &str, args: &[&str]) -> Result<i64> {
    Ok(c.query_row(sql, rusqlite::params_from_iter(args), |r| r.get(0))?)
}
fn exists(c: &Connection, sql: &str, args: &[&str]) -> Result<bool> {
    Ok(count(c, &format!("SELECT EXISTS({sql})"), args)? != 0)
}
/// Parameter conversion runs before opening any databases, as in the Python dispatcher.
fn preflight(path: &str, q: &HashMap<String, String>) -> Result<()> {
    let keys: Vec<(&str, &str)> = match path {
        "/api/assets" | "/api/ids" => vec![("offset", "0"), ("limit", "100")],
        "/api/home" => vec![("days", "5")],
        _ => return Ok(()),
    };
    let mut parsed = Vec::new();
    for (k, d) in keys {
        parsed.push(python_int(q.get(k).map(String::as_str).unwrap_or(d)).map_err(Error::Invalid)?);
    }
    if path == "/api/assets" && (!(0..=10000000).contains(&parsed[0]) || !(1..=200).contains(&parsed[1])) {
        return Err(Error::Invalid("分页参数不正确".into()));
    }
    Ok(())
}
fn attach_provenance(c: &Connection, context: &LibraryContext) -> Result<()> {
    if let Some(library) = &context.library {
        let path = library.join("provenance.sqlite");
        if path.exists() {
            c.execute("ATTACH DATABASE ? AS prov", [crate::http_read::sqlite_uri(&path)])?;
        }
    }
    Ok(())
}
// Python isdigit includes Digit (not only Decimal) Unicode 15 characters.
fn digit_extra(c: char) -> bool {
    const R: &[(u32, u32)] = &[
        (0xb2, 0xb3),
        (0xb9, 0xb9),
        (0x1369, 0x1371),
        (0x19da, 0x19da),
        (0x2070, 0x2070),
        (0x2074, 0x2079),
        (0x2080, 0x2089),
        (0x2460, 0x2468),
        (0x2474, 0x247c),
        (0x2488, 0x2490),
        (0x24ea, 0x24ea),
        (0x24f5, 0x24fd),
        (0x24ff, 0x24ff),
        (0x2776, 0x277e),
        (0x2780, 0x2788),
        (0x278a, 0x2792),
        (0x10a40, 0x10a43),
        (0x10e60, 0x10e68),
        (0x11052, 0x1105a),
        (0x1f100, 0x1f10a),
    ];
    R.iter().any(|&(a, b)| (a..=b).contains(&(c as u32)))
}
pub(crate) fn selection(
    c: &Connection,
    context: &LibraryContext,
    f: &HashMap<String, String>,
) -> Result<(String, Vec<Value>, String)> {
    let allowed = [
        "entity", "variant", "status", "q", "tag", "utag", "pool", "order", "nsfw", "dup", "reason", "cluster", "work",
        "day", "artist",
    ];
    if f.keys().any(|k| !allowed.contains(&k.as_str())) {
        return Err(Error::Invalid("筛选参数不正确".into()));
    }
    let v = |k| value(f, k);
    let mut entity = v("entity").to_owned();
    let mut variant = v("variant").to_owned();
    let mut clauses = vec!["1".to_owned()];
    let mut args = Vec::new();
    let mut numeric_index = None;
    if !v("day").is_empty() {
        let (lo, hi) = day_bounds(v("day"))?;
        clauses.push("a.added_at>=? AND a.added_at<?".into());
        args.extend([lo, hi]);
    }
    if !v("artist").is_empty() {
        if context.library.is_none() {
            return Err(Error::Invalid("这个图库还没有画师数据".into()));
        }
        clauses.push("a.uuid IN (SELECT value FROM json_each(?))".into());
        if !flag(c, "terms_artists")? {
            attach_provenance(c, context)?;
        }
        args.push(
            Json::Array(crate::http_artists::assets_of(c, true, v("artist"))?.into_iter().map(Into::into).collect())
                .dump(),
        );
    }
    if !v("work").is_empty() {
        if !exists(c, "SELECT 1 FROM entities WHERE id=? AND kind='work' AND merged_into IS NULL", &[v("work")])? {
            return Err(Error::Invalid("作品不存在".into()));
        }
        let ids = strings(c, &format!("{} SELECT id FROM tree", crate::http_read::TREE), &[v("work")])?;
        clauses.push(format!("EXISTS(SELECT 1 FROM memberships m JOIN entities e ON e.id=m.entity WHERE m.uuid=a.uuid AND e.work_id IN ({}))",vec!["?";ids.len()].join(",")));
        args.extend(ids);
    }
    if !entity.is_empty() {
        for _ in 0..16 {
            let next: Option<Option<String>> =
                c.query_row("SELECT merged_into FROM entities WHERE id=?", [&entity], |r| r.get(0)).optional()?;
            match next {
                Some(Some(n)) if !n.is_empty() => entity = n,
                _ => break,
            }
        }
        let parent: Option<Option<String>> =
            c.query_row("SELECT parent_id FROM entities WHERE id=?", [&entity], |r| r.get(0)).optional()?;
        match parent {
            None => return Err(Error::Invalid("角色不存在".into())),
            Some(Some(p)) if !p.is_empty() => {
                variant = entity;
                entity = p
            }
            _ => {}
        }
        if v("status") == "maybe" {
            clauses.push(format!("EXISTS(SELECT 1 FROM {HINTS} v WHERE v.uuid=a.uuid AND v.entity=?) AND NOT EXISTS(SELECT 1 FROM memberships m WHERE m.uuid=a.uuid AND m.entity=?) AND NOT EXISTS(SELECT 1 FROM assertions s WHERE s.uuid=a.uuid AND s.entity=? AND s.status='rejected')"));
            args.extend(vec![entity.clone(); 3]);
        } else {
            let mascot = exists(c, "SELECT 1 FROM entities WHERE id=? AND mascot<>0", &[&entity])?;
            clauses.push(format!(
                "EXISTS(SELECT 1 FROM {} m WHERE m.uuid=a.uuid AND m.entity=?)",
                if mascot { "tag_memberships" } else { "memberships" }
            ));
            args.push(entity.clone());
        }
    }
    if !variant.is_empty() {
        let p: Option<Option<String>> =
            c.query_row("SELECT parent_id FROM entities WHERE id=?", [&variant], |r| r.get(0)).optional()?;
        if !p.flatten().is_some_and(|p| !p.is_empty() && (entity.is_empty() || p == entity)) {
            return Err(Error::Invalid("子标签不属于该角色".into()));
        }
        clauses.push("EXISTS(SELECT 1 FROM variant_memberships v WHERE v.uuid=a.uuid AND v.entity=?)".into());
        args.push(variant);
    }
    let pool = v("pool");
    match pool{
 "unmatched"=>clauses.extend(["NOT EXISTS(SELECT 1 FROM memberships m WHERE m.uuid=a.uuid)","NOT EXISTS(SELECT 1 FROM tag_memberships t WHERE t.uuid=a.uuid)","NOT EXISTS(SELECT 1 FROM deferred d WHERE d.uuid=a.uuid)"].map(String::from)),
 "skipped"=>clauses.push("EXISTS(SELECT 1 FROM deferred d WHERE d.uuid=a.uuid)".into()),"duplicates"=>clauses.push("EXISTS(SELECT 1 FROM duplicates d WHERE d.uuid=a.uuid)".into()),"excluded"=>clauses.push("EXISTS(SELECT 1 FROM excluded x WHERE x.uuid=a.uuid)".into()),
 "cluster"|"similar"=>{let cluster=v("cluster");if cluster.is_empty()||!cluster.chars().all(|c|crate::compat_unicode::decimal(c).is_some()||digit_extra(c)){return Err(Error::Invalid(if pool=="cluster"{"请指定分组"}else{"请指定相似组"}.into()))}let n=python_int(cluster).map_err(Error::Invalid)?;numeric_index=Some(args.len());if pool=="cluster"{clauses.extend(["EXISTS(SELECT 1 FROM clusters k WHERE k.uuid=a.uuid AND k.cid=?)","NOT EXISTS(SELECT 1 FROM memberships m WHERE m.uuid=a.uuid)","NOT EXISTS(SELECT 1 FROM deferred d WHERE d.uuid=a.uuid)"].map(String::from));}else{clauses.push("EXISTS(SELECT 1 FROM similar_groups g WHERE g.uuid=a.uuid AND g.gid=?)".into())}args.push(n.to_string());},
 "recent"=>clauses.push("EXISTS(SELECT 1 FROM sync_added s WHERE s.uuid=a.uuid AND s.run_id=(SELECT max(run_id) FROM sync_added))".into()),"audit"=>clauses.push("EXISTS(SELECT 1 FROM audit_sample u WHERE u.uuid=a.uuid)".into()),"pending"=>clauses.push("EXISTS(SELECT 1 FROM auto_rules r WHERE r.uuid=a.uuid AND r.kind='pending') AND NOT EXISTS(SELECT 1 FROM curation k WHERE k.uuid=a.uuid)".into()),""=>{},_=>return Err(Error::Invalid("未知图片池".into()))}
    clauses.push(match pool {
        "duplicates" => "NOT EXISTS(SELECT 1 FROM excluded x WHERE x.uuid=a.uuid)".into(),
        "excluded" => unfolded("a"),
        _ => visible("a"),
    });
    if !v("reason").is_empty() {
        if pool != "excluded" || !["photo", "screenshot", "text", "manual"].contains(&v("reason")) {
            return Err(Error::Invalid("未知排除原因".into()));
        }
        clauses.push("EXISTS(SELECT 1 FROM excluded x WHERE x.uuid=a.uuid AND x.reason=?)".into());
        args.push(v("reason").into());
    }
    let dup = v("dup");
    if dup == "mixed" {
        clauses.push(format!("EXISTS(SELECT 1 FROM duplicates d WHERE d.uuid=a.uuid AND {MIXED})"));
    } else if !dup.is_empty() {
        return Err(Error::Invalid("未知重复筛选".into()));
    }
    let status = f.get("status").map(String::as_str).unwrap_or("all");
    match status {
        "maybe" if entity.is_empty() => return Err(Error::Invalid("「可能也是」需要指定角色".into())),
        "maybe" | "all" => {}
        "confirmed" | "machine" => {
            clauses.push(format!(
                "EXISTS(SELECT 1 FROM memberships m WHERE m.uuid=a.uuid AND m.status=?{})",
                if entity.is_empty() { "" } else { " AND m.entity=?" }
            ));
            args.push(status.into());
            if !entity.is_empty() {
                args.push(entity.clone())
            }
        }
        "ambiguous" => {
            clauses.push(format!(
                "EXISTS(SELECT 1 FROM ambiguous_claims x WHERE x.uuid=a.uuid{})",
                if entity.is_empty() { "" } else { " AND x.entity=?" }
            ));
            if !entity.is_empty() {
                args.push(entity.clone())
            }
        }
        "unknown" => clauses.push("EXISTS(SELECT 1 FROM deferred d WHERE d.uuid=a.uuid)".into()),
        _ => return Err(Error::Invalid("未知筛选状态".into())),
    }
    let query = trim(v("q")).chars().take(160).collect::<String>();
    if !query.is_empty() {
        let hits = crate::names_search::hits(c, &query)?;
        let mut search = "a.filename LIKE ? ESCAPE '\\'".to_owned();
        args.push(format!("%{}%", query.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")));
        if !hits.is_empty() {
            search += &format!(
                " OR a.uuid IN (SELECT uuid FROM memberships WHERE entity IN ({}))",
                vec!["?"; hits.len()].join(",")
            );
            args.extend(hits.into_keys());
        }
        clauses.push(format!("({search})"));
    }
    let tag = v("tag");
    let nsfw = v("nsfw");
    let combined = !tag.is_empty() && nsfw == "only" && flag(c, "terms_labels")?;
    if !tag.is_empty() {
        if !crate::http_read::tag_vocabulary(context)?.iter().any(|t| t == tag) {
            return Err(Error::Invalid("请选择已有的通用标签".into()));
        }
        clauses.push(if combined{"a.uuid IN (SELECT ta.key FROM term_projection rated JOIN term_assets ta ON ta.id=rated.asset_id AND ta.namespace='library' WHERE rated.state IN ('machine','confirmed') AND rated.term_id IN (SELECT t.id FROM terms t JOIN term_links l ON l.term_id=t.id AND l.system='wd_rating' WHERE t.kind='rating' AND l.key IN ('questionable', 'explicit')) AND EXISTS(SELECT 1 FROM term_projection tagged WHERE tagged.asset_id=rated.asset_id AND tagged.state IN ('machine','confirmed') AND tagged.term_id IN (SELECT t.id FROM terms t JOIN term_links l ON l.term_id=t.id AND l.system='wd' WHERE t.kind='topic' AND l.key=?)))"}else{"a.uuid IN (SELECT uuid FROM general_hits WHERE value=?)"}.into());
        args.push(tag.into());
    }
    if ["hide", "only"].contains(&nsfw) && !combined {
        let hit = "a.uuid IN (SELECT uuid FROM asset_rating WHERE rating IN ('questionable', 'explicit'))";
        clauses.push(if nsfw == "hide" { format!("NOT coalesce({hit},0)") } else { hit.into() });
    } else if !nsfw.is_empty() && !["hide", "only"].contains(&nsfw) {
        return Err(Error::Invalid("未知内容筛选".into()));
    }
    if !v("utag").is_empty() {
        clauses.push("a.uuid IN (SELECT uuid FROM asset_tags WHERE tag=?)".into());
        args.push(v("utag").into());
    }
    let order = f.get("order").map(String::as_str).unwrap_or("newest");
    if !["newest", "oldest", "suspicious", "hinted"].contains(&order) {
        return Err(Error::Invalid("未知排序".into()));
    }
    let mut ordering = format!("a.added_at {},a.uuid", if order == "oldest" { "ASC" } else { "DESC" });
    if (pool == "duplicates" || !dup.is_empty()) && order != "suspicious" {
        ordering=format!("(SELECT d.grp_added FROM duplicates d WHERE d.uuid=a.uuid) {},(SELECT d.grp FROM duplicates d WHERE d.uuid=a.uuid),(SELECT d.pos FROM duplicates d WHERE d.uuid=a.uuid),a.uuid",if order=="oldest"{"ASC"}else{"DESC"});
    }
    if status == "maybe" && order == "suspicious" {
        ordering = format!(
            "(SELECT min(v.weak)*10-max(v.score) FROM {HINTS} v WHERE v.uuid=a.uuid AND v.entity='{}'),a.uuid",
            entity.replace('\'', "''")
        );
    } else if order == "hinted" {
        ordering = format!(
            "coalesce((SELECT min(v.weak)*10-max(v.score) FROM {} v WHERE v.uuid=a.uuid),99),a.added_at DESC,a.uuid",
            leads()
        );
    } else if order == "suspicious" {
        ordering = format!(
            "(SELECT min(m.skey) FROM memberships m WHERE m.uuid=a.uuid{}),a.uuid",
            if entity.is_empty() { String::new() } else { format!(" AND m.entity='{}'", entity.replace('\'', "''")) }
        );
    }
    if numeric_index.is_some() {
        let digits = v("cluster")
            .chars()
            .map(|c| (b'0' + crate::compat_unicode::decimal(c).expect("http_browse internal invariant")) as char)
            .collect::<String>();
        let digits = digits.trim_start_matches('0');
        if digits.len() > 19 || (digits.len() == 19 && digits > "9223372036854775807") {
            // All filter validation precedes SQLite's Python integer binding failure.
            return Err(Error::Io(std::io::Error::other("Python int too large to convert to SQLite INTEGER")));
        }
    }
    let args = args
        .into_iter()
        .enumerate()
        .map(|(i, v)| {
            if Some(i) == numeric_index {
                Value::Integer(v.parse().expect("http_browse internal invariant"))
            } else {
                Value::Text(v)
            }
        })
        .collect();
    Ok((clauses.join(" AND "), args, ordering))
}
fn evidence(v: &Json, fields: &[&str]) -> Json {
    let mut sources =
        get(v, "sources").text().split(',').filter(|s| !s.is_empty()).map(String::from).collect::<Vec<_>>();
    sources.sort();
    let mut e = Json::object([
        ("sources", Json::Array(sources.into_iter().map(Into::into).collect())),
        ("max_score", get(v, "score")),
    ]);
    for k in fields {
        e.set(*k, get(v, k));
    }
    e
}
fn shown(c: &Connection, v: Json, skin: bool) -> Result<Json> {
    crate::http_read::display_skin(c, v, skin)
}
fn memberships(c: &Connection, id: &str) -> Result<Vec<Json>> {
    let mut out = Vec::new();
    for r in rows(c,"SELECT m.*,e.id,entity_name(e.id) AS name,e.work FROM memberships m JOIN entities e ON e.id=m.entity WHERE m.uuid=? ORDER BY m.status='machine',name,e.id",&[id])?{let entity=get(&r,"id");let mut item=display(c,r.clone())?;item.set("evidence",evidence(&r,&[]));item.set("variants",Json::Array(rows(c,"SELECT entity AS id,variant AS name,status FROM variant_memberships WHERE uuid=? AND parent_id=? ORDER BY variant",&[id,entity.text()])?.into_iter().map(|v|shown(c,v,true)).collect::<Result<_>>()?));out.push(item);}
    Ok(out)
}
// A projected page needs only its identities, work names and hybrid nodes. Keep
// the display overwrite order, but do not prepare several SQL queries per card.
struct PageDisplay {
    entities: HashMap<String, Json>,
    names: HashMap<(String, bool), Json>,
    nodes: HashMap<String, Json>,
}
impl PageDisplay {
    fn load(c: &Connection, ids: Vec<Json>) -> Result<Option<Self>> {
        if !flag(c, "terms_names")? {
            return Ok(None);
        }
        let mut name_ids = ids.clone();
        let ids = Json::Array(ids).dump();
        let mut entities = HashMap::new();
        for r in rows(c, "SELECT e.id,e.kind,e.original,coalesce(p.work_id,e.work_id) AS work_id FROM entities e LEFT JOIN entities p ON p.id=e.parent_id AND e.kind='skin' WHERE e.id IN (SELECT value FROM json_each(?))", &[&ids])? {
            entities.insert(get(&r, "id").text().to_owned(), r);
        }
        name_ids.extend(entities.values().map(|r| get(r, "work_id")));
        let name_ids = Json::Array(name_ids).dump();
        let mut names = HashMap::new();
        for r in rows(c, "SELECT entity,skin_only,name,name_source,zh,ja,en,tag FROM term_name_display WHERE entity IN (SELECT value FROM json_each(?))", &[&name_ids])? {
            names.insert((get(&r, "entity").text().to_owned(), get(&r, "skin_only").integer() != 0), crate::http_read::shown_info(Json::object([
                ("name", get(&r, "name")), ("name_source", get(&r, "name_source")),
                ("names", Json::object([("zh", get(&r, "zh")), ("ja", get(&r, "ja")), ("en", get(&r, "en"))])),
                ("tag", get(&r, "tag")),
            ])));
        }
        let mut nodes = HashMap::new();
        if c.query_row("SELECT value FROM meta WHERE key='identity_policy'", [], |r| r.get::<_, String>(0))
            .optional()?
            .as_deref()
            == Some("hybrid")
        {
            for mut r in rows(c, "SELECT id,node_type,character_id,variant_id,canonical_id FROM identity_nodes WHERE id IN (SELECT value FROM json_each(?))", &[&ids])? {
                let id = get(&r, "id").text().to_owned();
                if let Json::Object(fields) = &mut r { fields.retain(|(k, _)| k != "id"); }
                nodes.insert(id, r);
            }
        }
        Ok(Some(Self { entities, names, nodes }))
    }
    fn display(&self, mut v: Json, skin: bool) -> Json {
        let id = get(&v, "id").text().to_owned();
        if let Some(info) = self.names.get(&(id.clone(), skin)) {
            v.extend(info.clone());
        }
        let mut work = null();
        if let Some(entity) = self.entities.get(&id) {
            if v.get("kind").is_some() && get(entity, "kind").text() != "work" {
                v.set("kind", if get(entity, "original").integer() != 0 { "oc" } else { "" }.into());
            }
            work = get(entity, "work_id");
            v.set("work_id", work.clone());
        }
        if v.get("work").is_some() {
            let name = match &work {
                Json::Scalar(serde_json::Value::String(id)) => {
                    self.names.get(&(id.clone(), false)).map(|r| get(r, "name"))
                }
                _ => None,
            }
            .unwrap_or_else(|| "".into());
            v.set("work_name", name);
        }
        if let Some(node) = self.nodes.get(&id) {
            v.extend(node.clone());
        }
        v
    }
}
/// One batched page, retaining first best lead and Python dict overwrite semantics.
fn decorate(c: &Connection, page: &mut [Json]) -> Result<()> {
    if page.is_empty() {
        return Ok(());
    }
    let ids = Json::Array(page.iter().map(|v| get(v, "uuid")).collect()).dump();
    let selected = "IN (SELECT value FROM json_each(?))";
    let mut leads_by = HashMap::new();
    for r in rows(c,&format!("SELECT v.uuid,v.entity FROM {} v WHERE v.uuid {selected} AND NOT EXISTS(SELECT 1 FROM memberships m WHERE m.uuid=v.uuid) ORDER BY v.weak,v.score DESC",leads()),&[&ids])?{leads_by.entry(get(&r,"uuid").text().to_owned()).or_insert(get(&r,"entity"));}
    let mut audit = HashMap::new();
    for r in rows(c, &format!("SELECT uuid,entity FROM audit_sample WHERE uuid {selected}"), &[&ids])? {
        audit.insert(get(&r, "uuid").text().to_owned(), get(&r, "entity"));
    }
    let mut answered = HashMap::new();
    for r in rows(c,&format!("SELECT u.uuid,coalesce((SELECT k.answer FROM asked k WHERE k.uuid=u.uuid AND k.entity=u.entity AND k.answer<>'later'),(SELECT CASE s.status WHEN 'confirmed' THEN 'yes' ELSE 'no' END FROM assertions s JOIN entities e ON e.id=s.entity WHERE s.uuid=u.uuid AND coalesce(e.parent_id,e.id)=u.entity ORDER BY s.status='confirmed' LIMIT 1),(SELECT 'no' FROM assertions s WHERE s.uuid=u.uuid AND s.status='confirmed' AND u.solo=1 LIMIT 1)) AS answer FROM audit_sample u WHERE u.uuid {selected}"),&[&ids])?{answered.insert(get(&r,"uuid").text().to_owned(),get(&r,"answer"));}
    let variant_rows = rows(c,&format!("SELECT uuid,parent_id,entity AS id,variant AS name,status FROM variant_memberships WHERE uuid {selected} ORDER BY variant"),&[&ids])?;
    let member_rows = rows(c,&format!("SELECT m.*,e.id,entity_name(e.id) AS name,e.work FROM memberships m JOIN entities e ON e.id=m.entity WHERE m.uuid {selected} ORDER BY m.status='machine',name,e.id"),&[&ids])?;
    let mut entity_ids = variant_rows.iter().chain(&member_rows).map(|r| get(r, "id")).collect::<Vec<_>>();
    entity_ids.extend(leads_by.values().chain(audit.values()).cloned());
    let reader = PageDisplay::load(c, entity_ids)?;
    let display = |v: Json, skin| -> Result<Json> {
        match &reader {
            Some(reader) => Ok(reader.display(v, skin)),
            None => shown(c, v, skin),
        }
    };
    let mut variants: HashMap<(String, String), Vec<Json>> = HashMap::new();
    for r in variant_rows {
        let item = display(
            Json::object([("id", get(&r, "id")), ("name", get(&r, "name")), ("status", get(&r, "status"))]),
            true,
        )?;
        variants.entry((get(&r, "uuid").text().into(), get(&r, "parent_id").text().into())).or_default().push(item);
    }
    let mut members: HashMap<String, Vec<Json>> = HashMap::new();
    for r in member_rows {
        let mut item = display(r.clone(), false)?;
        item.set("evidence", evidence(&r, &[]));
        item.set(
            "variants",
            Json::Array(
                variants
                    .get(&(get(&r, "uuid").text().into(), get(&r, "id").text().into()))
                    .cloned()
                    .unwrap_or_default(),
            ),
        );
        members.entry(get(&r, "uuid").text().into()).or_default().push(item);
    }
    let mut tags: HashMap<String, Vec<Json>> = HashMap::new();
    for r in rows(c, &format!("SELECT uuid,tag FROM asset_tags WHERE uuid {selected} ORDER BY tag"), &[&ids])? {
        tags.entry(get(&r, "uuid").text().into()).or_default().push(get(&r, "tag"));
    }
    let deferred: HashSet<_> =
        strings(c, &format!("SELECT uuid FROM deferred WHERE uuid {selected}"), &[&ids])?.into_iter().collect();
    let mut duplicates = HashMap::new();
    for r in rows(c, &format!("SELECT uuid,grp,size,pos FROM duplicates WHERE uuid {selected}"), &[&ids])? {
        duplicates.insert(
            get(&r, "uuid").text().to_owned(),
            Json::object([("grp", get(&r, "grp")), ("size", get(&r, "size")), ("pos", get(&r, "pos"))]),
        );
    }
    let mut excluded = HashMap::new();
    for r in rows(c, &format!("SELECT uuid,reason FROM excluded WHERE uuid {selected}"), &[&ids])? {
        excluded.insert(get(&r, "uuid").text().to_owned(), get(&r, "reason"));
    }
    for row in page {
        let id = get(row, "uuid").text().to_owned();
        let m = members.get(&id).cloned().unwrap_or_default();
        row.set("memberships", Json::Array(m.clone()));
        row.set("confirmed", Json::Array(m.into_iter().filter(|m| get(m, "status").text() == "confirmed").collect()));
        row.set("unknown", deferred.contains(&id).into());
        row.set("user_tags", Json::Array(tags.get(&id).cloned().unwrap_or_default()));
        row.set("dup", duplicates.get(&id).cloned().unwrap_or_else(null));
        row.set("excluded", excluded.get(&id).cloned().unwrap_or_else(null));
        for (key, map) in [("hint", &leads_by), ("audit", &audit)] {
            let mut info = null();
            if let Some(e) = map.get(&id) {
                let shown = display(Json::object([("id", e.clone())]), false)?;
                info = Json::object([("id", get(&shown, "id")), ("name", get(&shown, "name"))]);
                if key == "audit" {
                    info.set("answer", answered.get(&id).cloned().unwrap_or_else(null));
                }
            }
            row.set(key, info);
        }
    }
    Ok(())
}
pub fn validate(path: &str, q: &HashMap<String, String>) -> Result<()> {
    preflight(path, q)?;
    if path == "/api/ids" {
        let offset = python_int(q.get("offset").map(String::as_str).unwrap_or("0")).map_err(Error::Invalid)?;
        let limit = python_int(q.get("limit").map(String::as_str).unwrap_or("100")).map_err(Error::Invalid)?;
        if !(0..=10000000).contains(&offset) || !(1..=5000).contains(&limit) {
            return Err(Error::Invalid("分页参数不正确".into()));
        }
    }
    if path == "/api/home" {
        if !value(q, "before").is_empty() {
            day_bounds(value(q, "before"))?;
        }
        let n = python_int(q.get("days").map(String::as_str).unwrap_or("5")).map_err(Error::Invalid)?;
        if !(1..=20).contains(&n) {
            return Err(Error::Invalid("分页参数不正确".into()));
        }
        rating("a", value(q, "nsfw"))?;
    }
    Ok(())
}
fn question(c: &Connection, id: &str) -> Result<Json> {
    if exists(c, "SELECT 1 FROM excluded WHERE uuid=?", &[id])? {
        return Ok(null());
    }
    let audit = strings(
        c,
        &format!(
            "SELECT u.entity FROM audit_sample u JOIN assets a ON a.uuid=u.uuid WHERE u.uuid=? AND {}",
            audit_open("a")
        ),
        &[id],
    )?
    .into_iter()
    .next();
    let unasked = |a: &str| format!("NOT EXISTS(SELECT 1 FROM asked k WHERE k.uuid={a}.uuid AND k.entity={a}.entity)");
    let learnable = |a: &str| format!("NOT EXISTS(SELECT 1 FROM entities l WHERE l.id={a}.entity AND l.label_only=1)");
    let mut ambiguous = None;
    let mut hint = None;
    if audit.is_none() {
        ambiguous=rows(c,&format!("SELECT m.entity,m.rival FROM ambiguous_claims m WHERE m.uuid=? AND {} AND {} ORDER BY m.margin LIMIT 1",unasked("m"),learnable("m")),&[id])?.into_iter().next();
        hint=strings(c,&format!("SELECT w.entity FROM weak_claims w JOIN single_assets o ON o.uuid=w.uuid WHERE w.source='PROTO' AND {} AND {} AND NOT EXISTS(SELECT 1 FROM memberships m WHERE m.uuid=w.uuid AND m.entity=w.entity) AND NOT EXISTS(SELECT 1 FROM assertions s WHERE s.uuid=w.uuid AND (s.status='confirmed' OR s.entity=w.entity)) AND NOT EXISTS(SELECT 1 FROM excluded x WHERE x.uuid=w.uuid) AND w.uuid=? ORDER BY w.score DESC LIMIT 1",unasked("w"),learnable("w")),&[id])?.into_iter().next();
    }
    let confirmed = |e: &str| count(c, "SELECT count(*) FROM memberships WHERE entity=? AND status='confirmed'", &[e]);
    let (kind, entity, rival) = if let Some(e) = audit {
        ("audit", e, None)
    } else if let Some(e) = hint {
        ("hint", e, None)
    } else if let Some(r) = ambiguous {
        ("ambiguous", get(&r, "entity").text().to_owned(), Some(get(&r, "rival").text().to_owned()))
    } else {
        let mut found = None;
        for e in strings(c,&format!("SELECT m.entity FROM memberships m JOIN single_assets o ON o.uuid=m.uuid WHERE m.uuid=? AND m.status='machine' AND {} AND {}",unasked("m"),learnable("m")),&[id])?{if confirmed(&e)?<5{found=Some(e);break;}}
        let Some(e) = found else { return Ok(null()) };
        ("seed", e, None)
    };
    let name = |e: &str| -> Result<Json> {
        Ok(c.query_row("SELECT entity_name(e.id) FROM entities e WHERE id=?", [e], |r| r.get::<_, String>(0))?.into())
    };
    let mut out = Json::object([
        ("kind", kind.into()),
        ("entity", entity.clone().into()),
        ("name", name(&entity)?),
        ("confirmed", confirmed(&entity)?.into()),
    ]);
    if let Some(rival) = rival.filter(|r| !r.is_empty()) {
        out.set("rival", rival.clone().into());
        out.set("rival_name", name(&rival)?);
    }
    if kind == "hint" {
        if let Some(current) =
            strings(c, "SELECT entity FROM memberships WHERE uuid=? AND entity<>? LIMIT 1", &[id, &entity])?
                .into_iter()
                .next()
        {
            out.set("current", current.clone().into());
            out.set("current_name", name(&current)?);
        }
    }
    Ok(out)
}
fn detail(c: &Connection, context: &LibraryContext, id: &str) -> Result<Json> {
    let Some(mut out) = rows(c, "SELECT uuid,filename,width,height,added_at,src_kind FROM assets WHERE uuid=?", &[id])?
        .into_iter()
        .next()
    else {
        return Err(Error::Invalid("图片不存在".into()));
    };
    let members = memberships(c, id)?;
    out.set("memberships", Json::Array(members.clone()));
    out.set("tag_identities",Json::Array(rows(c,"SELECT e.id,entity_name(e.id) AS name,t.status FROM tag_memberships t JOIN entities e ON e.id=t.entity WHERE t.uuid=? ORDER BY name",&[id])?));
    let mut ownership = members;
    for r in rows(c,"SELECT e.id,entity_name(e.id) AS name,s.status,s.origin,v.sources,v.score FROM assertions s JOIN entities e ON e.id=s.entity LEFT JOIN family_evidence v ON v.entity=e.id AND v.uuid=s.uuid WHERE s.uuid=? AND s.status='rejected' AND e.parent_id IS NULL",&[id])?{let mut item=display(c,r.clone())?;item.set("evidence",evidence(&r,&[]));ownership.push(item);}
    out.set("ownership", Json::Array(ownership));
    let sql="SELECT * FROM (SELECT e.id,entity_name(e.id) AS name,e.work,v.sources,v.score,v.wd,v.ccip,v.rank,0 AS weak FROM family_evidence v JOIN entities e ON e.id=v.entity WHERE v.uuid=?1 UNION ALL SELECT e.id,entity_name(e.id),e.work,group_concat(w.source),max(w.score),NULL,max(w.score),1000,1 FROM weak_claims w JOIN entities e ON e.id=w.entity WHERE w.uuid=?1 AND NOT EXISTS(SELECT 1 FROM family_evidence f WHERE f.uuid=w.uuid AND f.entity=w.entity) GROUP BY e.id) x WHERE NOT EXISTS(SELECT 1 FROM memberships m WHERE m.uuid=?1 AND m.entity=x.id) AND NOT EXISTS(SELECT 1 FROM assertions s WHERE s.uuid=?1 AND s.entity=x.id AND s.status='rejected') ORDER BY weak,rank,score DESC LIMIT 12";
    let mut alternatives = Vec::new();
    for r in rows(c, sql, &[id])? {
        let mut item =
            display(c, Json::object([("id", get(&r, "id")), ("name", get(&r, "name")), ("work", get(&r, "work"))]))?;
        item.set("weak", (get(&r, "weak").integer() != 0).into());
        item.set("evidence", evidence(&r, &["wd", "ccip"]));
        alternatives.push(item);
    }
    out.set("alternatives", Json::Array(alternatives));
    out.set("single", exists(c, "SELECT 1 FROM single_assets WHERE uuid=?", &[id])?.into());
    out.set("assertions",Json::Array(rows(c,"SELECT e.id,entity_name(e.id) AS name,s.status,s.origin FROM assertions s JOIN entities e ON e.id=s.entity WHERE s.uuid=? ORDER BY name,e.id",&[id])?.into_iter().map(|r|display(c,r)).collect::<Result<_>>()?));
    out.set("candidates",Json::Array(rows(c,"SELECT e.id,entity_name(e.id) AS name,c.source,c.score FROM candidates c JOIN entities e ON e.id=c.entity WHERE c.uuid=? ORDER BY name,e.id",&[id])?.into_iter().map(|r|display(c,r)).collect::<Result<_>>()?));
    out.set("unknown", exists(c, "SELECT 1 FROM deferred WHERE uuid=?", &[id])?.into());
    out.set(
        "user_tags",
        Json::Array(
            strings(c, "SELECT tag FROM asset_tags WHERE uuid=? ORDER BY tag", &[id])?
                .into_iter()
                .map(Into::into)
                .collect(),
        ),
    );
    out.set(
        "excluded",
        rows(c, "SELECT reason,origin FROM excluded WHERE uuid=?", &[id])?.into_iter().next().unwrap_or_else(null),
    );
    let curated = rows(c, "SELECT status FROM curation WHERE uuid=?", &[id])?.into_iter().next();
    out.set("curation", curated.as_ref().map(|v| get(v, "status")).unwrap_or_else(null));
    out.set(
        "pending",
        (curated.is_none() && exists(c, "SELECT 1 FROM auto_rules WHERE uuid=? AND kind='pending'", &[id])?).into(),
    );
    let mut copies=rows(c,"SELECT y.uuid,y.pos,y.size,a.width,a.height,a.added_at FROM duplicates d JOIN duplicates y ON y.grp=d.grp AND y.uuid<>d.uuid JOIN assets a ON a.uuid=y.uuid WHERE d.uuid=? ORDER BY y.pos",&[id])?;
    for copy in &mut copies {
        let uuid = get(copy, "uuid");
        copy.set("names",Json::Array(strings(c,"SELECT entity_name(e.id) FROM memberships m JOIN entities e ON e.id=m.entity WHERE m.uuid=? ORDER BY m.status='machine',1",&[uuid.text()])?.into_iter().map(Into::into).collect()));
    }
    out.set("duplicates", Json::Array(copies));
    c.execute(
        "ATTACH DATABASE ? AS src",
        [crate::http_read::sqlite_uri(
            context.source.as_ref().ok_or_else(|| Error::Invalid("HTTP 图库状态缺少 source".into()))?,
        )],
    )?;
    out.set("tags",Json::Array(rows(c,"SELECT value,conf FROM src.labels WHERE uuid=? AND dim='general' AND conf>=0.35 ORDER BY conf DESC LIMIT 24",&[id])?));
    out.set("identity", crate::http_characters::assignments(c, id)?);
    out.set("question", question(c, id)?);
    attach_provenance(c, context)?;
    out.extend(crate::http_artists::for_asset(c, context.library.is_some(), id, None, false)?);
    Ok(out)
}
fn pools(c: &Connection, nsfw: &str) -> Result<Json> {
    let r = rating("a", nsfw)?;
    let seen = format!("{}{r}", visible("a"));
    let row=rows(c,&format!("SELECT count(*) AS n,min(a.uuid) AS cover FROM assets a WHERE {seen} AND NOT EXISTS(SELECT 1 FROM memberships m WHERE m.uuid=a.uuid) AND NOT EXISTS(SELECT 1 FROM tag_memberships t WHERE t.uuid=a.uuid) AND NOT EXISTS(SELECT 1 FROM deferred d WHERE d.uuid=a.uuid)"),&[])?.remove(0);
    let skipped = count(c, &format!("SELECT count(*) FROM deferred a JOIN assets USING(uuid) WHERE {seen}"), &[])?;
    let listed = format!("NOT EXISTS(SELECT 1 FROM excluded x WHERE x.uuid=d.uuid){}", rating("d", nsfw)?);
    let dups =
        rows(c, &format!("SELECT count(*) AS n,count(DISTINCT grp) AS groups FROM duplicates d WHERE {listed}"), &[])?
            .remove(0);
    let mixed = count(c, &format!("SELECT count(DISTINCT d.grp) FROM duplicates d WHERE {MIXED} AND {listed}"), &[])?;
    let mut reasons = Json::Object(Vec::new());
    let mut excluded = 0;
    for x in rows(
        c,
        &format!("SELECT reason,count(*) AS n FROM excluded a WHERE {}{r} GROUP BY reason", unfolded("a")),
        &[],
    )? {
        excluded += get(&x, "n").integer();
        reasons.set(get(&x, "reason").text(), get(&x, "n"));
    }
    let recent=count(c,&format!("SELECT count(*) FROM sync_added a WHERE run_id=(SELECT max(run_id) FROM sync_added) AND {seen} AND EXISTS(SELECT 1 FROM assets x WHERE x.uuid=a.uuid)"),&[])?;
    let pending=count(c,&format!("SELECT count(*) FROM auto_rules a WHERE kind='pending' AND {seen} AND NOT EXISTS(SELECT 1 FROM curation k WHERE k.uuid=a.uuid)"),&[])?;
    let audit = count(c, &format!("SELECT count(*) FROM assets a WHERE {} AND {seen}", audit_open("a")), &[])?;
    let basic = |kind: &str, name: &str, n: Json, cover: Json| {
        Json::object([("kind", kind.into()), ("name", name.into()), ("count", n), ("cover", cover)])
    };
    Ok(Json::Array(vec![
        basic("unmatched", "未识别", get(&row, "n"), get(&row, "cover")),
        basic("skipped", "已跳过", skipped.into(), null()),
        Json::object([
            ("kind", "duplicates".into()),
            ("name", "重复".into()),
            ("count", get(&dups, "n")),
            ("groups", get(&dups, "groups")),
            ("mixed", mixed.into()),
            ("cover", null()),
        ]),
        Json::object([
            ("kind", "excluded".into()),
            ("name", "已排除".into()),
            ("count", excluded.into()),
            ("reasons", reasons),
            ("cover", null()),
        ]),
        basic("pending", "待确认", pending.into(), null()),
        basic("recent", "新加入", recent.into(), null()),
        basic("audit", "随机抽查", audit.into(), null()),
    ]))
}
fn home(c: &Connection, q: &HashMap<String, String>) -> Result<Json> {
    let before = value(q, "before");
    let days = python_int(q.get("days").map(String::as_str).unwrap_or("5")).map_err(Error::Invalid)?;
    let seen = format!("{}{}", visible("a"), rating("a", value(q, "nsfw"))?);
    let mut args = Vec::new();
    if !before.is_empty() {
        args.push(Value::Text(before.to_owned()));
    }
    args.push(Value::Integer(days));
    let groups=typed_rows(c,&format!("SELECT substr(a.added_at,1,10) AS day,count(*) AS n FROM assets a WHERE {seen} AND a.added_at IS NOT NULL{} GROUP BY 1 ORDER BY 1 DESC LIMIT ?",if before.is_empty(){""}else{" AND a.added_at<?"}),&args)?;
    let mut result = Vec::new();
    let mut all = Vec::new();
    let mut lengths = Vec::new();
    for g in groups {
        let day = get(&g, "day");
        let (lo, hi) = day_bounds(day.text())?;
        let people=rows(c,&format!("SELECT m.entity,count(*) AS n FROM memberships m JOIN assets a ON a.uuid=m.uuid WHERE a.added_at>=? AND a.added_at<? AND {seen} GROUP BY m.entity ORDER BY n DESC,m.entity"),&[&lo,&hi])?;
        let items=typed_rows(c,&format!("SELECT a.uuid,a.width,a.height FROM assets a WHERE a.added_at>=? AND a.added_at<? AND {seen} ORDER BY a.added_at DESC,a.uuid LIMIT ?"),&[Value::Text(lo.clone()),Value::Text(hi.clone()),Value::Integer(18)])?;
        lengths.push(items.len());
        all.extend(items);
        let mut chars = Vec::new();
        for p in people.iter().take(8) {
            let shown = display(c, Json::object([("id", get(p, "entity"))]))?;
            chars.push(Json::object([
                ("id", get(&shown, "id")),
                ("name", get(&shown, "name")),
                ("tag", get(&shown, "tag")),
                ("count", get(p, "n")),
            ]));
        }
        result.push(Json::object([
            ("day", day),
            ("count", get(&g, "n")),
            ("people", (people.len() as i64).into()),
            ("items", Json::Array(Vec::new())),
            ("characters", Json::Array(chars)),
        ]));
    }
    decorate(c, &mut all)?;
    let mut at = 0;
    for (g, n) in result.iter_mut().zip(lengths) {
        let items = all[at..at + n].to_vec();
        at += n;
        let top = match g.get("characters") {
            Some(Json::Array(v)) => v.first().map(|v| get(v, "id")),
            _ => None,
        };
        let hero = top
            .and_then(|t| {
                items.iter().find(|v| match v.get("memberships") {
                    Some(Json::Array(m)) => m.iter().any(|m| get(m, "id").text() == t.text()),
                    _ => false,
                })
            })
            .or_else(|| items.first())
            .map(|v| get(v, "uuid"))
            .unwrap_or_else(null);
        g.set("items", Json::Array(items));
        g.set("hero", hero);
    }
    let next = if result.len() == days as usize {
        result.last().map(|v| get(v, "day")).unwrap_or_else(|| "".into())
    } else {
        "".into()
    };
    Ok(Json::object([("days", Json::Array(result)), ("next", next)]))
}
/// Smallest share of the library, in permille, left after hiding NSFW for the date-index page (#48).
/// Measured on synthetic libraries (debug host, 10k and 50k assets): counting twice rebuilds the NSFW
/// list from the term projection twice, so the date-index page only beats the filtered copy while
/// NSFW stays a minority; see docs/TERMS_PERFORMANCE.md section 3.
const DATE_PAGE_MIN_KEPT_PERMILLE: i64 = 700;
/// Whether the plain hide-NSFW date page should skip the filtered copy. Decided before the real count
/// from two cheap reads in the same snapshot: all assets, and the NSFW rows of the rating projection
/// (an index range without the joins of asset_rating, stopping once the threshold is exceeded). The
/// estimate ignores folded and excluded rows; it only chooses the path, never the result. Libraries
/// whose labels are not core keep the copy. Deep pages keep it too, since walking the index past half
/// the kept rows costs about another count.
fn date_page_pays(c: &Connection, offset: i64, limit: i64) -> Result<bool> {
    if !flag(c, "terms_labels")? {
        return Ok(false);
    }
    let all = count(c, "SELECT count(*) FROM assets", &[])?;
    let most_nsfw = all - (all * DATE_PAGE_MIN_KEPT_PERMILLE + 999) / 1000;
    let nsfw = count(c, &format!("SELECT count(*) FROM (SELECT 1 FROM term_projection WHERE state IN ('machine','confirmed') AND term_id IN (SELECT l.term_id FROM term_links l JOIN terms t ON t.id=l.term_id AND t.kind='rating' WHERE l.system='wd_rating' AND l.key IN ('questionable','explicit')) LIMIT {})", most_nsfw + 1), &[])?;
    Ok(nsfw <= most_nsfw && (offset + limit) * 2 <= all - nsfw)
}
pub fn read(c: &Connection, context: &LibraryContext, path: &str, q: &HashMap<String, String>) -> Result<(Json, u16)> {
    validate(path, q)?;
    let result = match path {
        "/api/fold" => {
            let kinds = c
                .query_row("SELECT value FROM prefs WHERE key='fold_kinds'", [], |r| r.get::<_, String>(0))
                .optional()?
                .map(|s| crate::http_json::parse_python(&s))
                .transpose()?
                .unwrap_or_else(|| Json::Array(vec!["duplicate".into(), "identical".into()]));
            Json::object([("kinds", kinds)])
        }
        "/api/review-pools" => pools(c, value(q, "nsfw"))?,
        "/api/detail" => detail(c, context, value(q, "id"))?,
        "/api/home" => home(c, q)?,
        "/api/assets" | "/api/assets/count" | "/api/ids" => {
            let mut f = q.clone();
            let offset = if path == "/api/assets/count" {
                0
            } else {
                python_int(q.get("offset").map(String::as_str).unwrap_or("0")).map_err(Error::Invalid)?
            };
            let limit = if path == "/api/assets/count" {
                100
            } else {
                python_int(q.get("limit").map(String::as_str).unwrap_or("100")).map_err(Error::Invalid)?
            };
            if path == "/api/ids" {
                f.remove("offset");
                f.remove("limit");
            } else if path == "/api/assets" {
                f.retain(|k, _| {
                    [
                        "entity", "status", "q", "order", "pool", "tag", "variant", "utag", "nsfw", "dup", "reason",
                        "cluster", "work", "day", "artist",
                    ]
                    .contains(&k.as_str())
                });
            }
            let (where_, mut args, order) = selection(c, context, &f)?;
            let total_sql = format!("SELECT count(*) AS total FROM assets a WHERE {where_}");
            if path == "/api/assets/count" {
                Json::object([("total", get(&typed_rows(c, &total_sql, &args)?.remove(0), "total"))])
            } else if path == "/api/ids" {
                args.extend([Value::Integer(limit), Value::Integer(offset)]);
                Json::Array(
                    typed_rows(
                        c,
                        &format!("SELECT a.uuid FROM assets a WHERE {where_} ORDER BY {order} LIMIT ? OFFSET ?"),
                        &args,
                    )?
                    .iter()
                    .map(|r| get(r, "uuid"))
                    .collect(),
                )
            } else {
                let total_needed = value(q, "count") != "0";
                let nsfw = value(q, "nsfw");
                // A rating-filtered total copies the matches once, then counts and sorts the copy, so the
                // rating subquery and other filters run once. Only the plain library view hiding NSFW in date
                // order (#48) may page assets through assets_date instead, when date_page_pays() estimates
                // the copy and its sort cost more than counting twice. Both read one transaction snapshot
                // and every ordering ends in the primary key, so either path returns the same rows in the
                // same order.
                let date_page = total_needed
                    && nsfw == "hide"
                    && order.starts_with("a.added_at ")
                    && f.iter().all(|(k, v)| k == "nsfw" || k == "order" || v.is_empty())
                    && date_page_pays(c, offset, limit)?;
                let (total, sql) = if total_needed && !nsfw.is_empty() && !date_page {
                    c.execute(&format!("CREATE TEMP TABLE selected_assets AS SELECT a.uuid,a.width,a.height,a.added_at FROM assets a WHERE {where_}"),rusqlite::params_from_iter(&args))?;
                    let n = count(c, "SELECT count(*) FROM selected_assets", &[])?;
                    args.clear();
                    (
                        n.into(),
                        format!(
                            "SELECT a.uuid,a.width,a.height FROM selected_assets a ORDER BY {order} LIMIT ? OFFSET ?"
                        ),
                    )
                } else {
                    let total =
                        if total_needed { get(&typed_rows(c, &total_sql, &args)?.remove(0), "total") } else { null() };
                    (total,format!("SELECT a.uuid,a.width,a.height FROM assets a WHERE {where_} ORDER BY {order} LIMIT ? OFFSET ?"))
                };
                args.extend([Value::Integer(limit), Value::Integer(offset)]);
                let mut items = typed_rows(c, &sql, &args)?;
                decorate(c, &mut items)?;
                Json::object([("items", Json::Array(items)), ("total", total), ("offset", offset.into())])
            }
        }
        _ => return Err(Error::Invalid("未迁移浏览接口".into())),
    };
    Ok((result, 200))
}

// Cached groups are also available without research workers.
pub(crate) fn similar_groups(c: &Connection, kind: &str, nsfw: &str) -> Result<Json> {
    const KINDS: &[&str] = &["identical", "duplicate", "subtle", "crop", "variant", "edit"];
    if !kind.is_empty() && !KINDS.contains(&kind) {
        return Err(Error::Invalid("未知关系".into()));
    }
    let a = format!("{}{}", visible("a"), rating("a", nsfw)?);
    let b = format!("{}{}", visible("b"), rating("b", nsfw)?);
    let mut members: Vec<(Json, Vec<Json>)> = Vec::new();
    let mut group_positions = HashMap::new();
    for r in rows(c,&format!("SELECT g.gid,g.uuid FROM similar_groups g JOIN assets a ON a.uuid=g.uuid WHERE {a} ORDER BY g.gid,a.added_at DESC"),&[])? {
        let gid=get(&r,"gid");
        let pos=*group_positions.entry(gid.integer()).or_insert_with(||{
            let pos=members.len();members.push((gid,Vec::new()));pos
        });
        members[pos].1.push(get(&r,"uuid"));
    }
    let mut kinds: HashMap<i64, Json> = HashMap::new();
    for r in rows(c,&format!("SELECT g.gid,r.kind,count(*) AS n FROM relations r JOIN similar_groups g ON g.uuid=r.a JOIN similar_groups h ON h.uuid=r.b AND h.gid=g.gid JOIN assets a ON a.uuid=r.a JOIN assets b ON b.uuid=r.b WHERE {a} AND {b} GROUP BY g.gid,r.kind"),&[])?{if KINDS.contains(&get(&r,"kind").text()){kinds.entry(get(&r,"gid").integer()).or_insert_with(||Json::Object(Vec::new())).set(get(&r,"kind").text(),get(&r,"n"));}}
    Ok(Json::Array(
        members
            .into_iter()
            .filter_map(|(gid, uuids)| {
                let k = kinds.remove(&gid.integer()).unwrap_or_else(|| Json::Object(Vec::new()));
                if uuids.len() <= 1 || (!kind.is_empty() && k.get(kind).is_none()) {
                    return None;
                }
                Some(Json::object([
                    ("gid", gid),
                    ("size", (uuids.len() as i64).into()),
                    ("covers", Json::Array(uuids.into_iter().take(4).collect())),
                    ("kinds", k),
                ]))
            })
            .collect(),
    ))
}
pub(crate) fn clusters(c: &Connection, nsfw: &str) -> Result<Json> {
    let mut groups=rows(c,&format!("SELECT i.cid,i.near,i.near_dist,i.near_kind,i.traits,i.built_at,count(k.uuid) AS size,group_concat(k.uuid) AS members FROM cluster_info i JOIN clusters k ON k.cid=i.cid JOIN assets a ON a.uuid=k.uuid WHERE {}{} AND NOT EXISTS(SELECT 1 FROM memberships m WHERE m.uuid=k.uuid) AND NOT EXISTS(SELECT 1 FROM deferred d WHERE d.uuid=k.uuid) GROUP BY i.cid HAVING count(k.uuid)>=2 ORDER BY size DESC,i.cid",visible("a"),rating("a",nsfw)?),&[])?;
    let mut members = HashMap::new();
    let mut ordered = Vec::new();
    for r in &groups {
        for u in get(r, "members").text().split(',') {
            if !members.contains_key(u) {
                ordered.push(u.to_owned());
            }
            members.insert(u.to_owned(), get(r, "cid").integer());
        }
    }
    let page = Json::Array(ordered.into_iter().map(Into::into).collect()).dump();
    let mut first = HashSet::new();
    let mut votes: HashMap<i64, Vec<(String, i64)>> = HashMap::new();
    let mut vote_positions = HashMap::new();
    for r in rows(c,&format!("SELECT v.uuid,v.entity FROM {} v WHERE v.uuid IN (SELECT value FROM json_each(?)) ORDER BY v.weak,v.score DESC",leads()),&[&page])? {
        let u=get(&r,"uuid").text().to_owned();
        if first.insert(u.clone()) {
            let entity=get(&r,"entity").text().to_owned();
            let cid=members[&u];let group=votes.entry(cid).or_default();
            let pos=*vote_positions.entry((cid,entity.clone())).or_insert_with(||{
                let pos=group.len();group.push((entity,0));pos
            });
            group[pos].1+=1;
        }
    }
    for r in &mut groups {
        let members = get(r, "members").text().split(',').map(|s| s.into()).collect::<Vec<Json>>();
        r.set("members", Json::Array(members.clone()));
        r.set("covers", Json::Array(members.into_iter().take(4).collect()));
        let traits = get(r, "traits");
        r.set(
            "traits",
            if traits.text().is_empty() {
                Json::Array(Vec::new())
            } else {
                crate::http_json::parse_python(traits.text())?
            },
        );
        let mut lead = null();
        if let Some(v) = votes.get(&get(r, "cid").integer()) {
            let top = v.iter().reduce(|a, b| if b.1 > a.1 { b } else { a });
            if let Some((id, n)) = top.filter(|(_, n)| *n >= 2.max((get(r, "size").integer() + 1) / 2)) {
                let info = display(c, Json::object([("id", id.clone().into())]))?;
                lead = Json::object([("id", id.clone().into()), ("name", get(&info, "name")), ("count", (*n).into())]);
            }
        }
        r.set("lead", lead);
    }
    Ok(Json::Array(groups))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn page_display_matches_single_row_renderer_for_projected_names() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE meta(key TEXT PRIMARY KEY,value TEXT); INSERT INTO meta VALUES ('terms_names','core');
            CREATE TABLE entities(id TEXT PRIMARY KEY,kind TEXT,original INTEGER,work_id TEXT,parent_id TEXT);
            INSERT INTO entities VALUES ('root','character',0,'work',NULL),('skin','skin',0,'unused','root'),('oc','character',1,NULL,NULL),('unshown','character',0,NULL,NULL),('work','work',0,NULL,NULL);
            CREATE TABLE identity_nodes(id TEXT PRIMARY KEY,node_type TEXT,character_id TEXT,variant_id TEXT,canonical_id TEXT);
            INSERT INTO identity_nodes VALUES ('root','character','root',NULL,'root'),('skin','form','root','skin','skin');").unwrap();
        c.execute_batch(crate::names_view::TABLES).unwrap();
        for (id, name) in [("root", "人物🙂"), ("skin", "人物 · 泳装"), ("oc", "OC"), ("work", "作品")] {
            for skin in [0, 1] {
                c.execute(
                    "INSERT INTO term_name_display VALUES (?,?,?,'user','中',NULL,'en','tag')",
                    rusqlite::params![id, skin, if skin == 0 { name } else { "短名" }],
                )
                .unwrap();
            }
        }
        for hybrid in [false, true] {
            c.execute("INSERT OR REPLACE INTO meta VALUES ('identity_policy',?)", [if hybrid { "hybrid" } else { "" }])
                .unwrap();
            crate::http_read::register_names(&c).unwrap();
            let ids = ["root", "skin", "oc", "unshown", "absent"];
            let page = PageDisplay::load(&c, ids.iter().map(|id| (*id).into()).collect()).unwrap().unwrap();
            for id in ids {
                for skin in [false, true] {
                    for work in [false, true] {
                        for kind in [false, true] {
                            let mut row = Json::object([
                                ("id", id.into()),
                                ("name", "旧名".into()),
                                ("status", "confirmed".into()),
                            ]);
                            if work {
                                row.set("work", "旧作品".into());
                            }
                            if kind {
                                row.set("kind", "旧类型".into());
                            }
                            assert_eq!(
                                page.display(row.clone(), skin).dump(),
                                shown(&c, row, skin).unwrap().dump(),
                                "{id} skin={skin} work={work} kind={kind} hybrid={hybrid}"
                            );
                        }
                    }
                }
            }
        }
    }
    #[test]
    fn query_only_groups_match_recorded_python_store_raw_json() {
        let path = std::env::temp_dir().join(format!(
            "illustash-browse-groups-{}-{}.sqlite",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        let mut c = crate::sqlite::open(&path).unwrap();
        c.execute_batch("CREATE TABLE meta(key TEXT,value TEXT);CREATE TABLE entities(id TEXT,kind TEXT,original INTEGER,work_id TEXT,parent_id TEXT);INSERT INTO entities VALUES ('alice','character',0,NULL,NULL),('bob','character',0,NULL,NULL);
            CREATE TABLE assets(uuid TEXT PRIMARY KEY,added_at TEXT);INSERT INTO assets VALUES ('a','2026-09-30'),('b','2026-09-30'),('c','2026-09-29'),('d','2026-09-28'),('e','2026-09-27'),('f','2026-09-26');
            CREATE TABLE duplicates(uuid TEXT,hidden INTEGER);INSERT INTO duplicates VALUES ('e',1);CREATE TABLE excluded(uuid TEXT);INSERT INTO excluded VALUES ('f');CREATE TABLE asset_rating(uuid TEXT,rating TEXT);INSERT INTO asset_rating VALUES ('c','explicit');
            CREATE TABLE memberships(uuid TEXT,entity TEXT);CREATE TABLE deferred(uuid TEXT);CREATE TABLE assertions(uuid TEXT,entity TEXT,status TEXT);
            CREATE TABLE family_evidence(uuid TEXT,entity TEXT,score REAL);INSERT INTO family_evidence VALUES ('a','bob',.92),('b','alice',.92),('c','alice',.91),('d','bob',.9);CREATE TABLE weak_claims(uuid TEXT,entity TEXT,score REAL);
            CREATE TABLE similar_groups(uuid TEXT,gid INTEGER);INSERT INTO similar_groups VALUES ('a',1),('b',1),('c',1),('d',1),('e',1),('f',1);
            CREATE TABLE relations(a TEXT,b TEXT,kind TEXT);INSERT INTO relations VALUES ('a','b','identical'),('b','c','subtle'),('c','d','edit'),('e','f','duplicate');
            CREATE TABLE clusters(uuid TEXT,cid INTEGER);INSERT INTO clusters VALUES ('a',7),('b',7),('c',7),('d',7),('e',7),('f',7);CREATE TABLE cluster_info(cid INTEGER,near TEXT,near_dist REAL,near_kind TEXT,traits TEXT,built_at TEXT);INSERT INTO cluster_info VALUES (7,'near',.125,'character','{\"z\":1,\"a\":2}','2026-09-30');
            CREATE INDEX weak_uuid ON weak_claims(uuid,entity);CREATE INDEX cluster_cid ON clusters(cid,uuid);").unwrap();
        let f = rusqlite::functions::FunctionFlags::SQLITE_UTF8;
        c.create_scalar_function("entity_info", 2, f, |ctx| {
            let id: String = ctx.get(0)?;
            Ok(Json::object([("name", id.into())]).dump())
        })
        .unwrap();
        // The former Python Store's clusters()/similar_groups() bytes on this database, recorded once (#9).
        let expected = include_bytes!("../tests/fixtures/json/browse_groups.json");
        let tx = c.transaction().unwrap();
        let mut actual = Vec::new();
        for nsfw in ["", "hide", "only"] {
            actual.push(clusters(&tx, nsfw).unwrap());
            for kind in ["", "identical", "subtle", "edit"] {
                actual.push(similar_groups(&tx, kind, nsfw).unwrap());
            }
        }
        assert_eq!(Json::Array(actual).dump().as_bytes(), expected.as_slice());
        tx.commit().unwrap();
        drop(c);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn rating_filtered_pages_keep_order_total_and_bounds() {
        // #48: rating-filtered pages keep fixed expectations for ties, NULL dates, folded/excluded rows,
        // the last partial page, past-the-end offsets and empty results on both paths. Without extra
        // rows half the library is NSFW, so every page falls back to the filtered copy; twelve excluded
        // general rows raise the kept share above the threshold, so shallow plain hide pages use
        // assets_date while deep ones still fall back.
        for extra in [0usize, 12] {
            rating_filtered_pages(extra);
        }
    }
    fn rating_filtered_pages(extra: usize) {
        let root = std::env::temp_dir().join(format!(
            "illustash-browse-rating-pages-{}-{extra}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(root.join("input")).unwrap();
        let dest = root.join("synthetic.pilib");
        crate::app_create::create_folder(&dest, &root.join("input"), None, None, None).unwrap();
        let c = crate::sqlite::open(dest.join("library.sqlite")).unwrap();
        let extras = (0..extra).map(|i| format!("z{i:02}")).collect::<Vec<_>>();
        for z in &extras {
            c.execute("INSERT INTO assets(uuid,filename,width,height,added_at) VALUES (?1,?1||'.png',1,1,'2026-09-01 10:00:00')", [z])
                .unwrap();
            c.execute("INSERT INTO excluded VALUES (?,'photo','manual')", [z]).unwrap();
        }
        // Insertion order differs from every expected order, so rowid order cannot pass by accident.
        c.execute_batch("INSERT INTO assets(uuid,filename,width,height,added_at) VALUES
            ('e1','e1.png',1,1,'2026-09-28 10:00:00'),('a3','a3.png',1,1,'2026-09-30 10:00:00'),
            ('c1','c1.png',1,1,NULL),('b2','b2.png',1,1,'2026-09-29 10:00:00'),
            ('a2','a2.png',1,1,'2026-09-30 10:00:00'),('d1','d1.png',1,1,'2026-09-28 10:00:00'),
            ('b1','b1.png',1,1,'2026-09-29 10:00:00'),('a1','a1.png',1,1,'2026-09-30 10:00:00');
            INSERT INTO duplicates VALUES ('d1','g','2',1,'2026-09-28 10:00:00',0,1),('b2','g','2',0,'2026-09-28 10:00:00',1,0);
            INSERT INTO excluded VALUES ('e1','photo','manual');
            INSERT INTO terms(key,kind,origin) VALUES ('synthetic-general','rating','synthetic'),('synthetic-sensitive','rating','synthetic'),
             ('synthetic-questionable','rating','synthetic'),('synthetic-explicit','rating','synthetic');
            INSERT INTO term_links(system,key,term_id,source) SELECT 'wd_rating',substr(key,11),id,'synthetic' FROM terms WHERE key LIKE 'synthetic-%';
            INSERT INTO term_assets(namespace,key) SELECT 'library',uuid FROM assets;
            INSERT INTO term_projection(asset_id,term_id,state) SELECT ta.id,t.id,'machine' FROM (VALUES
             ('a1','general'),('a2','explicit'),('b1','questionable'),('b2','sensitive'),('c1','general'),('d1','explicit'),('e1','explicit')) r
             JOIN term_assets ta ON ta.namespace='library' AND ta.key=r.column1 JOIN terms t ON t.key='synthetic-'||r.column2;
            INSERT INTO term_projection(asset_id,term_id,state) SELECT ta.id,t.id,'machine' FROM term_assets ta
             JOIN terms t ON t.key='synthetic-general' WHERE ta.key LIKE 'z%';").unwrap();
        // Both libraries hold four NSFW assets: 4/8 kept is below the threshold, 16/20 above it.
        let pays = |offset, limit| date_page_pays(&c, offset, limit).unwrap();
        if extra == 0 {
            assert!(!pays(0, 1));
        } else {
            assert!(pays(0, 2) && pays(3, 2) && pays(0, 8));
            assert!(!pays(7, 2) && !pays(0, 100));
        }
        drop(c);
        let context = crate::http::LibraryContext {
            db: dest.join("library.sqlite"),
            generation: "synthetic".into(),
            library: Some(dest.clone()),
            source: Some(dest.join("source.sqlite")),
            tags: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tags/tags_v3.csv"),
            token: Some("synthetic-token".into()),
            read_only: None,
        };
        let page = |query: &str| {
            let (body, status) = crate::http_read::read(&context, &format!("/api/assets?{query}")).unwrap().unwrap();
            assert_eq!(status, 200, "{query}");
            let ids = match body.get("items") {
                Some(Json::Array(items)) => items.iter().map(|v| get(v, "uuid").text().to_owned()).collect::<Vec<_>>(),
                _ => panic!("{query}"),
            };
            (ids, get(&body, "total").dump(), body)
        };
        let excluded = extras.iter().map(String::as_str).collect::<Vec<_>>();
        let excluded_total = extra.to_string();
        let cases: &[(&str, &[&str], &str)] = &[
            ("nsfw=hide", &["a1", "a3", "b2", "c1"], "4"),
            ("nsfw=hide&limit=2", &["a1", "a3"], "4"),
            ("nsfw=hide&limit=1&offset=1", &["a3"], "4"),
            ("nsfw=hide&limit=2&offset=2", &["b2", "c1"], "4"),
            ("nsfw=hide&limit=2&offset=3", &["c1"], "4"),
            ("nsfw=hide&limit=2&offset=4", &[], "4"),
            ("nsfw=hide&limit=2&offset=7", &[], "4"),
            ("nsfw=hide&offset=99", &[], "4"),
            ("nsfw=hide&order=oldest", &["c1", "b2", "a1", "a3"], "4"),
            ("nsfw=hide&order=oldest&limit=2", &["c1", "b2"], "4"),
            ("nsfw=hide&order=oldest&limit=1&offset=3", &["a3"], "4"),
            ("nsfw=hide&q=", &["a1", "a3", "b2", "c1"], "4"),
            ("nsfw=hide&q=&limit=2", &["a1", "a3"], "4"),
            ("nsfw=hide&order=hinted", &["a1", "a3", "b2", "c1"], "4"),
            ("nsfw=hide&order=suspicious&limit=3&offset=1", &["a3", "b2", "c1"], "4"),
            ("nsfw=hide&q=a", &["a1", "a3"], "2"),
            ("nsfw=hide&pool=duplicates", &["b2"], "1"),
            ("nsfw=hide&pool=excluded", &excluded, &excluded_total),
            ("nsfw=only", &["a2", "b1"], "2"),
            ("nsfw=only&order=oldest", &["b1", "a2"], "2"),
            ("nsfw=only&pool=duplicates", &["d1"], "1"),
            ("nsfw=only&q=nomatch", &[], "0"),
            ("nsfw=only&pool=excluded", &["e1"], "1"),
            ("nsfw=", &["a1", "a2", "a3", "b1", "b2", "c1"], "6"),
        ];
        for (query, expected, total) in cases {
            let (ids, shown_total, body) = page(query);
            assert_eq!(
                (ids.iter().map(String::as_str).collect::<Vec<_>>(), shown_total.as_str()),
                (expected.to_vec(), *total),
                "{query} extra={extra}"
            );
            // The page without a total is the same rows, byte for byte.
            let (_, untotaled, unpaged) = page(&format!("{query}&count=0"));
            assert_eq!(untotaled, "null", "{query}");
            assert_eq!(get(&body, "items").dump(), get(&unpaged, "items").dump(), "{query}");
            let filters = query.split('&').filter(|p| !p.starts_with("limit=") && !p.starts_with("offset="));
            let url = format!("/api/assets/count?{}", filters.collect::<Vec<_>>().join("&"));
            let counted = crate::http_read::read(&context, &url).unwrap().unwrap();
            assert_eq!(get(&counted.0, "total").dump(), *total, "{query}");
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
