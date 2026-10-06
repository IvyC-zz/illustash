//! Native application POST wrappers: target selection and history remain inside the command transaction.
use crate::{http::LibraryContext, http_json::Json, http_read, store, Error, Result};
use rusqlite::{
    params, params_from_iter, types::Value as SqlValue, Connection, OpenFlags, OptionalExtension, TransactionBehavior,
};
use serde_json::Value;
use std::collections::{HashMap, HashSet};

pub(crate) fn request(context: &LibraryContext, path: &str, body: &Json) -> Result<Json> {
    request_prepared(context, path, body, None)
}
pub(crate) fn request_prepared(
    context: &LibraryContext,
    path: &str,
    body: &Json,
    prepared: Option<&crate::app_profiles::Prepared>,
) -> Result<Json> {
    let mut c = crate::sqlite::open_with_flags(&context.db, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    crate::configure(&c)?;
    http_read::register_names(&c)?;
    let result = match path {
        "/api/tag" => tag(&mut c, context, body),
        "/api/undo" => undo(&mut c, body),
        "/api/artist/verdict" => artist(&mut c, body),
        "/api/change" | "/api/audit/answer" | "/api/ask/answer" | "/api/reassign" | "/api/sweep" => {
            initialized(&c)?;
            crate::app_characters::request(&mut c, context, path, body)
        }
        "/api/identity/decision" | "/api/identity/skip" | "/api/merge" => {
            initialized(&c)?;
            crate::app_identity::request(&mut c, path, body)
        }
        "/api/curate" => {
            initialized(&c)?;
            crate::app_curation::curate(&mut c, context, body)
        }
        "/api/fold" => {
            initialized(&c)?;
            crate::app_curation::fold(&mut c, context, body)
        }
        path if crate::app_profiles::ROUTES.contains(&path) => {
            crate::app_profiles::request(&mut c, body, path, prepared)
        }
        _ => Err(Error::Invalid("接口不存在".into())),
    }?;
    if matches!(
        path,
        "/api/undo" | "/api/identity/decision" | "/api/merge" | "/api/character/profile" | "/api/local-links"
    ) {
        crate::app_effects::drain(&context.db).map_err(|error| {
            eprintln!("人工操作已提交，verdict事件处理失败：{error}");
            error
        })?;
    }
    Ok(result)
}
pub(crate) fn core_error(error: impl Into<Error>) -> Error {
    // Only CLI refusal codes 2/3/4 become terms_core.Failed and Store.Invalid.
    // SQL/IO failures and the separate NeedId protocol are not business refusals.
    match error.into() {
        Error::Invalid(message) | Error::Conflict(message) | Error::Schema(message) => Error::Invalid(message),
        other => other,
    }
}
pub(crate) fn initialized(c: &Connection) -> Result<()> {
    crate::schema::require_current(c)?;
    let ready: bool = c.query_row("SELECT count(*)=4 FROM meta WHERE key IN ('terms_artists','terms_labels','terms_characters','terms_names') AND value='core'", [], |r| r.get(0))?;
    if !ready {
        return Err(Error::Conflict("图库正在切换到词条核心，完成前只读".into()));
    }
    Ok(())
}
pub(crate) fn last_action(c: &Connection) -> Result<Json> {
    let rows =
        http_read::rows(c, "SELECT id,label,ts,before_json FROM actions WHERE undone=0 ORDER BY id DESC LIMIT 1", &[])?;
    let Some(row) = rows.into_iter().next() else {
        return Ok(None::<String>.into());
    };
    Ok(Json::object([
        ("id", row.get("id").expect("app_commands internal invariant").clone()),
        ("label", crate::http_app::action_label(c, &row)?.into()),
        ("ts", row.get("ts").expect("app_commands internal invariant").clone()),
    ]))
}
fn string(value: Option<&Json>) -> Option<&str> {
    match value {
        Some(Json::Scalar(Value::String(s))) => Some(s),
        _ => None,
    }
}
fn is_null(value: Option<&Json>) -> bool {
    matches!(value, None | Some(Json::Scalar(Value::Null)))
}
fn strings(value: &Json, message: &str) -> Result<Vec<String>> {
    match value {
        Json::Array(values) => values
            .iter()
            .map(|v| string(Some(v)).map(str::to_owned).ok_or_else(|| Error::Invalid(message.into())))
            .collect(),
        _ => Err(Error::Invalid(message.into())),
    }
}
pub(crate) fn selected(c: &Connection, context: &LibraryContext, body: &Json) -> Result<Vec<String>> {
    selected_scope(c, context, body, true)
}
pub(crate) fn selected_scope(
    c: &Connection,
    context: &LibraryContext,
    body: &Json,
    copies: bool,
) -> Result<Vec<String>> {
    let exclude = if is_null(body.get("exclude")) {
        Vec::new()
    } else {
        strings(body.get("exclude").expect("app_commands internal invariant"), "排除列表不正确")?
    };
    let mut ids = if !is_null(body.get("filter")) {
        if !is_null(body.get("ids")) {
            return Err(Error::Invalid("图片列表与筛选条件只能使用一种".into()));
        }
        let Some(Json::Object(fields)) = body.get("filter") else {
            return Err(Error::Invalid("筛选参数不正确".into()));
        };
        let filters: HashMap<String, String> = fields
            .iter()
            .map(|(k, v)| {
                string(Some(v))
                    .map(|v| (k.clone(), v.to_owned()))
                    .ok_or_else(|| Error::Invalid("筛选参数不正确".into()))
            })
            .collect::<Result<_>>()?;
        let (where_, args, _) = crate::http_browse::selection(c, context, &filters)?;
        let excluded: HashSet<_> = exclude.iter().collect();
        c.prepare(&format!("SELECT a.uuid FROM assets a WHERE {where_} ORDER BY a.uuid"))?
            .query_map(params_from_iter(&args), |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .filter(|id| !excluded.contains(id))
            .collect()
    } else {
        if !exclude.is_empty() {
            return Err(Error::Invalid("排除列表仅支持按筛选条件选择".into()));
        }
        strings(body.get("ids").unwrap_or(&Json::Scalar(Value::Null)), "请选择图片")?
    };
    if ids.is_empty() {
        return Err(Error::Invalid("请选择图片".into()));
    }
    if ids.len() > 500 && !matches!(body.get("confirm"), Some(Json::Scalar(Value::Bool(true)))) {
        return Err(Error::Invalid("超过 500 张，请确认数量并携带 confirm:true；整批可撤销".into()));
    }
    let mut seen = HashSet::new();
    ids.retain(|id| seen.insert(id.clone()));
    for id in &ids {
        let exists: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM assets WHERE uuid=?)", [id], |r| r.get(0))?;
        if !exists {
            return Err(Error::Invalid("所选图片不存在，请刷新".into()));
        }
    }
    if copies {
        for id in ids.clone() {
            for copy in http_read::strings(c,"SELECT y.uuid FROM duplicates d JOIN duplicates y ON y.grp=d.grp AND y.uuid<>d.uuid WHERE d.uuid=? ORDER BY y.pos",&[&id])? {
            if seen.insert(copy.clone()) { ids.push(copy); }
        }
        }
    }
    Ok(ids)
}
fn tag(c: &mut Connection, context: &LibraryContext, body: &Json) -> Result<Json> {
    initialized(c)?;
    let name = string(body.get("tag")).ok_or_else(|| Error::Invalid("标签应为 1–60 个字符".into()))?;
    let trimmed = http_read::trim(name);
    if trimmed.is_empty() || trimmed.chars().count() > 60 {
        return Err(Error::Invalid("标签应为 1–60 个字符".into()));
    }
    let name = trimmed.split(http_read::whitespace).filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" ");
    let add = !matches!(body.get("add"), Some(Json::Scalar(Value::Bool(false))));
    let tx = c.transaction_with_behavior(TransactionBehavior::Immediate).map_err(core_error)?;
    let ids = selected(&tx, context, body)?;
    let encoded = serde_json::to_string(&ids).map_err(|e| Error::Invalid(e.to_string()))?;
    let groups: HashMap<String, Option<String>> = tx
        .prepare("SELECT uuid,grp FROM duplicates WHERE uuid IN (SELECT value FROM json_each(?))")?
        .query_map([encoded], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let n =
        ids.iter().map(|id| groups.get(id).cloned().unwrap_or_else(|| Some(id.clone()))).collect::<HashSet<_>>().len();
    let label =
        if add { format!("已给 {n} 张加上标签 {name}") } else { format!("已从 {n} 张去掉标签 {name}") };
    // The existing CLI rejects the expanded batch before looking up/creating a concept.
    if ids.len() > 5000 {
        return Err(Error::Invalid("一次 1–5000 张图".into()));
    }
    let term = crate::legacy_labels::concept_by_name(&tx, &name, add.then_some("user"))
        .map_err(core_error)?
        .ok_or_else(|| Error::Invalid(format!("没有标签「{name}」")))?;
    let mut targets = Vec::new();
    for id in &ids {
        if let Some(asset) = store::library_asset(&tx, id).map_err(core_error)? {
            targets.push(store::VerdictTarget::Image { asset, term });
        }
    }
    if targets.is_empty() {
        return Err(Error::Invalid("图片不存在".into()));
    }
    store::judge_in(&tx, &targets, if add { store::Verdict::Yes } else { store::Verdict::Clear }, "user", "", &label)
        .map_err(core_error)?;
    tx.commit().map_err(core_error)?;
    http_read::register_names(c)?;
    let action = last_action(c)?;
    Ok(Json::object([("undo", action)]))
}
fn artist(c: &mut Connection, body: &Json) -> Result<Json> {
    initialized(c)?;
    let verdict = match body.get("verdict") {
        None | Some(Json::Scalar(Value::Null)) => store::Verdict::Clear,
        Some(Json::Scalar(Value::String(v))) if v == "yes" => store::Verdict::Yes,
        Some(Json::Scalar(Value::String(v))) if v == "no" => store::Verdict::No,
        _ => return Err(Error::Invalid("画师结论只能是「是」或「不是」".into())),
    };
    let artist = string(body.get("artist"))
        .filter(|s| (1..=80).contains(&s.chars().count()))
        .ok_or_else(|| Error::Invalid("画师不正确".into()))?;
    let ids = strings(body.get("ids").unwrap_or(&Json::Scalar(Value::Null)), "图片不正确")?;
    if !(1..=5000).contains(&ids.len()) {
        return Err(Error::Invalid("图片不正确".into()));
    }
    let name = string(body.get("name")).filter(|s| !http_read::trim(s).is_empty()).unwrap_or("该画师");
    let tx = c.transaction_with_behavior(TransactionBehavior::Immediate).map_err(core_error)?;
    // This endpoint selects existing UUIDs directly, without the tag/curate copy expansion.
    let placeholders = vec!["?"; ids.len()].join(",");
    let ids = tx
        .prepare(&format!("SELECT uuid FROM assets WHERE uuid IN ({placeholders})"))?
        .query_map(params_from_iter(&ids), |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if ids.is_empty() {
        return Err(Error::Invalid("图片不存在".into()));
    }
    let n = ids.len();
    let label = match verdict {
        store::Verdict::Yes => format!("已确认 {n} 张是 {name} 画的"),
        store::Verdict::No => format!("已标记 {n} 张不是 {name} 画的"),
        store::Verdict::Clear => format!("已清除 {n} 张对 {name} 的判断"),
    };
    // Existing CLI accepts any existing term key here; retain that public contract.
    let (term, _) = store::term_by_key(&tx, artist)
        .map_err(core_error)?
        .ok_or_else(|| Error::Invalid(format!("词条不存在：{artist}")))?;
    let mut targets = Vec::new();
    for id in &ids {
        if let Some(asset) = store::library_asset(&tx, id).map_err(core_error)? {
            targets.push(store::VerdictTarget::Image { asset, term });
        }
    }
    store::judge_in(&tx, &targets, verdict, "user", "", &label).map_err(core_error)?;
    tx.commit().map_err(core_error)?;
    http_read::register_names(c)?;
    Ok(Json::object([("undo", last_action(c)?)]))
}
fn sqlite_id(value: Option<&Json>) -> Result<SqlValue> {
    Ok(match value {
        None | Some(Json::Scalar(Value::Null)) => SqlValue::Null,
        Some(Json::Scalar(Value::String(v))) => SqlValue::Text(v.clone()),
        Some(Json::Scalar(Value::Bool(v))) => SqlValue::Integer(i64::from(*v)),
        Some(Json::Scalar(Value::Number(v))) if v.is_i64() => {
            SqlValue::Integer(v.as_i64().expect("validated internal numeric field"))
        }
        Some(Json::Scalar(Value::Number(v))) if v.is_f64() => {
            SqlValue::Real(v.as_f64().expect("validated internal numeric field"))
        }
        Some(Json::Float(v)) => SqlValue::Real(*v),
        Some(Json::Integer(v)) => SqlValue::Integer(
            v.parse()
                .map_err(|_| Error::Io(std::io::Error::other("Python int too large to convert to SQLite INTEGER")))?,
        ),
        _ => return Err(Error::Io(std::io::Error::other("unsupported SQLite parameter"))),
    })
}
fn integer_id(value: &Json) -> Option<i64> {
    match value {
        Json::Scalar(Value::Number(n)) => n.as_i64(),
        Json::Integer(n) => n.parse().ok(),
        _ => None,
    }
}
fn undo(c: &mut Connection, body: &Json) -> Result<Json> {
    initialized(c)?;
    let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let input = body.get("id");
    let bound = sqlite_id(input)?;
    let row: Option<(i64, String)> = tx
        .query_row("SELECT id,before_json FROM actions WHERE id=? AND undone=0", [&bound], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .optional()?;
    let mut character = false;
    if let Some((id, before)) = row {
        let parsed: Value = serde_json::from_str(&before).map_err(|e| Error::Invalid(e.to_string()))?;
        let owned = crate::character_commands::owns(&parsed).map_err(core_error)?.is_some();
        let terms =
            parsed.as_array().is_some_and(|items| store::owns(items, store::Handover { artists: true, labels: true }));
        if owned {
            let strict = input.and_then(integer_id).ok_or_else(|| Error::Invalid("步骤 ID 不正确".into()))?;
            let (_, owned) = store::undo_in(&tx, Some(strict)).map_err(core_error)?;
            character = owned;
        } else if terms {
            let expected = match input {
                Some(Json::Scalar(Value::String(v))) => http_read::python_int(v).map_err(Error::Invalid)?,
                _ => id,
            };
            store::undo_in(&tx, Some(expected)).map_err(core_error)?;
        } else {
            restore_application(&tx, &bound, &parsed)?;
        }
    } else {
        return Err(Error::Invalid(store::HISTORY_CHANGED.into()));
    };
    if character {
        tx.execute("INSERT INTO backend_effects(kind) VALUES ('verdict')", []).map_err(core_error)?;
    }
    if character {
        crate::schema::commit_writing(tx).map_err(core_error)?;
    } else {
        tx.commit()?;
    }
    http_read::register_names(c)?;
    let action = last_action(c)?;
    Ok(Json::object([("undo", action)]))
}
fn restore_application(c: &Connection, bound: &SqlValue, before: &Value) -> Result<()> {
    let last: Option<i64> = c.query_row("SELECT max(id) FROM actions WHERE undone=0", [], |r| r.get(0))?;
    let same = match (last, bound) {
        (Some(a), SqlValue::Integer(b)) => a == *b,
        (Some(a), SqlValue::Real(b)) => (a as f64) == *b,
        _ => false,
    };
    if !same {
        return Err(Error::Invalid(store::HISTORY_CHANGED.into()));
    }
    let id = last.expect("app_commands internal invariant");
    c.execute("DELETE FROM asked WHERE action_id=?", [id])?;
    let items = before.as_array().ok_or_else(|| Error::Io(std::io::Error::other("未知历史")))?;
    let mut touched = Vec::new();
    for item in items {
        if let Some(skip) = item.get("identity_skip") {
            if skip.is_null() {
                c.execute("DELETE FROM prefs WHERE key='identity_skipped'", [])?;
            } else {
                c.execute(
                    "INSERT OR REPLACE INTO prefs VALUES ('identity_skipped',?)",
                    [skip.as_str().ok_or_else(|| Error::Io(std::io::Error::other("未知历史")))?],
                )?;
            }
        } else if let Some(had) = item.get("curation") {
            let uuid = item["uuid"].as_str().ok_or_else(|| Error::Io(std::io::Error::other("未知历史")))?;
            c.execute("DELETE FROM curation WHERE uuid=?", [uuid])?;
            if !had.is_null() {
                c.execute(
                    "INSERT INTO curation VALUES (?,?,?,?)",
                    params![uuid, had["status"].as_str(), had["origin"].as_str(), had["created_at"].as_str()],
                )?;
            }
            touched.push(uuid);
        } else {
            return Err(Error::Io(std::io::Error::other("未知历史")));
        }
    }
    // Bulk refresh preserves the original query's insertion/rowid order, including reversed targets.
    crate::app_curation::refresh_excluded(c, &touched.into_iter().map(str::to_owned).collect::<Vec<_>>())?;
    c.execute("DELETE FROM reviews WHERE action_id=?", [id])?;
    c.execute("UPDATE actions SET undone=1 WHERE id=?", [id])?;
    c.execute("INSERT INTO backend_effects(kind) VALUES ('verdict')", [])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Fixture {
        root: PathBuf,
        context: LibraryContext,
    }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "illustash-native-command-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&root).unwrap();
            let db = root.join("library.sqlite");
            let mut c = crate::open(&db).unwrap();
            crate::migrate(&mut c).unwrap();
            c.execute_batch("CREATE TABLE assets(uuid TEXT PRIMARY KEY,added_at TEXT); INSERT INTO assets VALUES ('a','2026-10-01'),('copy','2026-10-01'),('b','2026-09-30'); CREATE TABLE duplicates(uuid TEXT PRIMARY KEY,grp TEXT,pos INTEGER,hidden INTEGER); INSERT INTO duplicates VALUES ('a','g',0,0),('copy','g',1,1); CREATE TABLE excluded(uuid TEXT PRIMARY KEY,reason TEXT,origin TEXT); CREATE TABLE curation(uuid TEXT PRIMARY KEY,status TEXT,origin TEXT,created_at TEXT DEFAULT CURRENT_TIMESTAMP); CREATE TABLE auto_rules(uuid TEXT PRIMARY KEY,kind TEXT,reason TEXT); CREATE TABLE prefs(key TEXT PRIMARY KEY,value TEXT); CREATE TABLE asked(uuid TEXT,action_id INTEGER); CREATE TABLE reviews(entity TEXT,action_id INTEGER); CREATE TABLE backend_effects(id INTEGER PRIMARY KEY,kind TEXT,status TEXT NOT NULL DEFAULT 'pending',error TEXT NOT NULL DEFAULT '',created_at TEXT DEFAULT CURRENT_TIMESTAMP); INSERT INTO meta VALUES ('terms_artists','core'),('terms_labels','core'),('terms_characters','core'),('terms_names','core');").unwrap();
            c.execute_batch("CREATE TABLE entities(id TEXT PRIMARY KEY); CREATE TABLE links(entity_id TEXT); CREATE TABLE names(entity_id TEXT); CREATE TABLE display_names(kind TEXT,key TEXT); INSERT INTO meta VALUES ('names_version','0'),('names_projected','0');").unwrap();
            c.execute_batch(crate::legacy_labels::VIEWS).unwrap();
            let context = LibraryContext {
                db,
                generation: "synthetic".into(),
                library: Some(root.clone()),
                tags: root.join("unused.csv"),
                source: None,
                token: Some("synthetic".into()),
                read_only: None,
            };
            Self { root, context }
        }
        fn call(&self, path: &str, text: &str) -> Result<Json> {
            request(&self.context, path, &crate::http_json::parse_request(text).unwrap())
        }
        fn count(&self, sql: &str) -> i64 {
            crate::sqlite::open(&self.context.db).unwrap().query_row(sql, [], |r| r.get(0)).unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
    #[test]
    fn artist_uses_existing_targets_without_expanding_or_creating_terms() {
        let f = Fixture::new();
        let c = crate::sqlite::open(&f.context.db).unwrap();
        store::create_term(&c, crate::Kind::Artist, Some("artist"), "test").unwrap();
        let out = f
            .call(
                "/api/artist/verdict",
                r#"{"ids":["a","missing","a"],"artist":"artist","verdict":"yes","name":"  画师  "}"#,
            )
            .unwrap();
        assert_eq!(out.get("undo").unwrap().get("label").unwrap().dump(), r#""已确认 1 张是   画师   画的""#);
        assert_eq!(f.count("SELECT count(*) FROM term_assets"), 1);
        let id = out.get("undo").unwrap().get("id").unwrap().dump();
        f.call("/api/undo", &format!("{{\"id\":{id}}}")).unwrap();
        let before = f.count("SELECT count(*) FROM actions");
        let err = f.call("/api/artist/verdict", r#"{"ids":["a"],"artist":"missing"}"#).unwrap_err();
        assert!(matches!(err, Error::Invalid(s) if s == "词条不存在：missing"));
        assert_eq!(f.count("SELECT count(*) FROM actions"), before);
    }
    #[test]
    fn artist_validation_precedence_and_batch_limits_do_not_write() {
        let f = Fixture::new();
        for (body, expected) in [
            (r#"{"verdict":true}"#, "画师结论只能是「是」或「不是」"),
            (r#"{}"#, "画师不正确"),
            (r#"{"artist":"artist","ids":[]}"#, "图片不正确"),
            (r#"{"artist":"artist","ids":["missing"]}"#, "图片不存在"),
        ] {
            assert!(matches!(f.call("/api/artist/verdict", body), Err(Error::Invalid(s)) if s == expected));
        }
        assert_eq!(f.count("SELECT count(*) FROM actions"), 0);
    }
    #[test]
    fn only_core_business_refusals_become_invalid() {
        for error in
            [Error::Invalid("invalid".into()), Error::Conflict("conflict".into()), Error::Schema("schema".into())]
        {
            assert!(matches!(core_error(error), Error::Invalid(_)));
        }
        assert!(matches!(core_error(rusqlite::Error::InvalidQuery), Error::Sql(_)));
        assert!(matches!(core_error(Error::Io(std::io::Error::other("fault"))), Error::Io(_)));
        assert!(matches!(core_error(Error::NeedId("/id".into())), Error::NeedId(_)));
    }
    #[test]
    fn tag_expands_copies_counts_artworks_and_undoes_add_and_remove() {
        let f = Fixture::new();
        let out = f.call("/api/tag", r#"{"ids":["a","a","b"],"tag":"  合成\u001c标签  "}"#).unwrap();
        let action = out.get("undo").unwrap();
        assert_eq!(action.get("label").unwrap().text(), "已给 2 张加上标签 合成 标签");
        assert_eq!(f.count("SELECT count(*) FROM asset_tags"), 3);
        assert_eq!(f.count("SELECT count(*) FROM backend_effects"), 0);
        f.call("/api/tag", r#"{"filter":{"utag":"合成 标签"},"exclude":["b"],"tag":"合成 标签","add":false}"#).unwrap();
        assert_eq!(f.count("SELECT count(*) FROM asset_tags"), 1);
        f.call("/api/undo", r#"{"id":2}"#).unwrap();
        assert_eq!(f.count("SELECT count(*) FROM asset_tags"), 3);
        let out = f.call("/api/undo", r#"{"id":"1"}"#).unwrap();
        assert_eq!(out.dump(), "{\"undo\": null}");
        assert_eq!(f.count("SELECT count(*) FROM asset_tags"), 0);
        assert_eq!(f.count("SELECT count(*) FROM backend_effects"), 0);
    }
    #[test]
    fn parameter_order_selection_and_confirmation_do_not_write() {
        let f = Fixture::new();
        for (text, message) in [
            (r#"{"tag":"","exclude":false}"#, "标签应为 1–60 个字符"),
            (r#"{"tag":"x","ids":["a"],"exclude":"b"}"#, "排除列表不正确"),
            (r#"{"tag":"x","ids":["a"],"filter":{}}"#, "图片列表与筛选条件只能使用一种"),
            (r#"{"tag":"x","ids":["missing"]}"#, "所选图片不存在，请刷新"),
            (r#"{"tag":"x","filter":{"invalid":"x"}}"#, "筛选参数不正确"),
        ] {
            assert!(matches!(f.call("/api/tag",text),Err(Error::Invalid(m)) if m==message));
        }
        let ids = vec!["a"; 501];
        let text = serde_json::json!({"tag":"x","ids":ids,"confirm":1}).to_string();
        assert!(matches!(f.call("/api/tag",&text),Err(Error::Invalid(m)) if m.starts_with("超过 500")));
        assert_eq!(f.count("SELECT count(*) FROM terms"), 0);
        assert_eq!(f.count("SELECT count(*) FROM actions"), 0);
    }
    #[test]
    fn failed_judge_rolls_back_new_concept_asset_registration_and_history() {
        let f = Fixture::new();
        crate::sqlite::open(&f.context.db).unwrap().execute_batch("CREATE TRIGGER fail_judge BEFORE INSERT ON term_verdicts BEGIN SELECT RAISE(ABORT,'synthetic judge failure'); END;").unwrap();
        assert!(matches!(f.call("/api/tag", r#"{"tag":"new","ids":["a"]}"#), Err(Error::Sql(_))));
        for table in ["terms", "term_names", "term_assets", "term_verdicts", "actions", "term_projection"] {
            assert_eq!(f.count(&format!("SELECT count(*) FROM {table}")), 0, "{table}");
        }
    }
    #[test]
    fn cross_type_history_restores_application_and_character_events_are_done() {
        let f = Fixture::new();
        f.call("/api/tag", r#"{"tag":"x","ids":["a"]}"#).unwrap();
        let c = crate::sqlite::open(&f.context.db).unwrap();
        c.execute_batch("INSERT INTO actions(label,before_json) VALUES ('已排除 1 张','[{\"uuid\":\"b\",\"curation\":null}]'); INSERT INTO curation VALUES ('b','excluded','review','synthetic'); INSERT INTO excluded VALUES ('b','manual','review'); INSERT INTO actions(label,before_json) VALUES ('角色历史','[{\"terms\":{\"v\":2,\"type\":\"change\",\"before\":[]}}]');").unwrap();
        assert!(matches!(f.call("/api/undo",r#"{"id":3.0}"#),Err(Error::Invalid(m)) if m=="步骤 ID 不正确"));
        f.call("/api/undo", r#"{"id":3}"#).unwrap();
        assert_eq!(f.count("SELECT count(*) FROM backend_effects WHERE status='done'"), 1);
        f.call("/api/undo", r#"{"id":2}"#).unwrap();
        assert_eq!(f.count("SELECT count(*) FROM excluded"), 0);
        assert_eq!(f.count("SELECT count(*) FROM backend_effects WHERE status='done'"), 2);
        f.call("/api/undo", r#"{"id":true}"#).unwrap();
        assert_eq!(f.count("SELECT count(*) FROM asset_tags"), 0);
    }
    #[test]
    fn unknown_history_rolls_back_and_does_not_skip_latest_step() {
        let f = Fixture::new();
        f.call("/api/tag", r#"{"tag":"x","ids":["a"]}"#).unwrap();
        crate::sqlite::open(&f.context.db)
            .unwrap()
            .execute_batch("INSERT INTO actions(label,before_json) VALUES ('unknown','[{\"unknown\":true}]');")
            .unwrap();
        assert!(f.call("/api/undo", r#"{"id":2}"#).is_err());
        assert_eq!(f.count("SELECT count(*) FROM actions WHERE undone=0"), 2);
        assert!(matches!(f.call("/api/undo",r#"{"id":1}"#),Err(Error::Invalid(m)) if m==store::HISTORY_CHANGED));
        assert_eq!(f.count("SELECT count(*) FROM backend_effects"), 0);
    }
}
