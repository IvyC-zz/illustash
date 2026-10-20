//! Character/identity HTTP reads. Query order, stable post-sorts and dict insertion order follow Store.
use crate::character_entities::ENTITIES;
use crate::{
    http::LibraryContext,
    http_json::{parse_python, Json},
    http_read::{display, python_int, rows, strings, trim},
    Error, Result,
};
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::{BTreeSet, HashMap, HashSet};

pub const ROUTES: &[&str] = &[
    "/api/characters",
    "/api/tagged",
    "/api/character/profile",
    "/api/character/activity",
    "/api/character/stats",
    "/api/identity/count",
    "/api/identity/queue",
];
fn val<'a>(j: &'a Json, k: &str) -> &'a Json {
    j.get(k).expect("query field")
}
fn text(j: &Json, k: &str) -> String {
    val(j, k).text().to_owned()
}
fn number(j: &Json, k: &str) -> i64 {
    val(j, k).integer()
}
fn array(v: Vec<String>) -> Json {
    Json::Array(v.into_iter().map(Json::from).collect())
}
fn bad<T>(s: &str) -> Result<T> {
    Err(Error::Invalid(s.into()))
}
fn remove(j: &mut Json, k: &str) -> Json {
    if let Json::Object(v) = j {
        if let Some(i) = v.iter().position(|(x, _)| x == k) {
            return v.remove(i).1;
        }
    }
    None::<String>.into()
}
fn truth(v: &Json) -> bool {
    match v {
        Json::Scalar(x) => crate::character_commands::truthy(x),
        Json::Float(x) => *x != 0.,
        Json::Array(x) => !x.is_empty(),
        Json::Object(x) => !x.is_empty(),
        Json::Integer(x) => x != "0",
    }
}
fn rating(alias: &str, nsfw: &str) -> Result<String> {
    match nsfw {""=>Ok(String::new()),"hide"|"only"=>Ok(format!(" AND {}coalesce({alias}.uuid IN (SELECT uuid FROM asset_rating WHERE rating IN ('questionable', 'explicit')),0)",if nsfw=="hide"{"NOT "}else{""})),_=>bad("未知内容筛选")}
}
fn visible(a: &str) -> String {
    format!("NOT EXISTS(SELECT 1 FROM duplicates h WHERE h.uuid={a}.uuid AND h.hidden=1) AND NOT EXISTS(SELECT 1 FROM excluded x WHERE x.uuid={a}.uuid)")
}
fn hybrid(c: &Connection) -> Result<bool> {
    Ok(c.query_row("SELECT value FROM meta WHERE key='identity_policy'", [], |r| r.get::<_, String>(0))
        .optional()?
        .as_deref()
        == Some("hybrid"))
}
fn shown(c: &Connection, mut r: Json, skin: bool, identity: bool) -> Result<Json> {
    r = display(c, r)?;
    if skin {
        let s: String = c.query_row("SELECT entity_info(?,1)", [text(&r, "id")], |r| r.get(0))?;
        r.extend(parse_python(&s)?);
    }
    if !identity {
        for k in ["node_type", "character_id", "variant_id", "canonical_id"] {
            remove(&mut r, k);
        }
    }
    Ok(r)
}
fn search(c: &Connection, q: &str) -> Result<()> {
    c.execute_batch("CREATE TEMP TABLE IF NOT EXISTS name_matches(entity TEXT PRIMARY KEY,rank INTEGER NOT NULL); DELETE FROM name_matches;")?;
    let mut hits = HashMap::new();
    for (e, r) in crate::names_search::hits(c, q)? {
        let e = crate::character_commands::resolve(c, &e)?;
        let v = hits.entry(e).or_insert(-1);
        *v = (*v).max(r);
    }
    for (e, r) in hits {
        c.execute("INSERT INTO name_matches VALUES (?,?)", params![e, r])?;
    }
    Ok(())
}
fn characters(c: &Connection, q: &HashMap<String, String>, minimum: i64) -> Result<Json> {
    if minimum < 0 {
        return bad("最小成员数不正确");
    }
    let get = |k: &str| q.get(k).map(String::as_str).unwrap_or("");
    let query: String = trim(get("q")).chars().take(160).collect();
    let ident = get("id");
    let nsfw = get("nsfw");
    let summary = get("summary") == "1";
    if !query.is_empty() {
        search(c, &query)?;
    }
    let work = get("work");
    let mut args = vec![nsfw.to_owned()];
    let work_ids = if work.is_empty() {
        Vec::new()
    } else {
        let found: i64 = c.query_row(
            &format!(
                "SELECT count(*)
                 FROM {ENTITIES}
                 WHERE id=? AND kind='work' AND merged_into IS NULL"
            ),
            [work],
            |r| r.get(0),
        )?;
        if found == 0 {
            return bad("作品不存在");
        }
        strings(c, &format!("{} SELECT id FROM tree", crate::http_read::TREE), &[work])?
    };
    let work_filter = if work_ids.is_empty() {
        String::new()
    } else {
        format!(" AND e.work_id IN ({})", vec!["?"; work_ids.len()].join(","))
    };
    if !ident.is_empty() {
        args.extend([ident.to_owned(), ident.to_owned(), ident.to_owned()]);
    }
    args.extend(work_ids);
    args.push(minimum.to_string());
    let sql = format!(
        "SELECT e.id,entity_name(e.id) AS name,e.work,e.kind,e.artist,e.label_only,e.mascot,e.mascot_origin,
           count(m.uuid) AS count,coalesce(sum(m.status='machine'),0) AS unchecked,
           min(CASE WHEN ?<>'hide'
                      OR NOT coalesce(m.uuid IN (SELECT uuid FROM asset_rating WHERE rating IN ('questionable', 'explicit')),0)
                    THEN m.uuid END) AS cover,
           (SELECT max(reviewed_at) FROM reviews r WHERE r.entity=e.id) AS reviewed_at
         FROM {ENTITIES} e
         {matched}
         LEFT JOIN {members} m ON m.entity=e.id AND {visible}{rating}
         WHERE e.parent_id IS NULL AND e.kind<>'work' AND e.merged_into IS NULL{mascot}{only}{work_filter}
         GROUP BY e.id
         HAVING count(m.uuid)>=?
         ORDER BY {ranking}count DESC,name,e.id",
        matched = if query.is_empty() { "" } else { "JOIN name_matches sm ON sm.entity=e.id" },
        members = if ident.is_empty() {
            "memberships"
        } else {
            "(SELECT * FROM memberships WHERE entity=? UNION ALL SELECT * FROM tag_memberships WHERE entity=?)"
        },
        visible = visible("m"),
        rating = rating("m", nsfw)?,
        mascot = if ident.is_empty() { " AND e.mascot=0" } else { "" },
        only = if ident.is_empty() { "" } else { " AND e.id=?" },
        ranking = if query.is_empty() { "" } else { "sm.rank DESC," },
    );
    // Python int is unbounded; sqlite3 rejects a minimum outside its signed 64-bit binding range.
    // Check at the SQL bind point, after the same work/rating checks as Store.characters.
    let exact_min: String = get("min")
        .trim()
        .chars()
        .filter(|c| *c != '_')
        .map(|c| crate::compat_unicode::decimal(c).map(|n| (b'0' + n) as char).unwrap_or(c))
        .collect();
    if !exact_min.is_empty() && exact_min.parse::<i64>().is_err() {
        return Err(Error::Sql(rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Python int too large to convert to SQLite INTEGER",
        )))));
    }
    let mut bound: Vec<rusqlite::types::Value> = args.into_iter().map(Into::into).collect();
    *bound.last_mut().expect("minimum argument") = minimum.into();
    let mut result = crate::http_read::typed_rows(c, &sql, &bound)?
        .into_iter()
        .map(|r| shown(c, r, false, !summary))
        .collect::<Result<Vec<_>>>()?;
    if result.is_empty() {
        return Ok(Json::Array(result));
    }
    let ids = Json::Array(result.iter().map(|r| val(r, "id").clone()).collect()).dump();
    let variant_count = if summary {
        "NULL".to_owned()
    } else {
        format!(
            "(SELECT count(*) FROM variant_memberships v WHERE v.entity=e.id AND {}{})",
            visible("v"),
            rating("v", nsfw)?
        )
    };
    let mut variants: HashMap<String, Vec<Json>> = HashMap::new();
    let skins = format!(
        "SELECT e.id,e.parent_id,e.variant AS name,{variant_count} AS count
         FROM {ENTITIES} e
         WHERE e.parent_id IN (SELECT value FROM json_each(?)) AND e.kind<>'work' AND e.merged_into IS NULL
         ORDER BY e.variant,e.id"
    );
    for r in rows(c, &skins, &[&ids])? {
        let parent = text(&r, "parent_id");
        variants.entry(parent).or_default().push(shown(c, r, true, !summary)?);
    }
    for r in &mut result {
        r.set("variants", Json::Array(variants.remove(&text(r, "id")).unwrap_or_default()));
        if summary {
            r.set("summary", true.into());
        }
    }
    let ranked=rows(c,&format!("SELECT entity,uuid FROM (SELECT m.entity,m.uuid,row_number() OVER (PARTITION BY m.entity ORDER BY m.status='machine',coalesce(a.width>a.height,0),a.added_at DESC,m.uuid) AS rk FROM memberships m JOIN assets a ON a.uuid=m.uuid WHERE m.entity IN (SELECT value FROM json_each(?)){}) WHERE rk<=12 ORDER BY entity,rk",rating("m",nsfw)?),&[&ids])?;
    let pool = array(ranked.iter().map(|r| text(r, "uuid")).collect::<BTreeSet<_>>().into_iter().collect()).dump();
    let vis: HashSet<String> = strings(
        c,
        &format!("SELECT a.uuid FROM assets a WHERE a.uuid IN (SELECT value FROM json_each(?)) AND {}", visible("a")),
        &[&pool],
    )?
    .into_iter()
    .collect();
    let shared: HashSet<String> = strings(
        c,
        "SELECT uuid FROM memberships WHERE uuid IN (SELECT value FROM json_each(?)) GROUP BY uuid HAVING count(*)>1",
        &[&pool],
    )?
    .into_iter()
    .collect();
    let mut options: HashMap<String, Vec<String>> = HashMap::new();
    for r in ranked {
        let u = text(&r, "uuid");
        if vis.contains(&u) {
            options.entry(text(&r, "entity")).or_default().push(u);
        }
    }
    for opts in options.values_mut() {
        opts.sort_by_key(|u| shared.contains(u));
    }
    let mut ordered: Vec<usize> = (0..result.len()).collect();
    ordered.sort_by_key(|i| -number(&result[*i], "count"));
    let mut used = HashSet::new();
    for i in ordered {
        let r = &mut result[i];
        let opts = options.get(&text(r, "id"));
        let cover = opts
            .and_then(|v| v.iter().find(|u| !used.contains(*u)).or(v.first()))
            .map(|u| Json::from(u.clone()))
            .unwrap_or_else(|| val(r, "cover").clone());
        used.insert(cover.text().to_owned());
        r.set("cover", cover);
    }
    if summary {
        result = result
            .into_iter()
            .map(|r| {
                let mut s = Json::Object(Vec::new());
                for k in [
                    "id",
                    "name",
                    "tag",
                    "name_source",
                    "work",
                    "work_id",
                    "work_name",
                    "kind",
                    "count",
                    "unchecked",
                    "cover",
                    "summary",
                ] {
                    if let Some(v) = r.get(k) {
                        s.set(k, v.clone());
                    }
                }
                let variants = if let Json::Array(v) = val(&r, "variants") {
                    v.iter()
                        .map(|v| Json::object([("id", val(v, "id").clone()), ("name", val(v, "name").clone())]))
                        .collect()
                } else {
                    Vec::new()
                };
                s.set("variants", Json::Array(variants));
                s
            })
            .collect();
    }
    Ok(Json::Array(result))
}
fn profile(c: &Connection, e: &str) -> Result<Json> {
    let id = crate::character_commands::resolve(c, e)?;
    let row = rows(
        c,
        &format!(
            "SELECT id,parent_id,kind,original,label_only,work_id
             FROM {ENTITIES}
             WHERE kind<>'work' AND id=?"
        ),
        &[&id],
    )?
    .into_iter()
    .next()
    .ok_or_else(|| Error::Invalid("角色不存在".into()))?;
    if truth(val(&row, "parent_id")) {
        return bad("请编辑角色本身，子标签跟随角色");
    }
    let shown = display(c, Json::object([("id", id.clone().into())]))?;
    let user = strings(
        c,
        "SELECT value
         FROM term_names
         WHERE term_id = (SELECT id FROM terms WHERE key = ?) AND role = 'display' AND source = 'user'
         ORDER BY preferred DESC
         LIMIT 1",
        &[&id],
    )?
    .into_iter()
    .next()
    .unwrap_or_default();
    let tag = strings(
        c,
        &format!(
            "SELECT l.key
                 FROM {LINKS} l
                 WHERE l.entity_id = ? AND l.core_system = 'danbooru' AND l.current = 1
                 LIMIT 1",
            LINKS = crate::character_links::LINKS
        ),
        &[&id],
    )?
    .into_iter()
    .next()
    .unwrap_or_default();
    let other: i64 = c.query_row(
        "SELECT count(DISTINCT value)
         FROM term_names
         WHERE term_id = (SELECT id FROM terms WHERE key = ?) AND role = 'alias' AND source <> 'user'",
        [&id],
        |r| r.get(0),
    )?;
    let work = text(&row, "work_id");
    let name =
        if work.is_empty() { String::new() } else { c.query_row("SELECT entity_name(?)", [&work], |r| r.get(0))? };
    Ok(Json::object([
        ("id", id.clone().into()),
        ("name", val(&shown, "name").clone()),
        ("user_name", user.into()),
        (
            "aliases",
            array(strings(
                c,
                "SELECT value
                 FROM term_names
                 WHERE term_id = (SELECT id FROM terms WHERE key = ?) AND role = 'alias' AND source = 'user'
                 ORDER BY position, value",
                &[&id],
            )?),
        ),
        ("other_aliases", other.into()),
        ("tag", tag.into()),
        ("work_id", val(&row, "work_id").clone()),
        ("work_name", name.into()),
        ("oc", (number(&row, "original") != 0).into()),
        ("label_only", (number(&row, "label_only") != 0).into()),
    ]))
}
fn activity(c: &Connection, nsfw: &str) -> Result<Json> {
    let t = crate::platform_time::local_date(30 * 86400);
    let since = format!("{:04}-{:02}-{:02}", t.tm_year + 1900, t.tm_mon + 1, t.tm_mday);
    let mut out = Json::Object(Vec::new());
    for r in rows(c,&format!("SELECT m.entity,sum(a.added_at>=?) AS recent,max(substr(a.added_at,1,10)) AS last FROM memberships m JOIN assets a ON a.uuid=m.uuid WHERE {}{} GROUP BY m.entity",visible("m"),rating("m",nsfw)?),&[&since])? {out.set(text(&r,"entity"),Json::object([("recent",number(&r,"recent").into()),("last",text(&r,"last").into())]));}
    Ok(out)
}
fn stats(c: &Connection, id: &str, nsfw: &str, exclude: &str) -> Result<Json> {
    if id.is_empty() {
        return bad("缺少角色");
    }
    let where_ = format!("m.entity=? AND {}{}", visible("m"), rating("m", nsfw)?);
    let per=rows(c,&format!("SELECT substr(a.added_at,1,7) AS month,count(*) AS count FROM memberships m JOIN assets a ON a.uuid=m.uuid WHERE {where_} GROUP BY 1"),&[id])?;
    let mut dated: Vec<String> = per.iter().map(|r| text(r, "month")).filter(|s| !s.is_empty()).collect();
    dated.sort();
    let t = crate::platform_time::local_date(0);
    let mut y = t.tm_year + 1900;
    let mut m = t.tm_mon + 1;
    let mut months = Vec::new();
    for _ in 0..12 {
        let k = format!("{y:04}-{m:02}");
        let n = per.iter().find(|r| text(r, "month") == k).map(|r| number(r, "count")).unwrap_or(0);
        months.push(Json::object([("month", k.into()), ("count", n.into())]));
        if m == 1 {
            y -= 1;
            m = 12;
        } else {
            m -= 1;
        }
    }
    months.reverse();
    let recent=strings(c,&format!("SELECT m.uuid FROM memberships m JOIN assets a ON a.uuid=m.uuid WHERE {where_} AND m.uuid<>? ORDER BY a.added_at DESC,m.uuid LIMIT 4"),&[id,exclude])?;
    Ok(Json::object([
        ("count", per.iter().map(|r| number(r, "count")).sum::<i64>().into()),
        ("first", dated.first().cloned().unwrap_or_default().into()),
        ("months", Json::Array(months)),
        ("recent", array(recent)),
    ]))
}
fn people(c: &Connection) -> Result<HashMap<String, String>> {
    Ok(rows(c, "SELECT id,character_id FROM identity_nodes", &[])?
        .into_iter()
        .map(|r| (text(&r, "id"), text(&r, "character_id")))
        .collect())
}
fn page<T>(v: Vec<T>, offset: i64, limit: i64) -> Vec<T> {
    v.into_iter().skip(usize::try_from(offset).unwrap_or(usize::MAX)).take(limit as usize).collect()
}
fn pair_key(a: &str, b: &str) -> (String, String) {
    if a <= b {
        (a.into(), b.into())
    } else {
        (b.into(), a.into())
    }
}
fn numeric(j: &Json) -> Option<f64> {
    match j {
        Json::Float(x) => Some(*x),
        Json::Scalar(v) => v.as_f64(),
        _ => None,
    }
}
fn queue(c: &Connection, limit: i64, offset: i64, nsfw: &str, count_only: bool) -> Result<Json> {
    if !(1..=100).contains(&limit) || offset < 0 {
        return bad("分页参数不正确");
    }
    let shown = rating("m", nsfw)?;
    let nodes = people(c)?;
    let fam = |e: &str| nodes.get(e).cloned().unwrap_or_else(|| e.to_owned());
    let counts: HashMap<String, i64> = rows(
        c,
        &format!("SELECT entity,count(*) AS count FROM memberships m WHERE {}{shown} GROUP BY entity", visible("m")),
        &[],
    )?
    .iter()
    .map(|r| (text(r, "entity"), number(r, "count")))
    .collect();
    let decided: HashSet<_> = rows(c, "SELECT a,b FROM identity_relations WHERE status<>'proposed'", &[])?
        .iter()
        .map(|r| pair_key(&fam(&text(r, "a")), &fam(&text(r, "b"))))
        .collect();
    let skipped = strings(c, "SELECT value FROM prefs WHERE key='identity_skipped'", &[])?
        .into_iter()
        .next()
        .unwrap_or_else(|| "[]".into());
    let skipped = parse_python(&skipped)?;
    let skip: HashSet<String> =
        if let Json::Array(v) = skipped { v.iter().map(|x| x.text().to_owned()).collect() } else { HashSet::new() };
    let marks: HashSet<String> =
        strings(c, &format!("SELECT id FROM {ENTITIES} WHERE label_only=1"), &[])?.iter().map(|e| fam(e)).collect();
    let mut found: Vec<Json> = Vec::new();
    let mut keys = HashMap::new();
    let mut add = |a: String, b: String, distance: Json, hint: Json, relation: Json| {
        let a = fam(&a);
        let b = fam(&b);
        let key = pair_key(&a, &b);
        if a == b
            || marks.contains(&a)
            || marks.contains(&b)
            || decided.contains(&key)
            || skip.contains(&format!("{}|{}", key.0, key.1))
            || counts.get(&a).copied().unwrap_or(0) == 0
            || counts.get(&b).copied().unwrap_or(0) == 0
        {
            return;
        }
        let i = *keys.entry(key).or_insert_with(|| {
            found.push(Json::object([
                ("a", a.clone().into()),
                ("b", b.clone().into()),
                ("distance", None::<String>.into()),
                ("hint", None::<String>.into()),
                ("relation_id", None::<String>.into()),
            ]));
            found.len() - 1
        });
        let item = &mut found[i];
        if let Some(d) = numeric(&distance) {
            if numeric(val(item, "distance")).is_none_or(|old| d < old) {
                item.set("distance", distance);
            }
        }
        if truth(&hint) && !truth(val(item, "hint")) {
            item.set("hint", hint);
            item.set("relation_id", relation);
        }
    };
    for r in rows(c, "SELECT a,b,original_distance,distance,evidence_json FROM identity_pairs", &[])? {
        let mut hint = None::<String>.into();
        let distance = if count_only {
            None::<String>.into()
        } else {
            let e = parse_python(val(&r, "evidence_json").text())?;
            if ["relation", "subject", "object"].iter().all(|k| e.get(k).is_some_and(truth)) {
                hint = Json::object([
                    ("kind", val(&e, "relation").clone()),
                    ("subject", val(&e, "subject").clone()),
                    ("object", val(&e, "object").clone()),
                    ("source", "Danbooru".into()),
                ]);
            }
            if numeric(val(&r, "distance")).is_some() {
                val(&r, "distance").clone()
            } else {
                val(&r, "original_distance").clone()
            }
        };
        add(text(&r, "a"), text(&r, "b"), distance, hint, None::<String>.into());
    }
    for r in rows(c,"SELECT r.id,r.a,r.b,r.kind,(SELECT min(source) FROM identity_evidence e WHERE e.relation_id=r.id) AS source FROM identity_relations r WHERE r.status='proposed'",&[])? {let a=text(&r,"a");let b=text(&r,"b");let hint=if count_only{None::<String>.into()}else{let tags:HashMap<String,String>=rows(c,&format!(
            "SELECT l.entity_id, l.key
             FROM {LINKS} l
             WHERE l.core_system = 'danbooru' AND l.entity_id IN (?, ?)",
            LINKS = crate::character_links::LINKS
        ),&[&a,&b])?.iter().map(|r|(text(r,"entity_id"),text(r,"key"))).collect();Json::object([("kind",val(&r,"kind").clone()),("subject",tags.get(&a).cloned().unwrap_or_default().into()),("object",tags.get(&b).cloned().unwrap_or_default().into()),("source",text(&r,"source").into())])};add(a,b,None::<String>.into(),hint,val(&r,"id").clone());}
    let total = found.len();
    if count_only {
        return Ok(Json::object([("total", (total as i64).into())]));
    }
    found.sort_by(|x, y| {
        let n = |v: &Json| counts[&text(v, "a")] + counts[&text(v, "b")];
        n(y).cmp(&n(x)).then(text(x, "a").cmp(&text(y, "a"))).then(text(x, "b").cmp(&text(y, "b")))
    });
    let mut items = page(found, offset, limit);
    for x in &mut items {
        for side in ["a", "b"] {
            let e = text(x, side);
            let name: String = c.query_row("SELECT entity_info(?,0)", [&e], |r| r.get(0))?;
            let info = parse_python(&name)?;
            let samples=strings(c,&format!("SELECT m.uuid FROM memberships m WHERE m.entity=? AND {}{shown} ORDER BY m.status='confirmed' DESC,m.score DESC,m.uuid LIMIT 4",visible("m")),&[&e])?;
            x.set(
                side,
                Json::object([
                    ("id", e.clone().into()),
                    ("name", info.get("name").cloned().unwrap_or_else(|| "".into())),
                    ("tag", info.get("tag").cloned().unwrap_or_else(|| "".into())),
                    ("count", counts[&e].into()),
                    ("samples", array(samples)),
                ]),
            );
        }
    }
    Ok(Json::object([("items", Json::Array(items)), ("total", (total as i64).into())]))
}
pub(crate) fn assignments(c: &Connection, id: &str) -> Result<Json> {
    let (a, mut p) = if hybrid(c)? {
        (rows(c,"SELECT m.uuid,m.entity AS character_id,v.entity AS variant_id,m.status,m.origin FROM memberships m LEFT JOIN (SELECT entity,parent_id FROM variant_memberships WHERE uuid=?) v ON v.parent_id=m.entity WHERE m.uuid=?",&[id,id])?,rows(c,"SELECT options_json,reason FROM identity_uncertain WHERE uuid=? UNION ALL SELECT options_json,reason FROM identity_suspended WHERE uuid=?",&[id,id])?)
    } else {
        (Vec::new(), Vec::new())
    };
    for r in &mut p {
        let options = remove(r, "options_json");
        r.set("options", parse_python(options.text())?);
    }
    Ok(Json::object([("assignments", Json::Array(a)), ("undetermined", Json::Array(p))]))
}
pub fn read(c: &Connection, _context: &LibraryContext, path: &str, q: &HashMap<String, String>) -> Result<(Json, u16)> {
    let get = |k: &str| q.get(k).map(String::as_str).unwrap_or("");
    let int =
        |k: &str, default: &str| python_int(q.get(k).map(String::as_str).unwrap_or(default)).map_err(Error::Invalid);
    let value = match path {
        "/api/characters" => characters(c, q, int("min", "0")?)?,
        "/api/tagged" => {
            let sql = format!(
                "SELECT e.id,entity_name(e.id) AS name,
                   (SELECT count(*) FROM tag_memberships m WHERE m.entity=e.id AND {visible}{rating}) AS count
                 FROM {ENTITIES} e
                 WHERE e.mascot=1 AND e.merged_into IS NULL
                 ORDER BY count DESC,name,e.seq",
                visible = visible("m"),
                rating = rating("m", get("nsfw"))?,
            );
            Json::Array(rows(c, &sql, &[])?)
        }
        "/api/character/profile" => profile(c, get("entity"))?,
        "/api/character/activity" => activity(c, get("nsfw"))?,
        "/api/character/stats" => stats(c, get("id"), get("nsfw"), get("exclude"))?,
        "/api/identity/count" => queue(c, 20, 0, get("nsfw"), true)?,
        "/api/identity/queue" => queue(c, int("limit", "20")?, int("offset", "0")?, get("nsfw"), false)?,
        _ => return bad("unknown character read"),
    };
    Ok((value, 200))
}

pub fn validate(path: &str, q: &HashMap<String, String>) -> Result<()> {
    let args: &[(&str, &str)] = match path {
        "/api/characters" => &[("min", "0")],
        "/api/identity/queue" => &[("limit", "20"), ("offset", "0")],
        _ => &[],
    };
    let numbers = args
        .iter()
        .map(|(k, default)| python_int(q.get(*k).map(String::as_str).unwrap_or(default)).map_err(Error::Invalid))
        .collect::<Result<Vec<_>>>()?;
    let get = |key| q.get(key).map(String::as_str).unwrap_or("");
    match path {
        "/api/characters" if numbers[0] < 0 => return bad("最小成员数不正确"),
        "/api/identity/queue" => {
            if !(1..=100).contains(&numbers[0]) || numbers[1] < 0 {
                return bad("分页参数不正确");
            }
            rating("m", get("nsfw"))?;
        }
        "/api/identity/count" => {
            rating("m", get("nsfw"))?;
        }
        "/api/character/stats" => {
            if get("id").is_empty() {
                return bad("缺少角色");
            }
            rating("m", get("nsfw"))?;
        }
        _ => {}
    }
    Ok(())
}
