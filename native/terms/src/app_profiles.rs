//! Native character profile HTTP orchestration. Core mutations and projections share one transaction.
use crate::{
    app_commands::{core_error, initialized, last_action},
    http_json::Json,
    http_read, Error, Result,
};
use rusqlite::{Connection, TransactionBehavior};
use serde_json::{json, Value};

pub(crate) const ROUTES: &[&str] = &[
    "/api/entities",
    "/api/character/profile",
    "/api/local-links",
    "/api/works",
    "/api/character/mascot",
    "/api/kind",
    "/api/oc/shared",
];
fn invalid<T>(s: &str) -> Result<T> {
    Err(Error::Invalid(s.into()))
}
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
        Value::Number(n) => n.as_f64().is_some_and(|n| n != 0.0),
    }
}
fn tag(v: &Value) -> Result<String> {
    let Some(s) = v.as_str() else { return invalid("请选择 Danbooru 角色 tag") };
    let s = http_read::trim(s);
    if s.is_empty() || s.chars().count() > 200 {
        return invalid("请选择 Danbooru 角色 tag");
    }
    let s = s.to_lowercase().replace(' ', "_");
    if s.chars().any(http_read::whitespace) {
        return invalid("请选择 Danbooru 角色 tag");
    }
    Ok(s)
}
fn oc(v: &Value) -> Result<(String, String)> {
    let kind = v.get("kind").unwrap_or(&Value::Null);
    let kind = if kind.is_null() && v.get("kind").is_none() { "" } else { kind.as_str().unwrap_or("invalid") };
    let artist = v.get("artist").and_then(Value::as_str).or_else(|| v.get("artist").is_none().then_some(""));
    let Some(artist) = artist else { return invalid("画师名称不正确") };
    let artist = http_read::trim(artist);
    if !matches!(kind, "" | "oc") {
        return invalid("未知角色类型");
    }
    if artist.chars().count() > 120 || (!artist.is_empty() && kind != "oc") {
        return invalid("画师名称不正确");
    }
    Ok((kind.into(), artist.into()))
}
fn shown(c: &Connection, id: &str) -> Result<Json> {
    http_read::display(c, Json::object([("id", id.into())]))
}
pub(crate) struct Prepared {
    pub works: std::collections::HashMap<String, String>,
    version: String,
}
pub(crate) fn prepare(
    c: &Connection,
    body: &Json,
    path: &str,
    source: &mut crate::app_name_sources::Source,
) -> Result<Prepared> {
    let fields: &[&str] = match path {
        "/api/entities" if body.get("tag").is_some_and(crate::app_service::truthy) => &["tag"],
        "/api/character/profile" if body.get("tag").is_some_and(crate::app_service::truthy) => &["tag", "work"],
        "/api/local-links" => &["items"],
        _ => &[],
    };
    let mut selected = Json::object(fields.iter().filter_map(|k| body.get(k).map(|v| (*k, v.clone()))));
    if path == "/api/local-links" {
        if let Some(Json::Array(items)) = body.get("items") {
            selected.set(
                "items",
                Json::Array(
                    items
                        .iter()
                        .map(|v| {
                            if let Json::Object(_) = v {
                                Json::object(["tag"].iter().filter_map(|k| v.get(k).map(|v| (*k, v.clone()))))
                            } else {
                                v.clone()
                            }
                        })
                        .collect(),
                ),
            );
        }
    }
    let v: Value = serde_json::from_str(&selected.dump()).map_err(|_| Error::Invalid("关联参数不正确".into()))?;
    let version = c.query_row("SELECT value FROM meta WHERE key='names_version'", [], |r| r.get(0))?;
    let mut tags = Vec::new();
    match path {
        "/api/entities" if truthy(&v["tag"]) => {
            let t = tag(&v["tag"])?;
            if crate::character_profile::tag_owner(c, &t, None)?.is_none() {
                tags.push(t);
            }
        }
        "/api/character/profile" if truthy(&v["tag"]) && v["work"].is_null() => tags.push(tag(&v["tag"])?),
        "/api/local-links" => {
            let Some(items) =
                v["items"].as_array().filter(|a| (1..=200).contains(&a.len()) && a.iter().all(Value::is_object))
            else {
                return invalid("请选择要关联的角色");
            };
            for item in items {
                tags.push(tag(&item["tag"])?);
            }
        }
        _ => {}
    }
    let mut works = std::collections::HashMap::new();
    for t in tags {
        if let std::collections::hash_map::Entry::Vacant(entry) = works.entry(t.clone()) {
            let cp = if crate::character_profile::local_work(c, &[crate::character_profile::qualifier(&t)])?.is_some() {
                String::new()
            } else {
                source.copyright(&t)?
            };
            entry.insert(cp);
        }
    }
    Ok(Prepared { works, version })
}
fn copyright(c: &Connection, tag: &str, prepared: Option<&Prepared>) -> Result<String> {
    if crate::character_profile::local_work(c, &[crate::character_profile::qualifier(tag)])?.is_some() {
        return Ok(String::new());
    }
    prepared.and_then(|p| p.works.get(tag)).cloned().ok_or_else(|| Error::Invalid("版权查询尚未准备".into()))
}
fn run(c: &Connection, args: &Value) -> Result<Value> {
    crate::character_commands::command(c, args).map_err(core_error)
}
/// Name requests share the mutation transaction; the native updater acknowledges successful application.
fn names(c: &Connection, id: Option<&str>) -> Result<()> {
    let kind = format!("names:{}", crate::character_json::dump(&json!({"entity":id,"refresh":false}), false));
    c.execute("INSERT INTO backend_effects(kind) VALUES (?)", [kind])?;
    Ok(())
}
pub(crate) fn request(c: &mut Connection, body: &Json, path: &str, prepared: Option<&Prepared>) -> Result<Json> {
    initialized(c)?;
    // The Python handlers consume only these fields; unused NaN/large integers are irrelevant.
    let linked = match body.get("tag") {
        Some(Json::Scalar(v)) => truthy(v),
        Some(Json::Float(v)) => *v != 0.0,
        Some(Json::Integer(v)) => v != "0",
        Some(Json::Array(v)) => !v.is_empty(),
        Some(Json::Object(v)) => !v.is_empty(),
        None => false,
    };
    let fields: &[&str] = match path {
        "/api/entities" if linked => &["tag", "typed"],
        "/api/entities" => &["name", "kind", "artist"],
        "/api/character/profile" => &["entity", "name", "aliases", "work", "tag"],
        "/api/local-links" => &["items"],
        "/api/works" => &["action", "id", "name", "parent", "tag"],
        "/api/character/mascot" => &["entity", "mascot"],
        "/api/kind" => &["entity", "kind", "artist"],
        "/api/oc/shared" => &[],
        _ => return invalid("接口不存在"),
    };
    let mut selected = Json::object(fields.iter().filter_map(|key| body.get(key).map(|value| (*key, value.clone()))));
    if path == "/api/entities" && linked && !matches!(body.get("typed"), Some(Json::Scalar(Value::String(_)))) {
        selected.set("typed", "".into());
    }
    if path == "/api/local-links" {
        if let Some(Json::Array(items)) = body.get("items") {
            selected.set(
                "items",
                Json::Array(
                    items
                        .iter()
                        .map(|item| match item {
                            Json::Object(_) => Json::object(
                                ["id", "tag"].iter().filter_map(|key| item.get(key).map(|v| (*key, v.clone()))),
                            ),
                            _ => item.clone(),
                        })
                        .collect(),
                ),
            );
        }
    }
    let mut v: Value = serde_json::from_str(&selected.dump()).map_err(|e| Error::Invalid(e.to_string()))?;
    let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if let Some(p) = prepared {
        let version: String = tx.query_row("SELECT value FROM meta WHERE key='names_version'", [], |r| r.get(0))?;
        if version != p.version {
            return invalid("名称来源已改变，请重试");
        }
    }
    crate::schema::begin_writing(&tx)?;
    let response = match path {
        "/api/entities" if truthy(&v["tag"]) => {
            let t = tag(&v["tag"])?;
            let id = if let Some(owner) = crate::character_profile::tag_owner(&tx, &t, None)? {
                owner
            } else {
                let cp = copyright(&tx, &t, prepared)?;
                let typed = http_read::trim(v["typed"].as_str().unwrap_or("")).chars().take(120).collect::<String>();
                let alias = (!typed.is_empty()
                    && http_read::normalize(&typed.replace(' ', "_")) != http_read::normalize(&t))
                .then_some(typed);
                run(&tx,&json!({"op":"create_linked","tag":t,"alias":alias,"copyright":cp,"readable":crate::names_view::readable(&cp,false)}))?["id"].as_str().expect("validated internal text field").to_owned()
            };
            // Render only after after_write has rebuilt the display projection.
            names(&tx, Some(&id))?;
            (id, None, 1)
        }
        "/api/entities" => {
            let (kind, artist) = oc(&v)?;
            let name = if kind == "oc" && (v["name"].is_null() || v["name"] == "") {
                let taken = http_read::strings(
                    &tx,
                    "SELECT n.value
                     FROM term_names n
                     JOIN term_tree r ON r.term_id = n.term_id
                     WHERE n.source = 'user' AND n.value LIKE '原创角色 %'",
                    &[],
                )?;
                (1..=taken.len() + 1)
                    .map(|n| format!("原创角色 {n}"))
                    .find(|n| !taken.contains(n))
                    .expect("app_profiles internal invariant")
            } else {
                let Some(name) = v["name"].as_str() else { return invalid("角色名称应为 1–120 个字符") };
                let name = http_read::trim(name);
                if name.is_empty() || name.chars().count() > 120 {
                    return invalid("角色名称应为 1–120 个字符");
                }
                name.into()
            };
            let rows = http_read::rows(&tx,"SELECT e.id,entity_name(e.id) AS name,e.kind,e.artist FROM entities e WHERE e.parent_id IS NULL AND e.kind<>'work' AND e.merged_into IS NULL AND (e.id IN (SELECT entity_id FROM links WHERE system IN ('danbooru','wd') AND key=? COLLATE NOCASE) OR e.id IN (SELECT t.key FROM term_names n JOIN terms t ON t.id = n.term_id WHERE n.value = ? COLLATE NOCASE AND n.role = 'alias') OR (e.id IN (SELECT t.key FROM term_names n JOIN terms t ON t.id = n.term_id WHERE n.value = ? COLLATE NOCASE AND n.source = 'user') AND NOT EXISTS(SELECT 1 FROM links l WHERE l.entity_id=e.id)))",&[&name,&name,&name])?;
            if let Some(row) = rows.into_iter().next() {
                let result = http_read::display(&tx, row)?;
                crate::schema::commit_writing(tx)?;
                return Ok(result);
            }
            let out = run(
                &tx,
                &json!({"op":"create_entity","name":name,"lang":crate::character_labels::lang_of(&name),"kind":kind,"artist":artist}),
            )?;
            let id = out["id"].as_str().expect("validated internal text field").to_owned();
            (
                id.clone(),
                Some(Json::object([
                    ("id", id.into()),
                    ("name", name.into()),
                    ("kind", kind.into()),
                    ("artist", artist.into()),
                ])),
                1,
            )
        }
        "/api/oc/shared" => {
            let rows=http_read::rows(&tx,"SELECT e.id,entity_name(e.id) AS name,e.kind,e.artist FROM entities e WHERE label_only=1 AND merged_into IS NULL ORDER BY rowid LIMIT 1",&[])?;
            if let Some(row) = rows.into_iter().next() {
                let result = http_read::display(&tx, row)?;
                crate::schema::commit_writing(tx)?;
                return Ok(result);
            }
            let id =
                run(&tx, &json!({"op":"shared_oc"}))?["id"].as_str().expect("validated internal text field").to_owned();
            (
                id.clone(),
                Some(Json::object([
                    ("id", id.into()),
                    ("name", crate::character_structure::SHARED_OC.into()),
                    ("kind", "oc".into()),
                    ("artist", "".into()),
                ])),
                1,
            )
        }
        "/api/character/profile" => {
            // A work chosen as new ({"create": name}) or from a Danbooru tag ({"tag": tag}) is made in this
            // same transaction: a failed profile write leaves no orphan work, and one save is one request.
            if v["work"].is_object() {
                let work = v["work"].take();
                let mut w = json!({"op":"edit_work","ident":null,"id":null});
                if let Some(name) = work.get("create").and_then(Value::as_str) {
                    w["action"] = json!("create");
                    w["name"] = json!(name);
                    w["lang"] = json!(crate::character_labels::lang_of(name));
                } else if let Some(t) = work.get("tag") {
                    let t = tag(t)?;
                    w["action"] = json!("from_tag");
                    w["lang"] = json!(crate::character_labels::lang_of(""));
                    w["readable"] = json!(crate::names_view::readable(&t, false));
                    w["tag"] = json!(t);
                } else {
                    return invalid("作品应为已有作品、新建名称或 Danbooru tag");
                }
                v["work"] = run(&tx, &w)?["id"].clone();
            }
            v["op"] = json!("set_profile");
            if truthy(&v["tag"]) {
                let t = tag(&v["tag"])?;
                v["copyright"] = json!(if v["work"].is_null() { copyright(&tx, &t, prepared)? } else { String::new() });
                v["tag"] = json!(t);
            }
            let out = run(&tx, &v)?;
            tx.execute("INSERT INTO backend_effects(kind) VALUES ('verdict')", [])?;
            let id = out["id"].as_str().expect("validated internal text field").to_owned();
            names(&tx, Some(&id))?;
            (id, None, 2)
        }
        "/api/local-links" => {
            let Some(items) =
                v["items"].as_array().filter(|a| (1..=200).contains(&a.len()) && a.iter().all(Value::is_object))
            else {
                return invalid("请选择要关联的角色");
            };
            let mut plan = Vec::new();
            // Validate the entire plan before external lookups, matching _link_args.
            for item in items {
                plan.push((item["id"].clone(), tag(&item["tag"])?));
            }
            let mut works = std::collections::HashMap::new();
            for (_, t) in &plan {
                works.insert(t.clone(), copyright(&tx, t, prepared)?);
            }
            let items: Vec<_> =
                plan.into_iter().map(|(id, t)| json!({"id":id,"copyright":works[&t],"tag":t})).collect();
            let out = run(&tx, &json!({"op":"link_local","items":items}))?;
            tx.execute("INSERT INTO backend_effects(kind) VALUES ('verdict')", [])?;
            names(&tx, None)?;
            (String::new(), Some(out["ids"].clone().into()), 3)
        }
        "/api/works" => {
            v["op"] = json!("edit_work");
            v["ident"] = v["id"].clone();
            v["id"] = Value::Null;
            v["lang"] = json!(crate::character_labels::lang_of(v["name"].as_str().unwrap_or("")));
            if v["action"] == "from_tag" {
                let t = tag(&v["tag"])?;
                v["readable"] = json!(crate::names_view::readable(&t, false));
                v["tag"] = json!(t);
            }
            let id = run(&tx, &v)?["id"].as_str().expect("validated internal text field").to_owned();
            (id, None, 4)
        }
        _ => {
            v["op"] = json!(match path {
                "/api/kind" => "set_kind",
                "/api/character/mascot" => "set_mascot",
                _ => return invalid("接口不存在"),
            });
            if path == "/api/kind" {
                let (kind, artist) = oc(&v)?;
                v["kind"] = json!(kind);
                v["artist"] = json!(artist);
            }
            let out = run(&tx, &v)?;
            (String::new(), Some(out.into()), 0)
        }
    };
    crate::schema::commit_writing(tx)?;
    http_read::register_names(c)?;
    let (id, value, mode) = response;
    match mode {
        1 => {
            if let Some(v) = value {
                http_read::display(c, v)
            } else {
                shown(c, &id)
            }
        }
        2 => Ok(Json::object([("undo", last_action(c)?), ("id", id.into())])),
        3 => Ok(Json::object([("undo", last_action(c)?), ("ids", value.expect("app_profiles internal invariant"))])),
        4 => {
            let Json::Array(rows) = http_read::works(c, "")? else { unreachable!() };
            rows.into_iter()
                .find(|r| r.get("id").is_some_and(|v| v.text() == id))
                .ok_or_else(|| Error::Invalid("作品不存在".into()))
        }
        _ => Ok(value.expect("app_profiles internal invariant")),
    }
}
