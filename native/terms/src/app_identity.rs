//! HTTP identity wrappers share the existing character transaction and undo protocol.
use crate::{app_commands, character_json, http_json::Json, http_read, Error, Result};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde_json::{json, Value};

fn argument(body: &Json, key: &str) -> Result<Value> {
    body.get(key)
        .map(|value| crate::strict_json::parse(&value.dump()).map_err(Error::Invalid))
        .unwrap_or(Ok(Value::Null))
}

pub(crate) fn request(c: &mut Connection, path: &str, body: &Json) -> Result<Json> {
    let command = match path {
        "/api/identity/skip" => return skip(c, body),
        "/api/identity/decision" => {
            // Store checks Python's exact int type before the core checks policy/endpoints.
            let relation = argument(body, "relation_id")?;
            if !relation.is_null() && relation.as_i64().is_none_or(|id| id <= 0) {
                return Err(Error::Invalid("关系 ID 不正确".into()));
            }
            json!({"op":"decide", "a":argument(body,"a")?, "b":argument(body,"b")?,
                "kind":argument(body,"kind")?, "relation_id":relation,
                "status":argument(body,"status")?, "reason":argument(body,"reason")?,
                "evidence":argument(body,"evidence")?})
        }
        "/api/identity/uncertain" => {
            let options = argument(body, "options")?;
            let reason =
                if body.get("reason").is_none() { json!("人工暂不判断") } else { argument(body, "reason")? };
            // The old wrapper validates these before passing selection to the core.
            let items = options.as_array().ok_or_else(|| Error::Invalid("未定状态需要 2–20 个不同身份候选".into()))?;
            let unique: std::collections::HashSet<_> = items.iter().filter_map(Value::as_str).collect();
            if !(2..=20).contains(&items.len()) || unique.len() != items.len() {
                return Err(Error::Invalid("未定状态需要 2–20 个不同身份候选".into()));
            }
            if reason.as_str().is_none_or(|s| http_read::trim(s).is_empty()) {
                return Err(Error::Invalid("请保存未定理由".into()));
            }
            json!({"op":"uncertain", "asked":argument(body,"ids")?, "options":options, "reason":reason})
        }
        "/api/merge" => json!({"op":"merge", "source":argument(body,"source")?, "target":argument(body,"target")?}),
        _ => return Err(Error::Invalid("接口不存在".into())),
    };
    let tx = c.transaction_with_behavior(TransactionBehavior::Immediate).map_err(app_commands::core_error)?;
    crate::schema::begin_writing(&tx).map_err(app_commands::core_error)?;
    crate::character_commands::command(&tx, &command).map_err(app_commands::core_error)?;
    tx.execute("INSERT INTO backend_effects(kind) VALUES ('verdict')", [])?;
    crate::schema::commit_writing(tx).map_err(app_commands::core_error)?;
    http_read::register_names(c)?;
    Ok(Json::object([("undo", app_commands::last_action(c)?)]))
}

fn skip(c: &mut Connection, body: &Json) -> Result<Json> {
    let a = argument(body, "a")?;
    let b = argument(body, "b")?;
    let (a, b) = match (a.as_str(), b.as_str()) {
        (Some(a), Some(b)) if !a.is_empty() && !b.is_empty() && a != b => (a, b),
        _ => return Err(Error::Invalid("请选择两个角色".into())),
    };
    // Queue skip deliberately accepts arbitrary distinct keys, without role/policy resolution.
    let key = if a < b { format!("{a}|{b}") } else { format!("{b}|{a}") };
    let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let before: Option<String> =
        tx.query_row("SELECT value FROM prefs WHERE key='identity_skipped'", [], |r| r.get(0)).optional()?;
    let mut skipped: Vec<String> = serde_json::from_str(before.as_deref().filter(|s| !s.is_empty()).unwrap_or("[]"))
        .map_err(|e| Error::Invalid(e.to_string()))?;
    if !skipped.contains(&key) {
        skipped.push(key);
    }
    tx.execute(
        "INSERT OR REPLACE INTO prefs VALUES ('identity_skipped',?)",
        [character_json::dump(&json!(skipped), true)],
    )?;
    tx.execute(
        "INSERT INTO actions(label,before_json) VALUES ('已暂不判断一对角色',?)",
        params![character_json::dump(&json!([{"identity_skip":before}]), true)],
    )?;
    tx.commit()?;
    Ok(Json::object([("undo", app_commands::last_action(c)?)]))
}
