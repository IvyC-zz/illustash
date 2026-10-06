//! HTTP parameter/selection adapter for character verdicts; business rules and history use the shared core.
use crate::{app_commands, character_commands, http::LibraryContext, http_json::Json, http_read, Error, Result};
use rusqlite::{Connection, TransactionBehavior};
use serde_json::{json, Value};

pub(crate) fn request(c: &mut Connection, context: &LibraryContext, path: &str, body: &Json) -> Result<Json> {
    // Only interpret consumed fields. Python accepts unused arbitrary-size integers and
    // non-finite floats; serializing the entire body through serde_json would refuse them.
    let mut input = json!({});
    for key in ["entity", "action", "entities", "switch", "yes", "no", "later", "old", "new", "id"] {
        input[key] = argument(body.get(key));
    }
    if let Some(kind) = body.get("kind") {
        input["kind"] = argument(Some(kind));
    }
    let op = match path {
        "/api/change" => "change",
        "/api/audit/answer" => "answer_audit",
        "/api/ask/answer" => "answer",
        "/api/reassign" => "reassign",
        "/api/sweep" => "sweep",
        _ => return Err(Error::Invalid("接口不存在".into())),
    };
    let mut args = json!({"op":op,"entity":input["entity"],"action":input["action"],
        "entities":input["entities"],"switch":input["switch"],"confirm":true});
    if matches!(op, "answer" | "answer_audit") {
        for key in ["yes", "no", "later"] {
            args[key] = if character_commands::truthy(&input[key]) { input[key].clone() } else { json!([]) };
        }
        args["kind"] = input.get("kind").cloned().unwrap_or(json!("seed"));
    }
    if op == "reassign" {
        args["old_entity"] = input["old"].clone();
        args["new_entity"] = input["new"].clone();
    }
    let tx = c.transaction_with_behavior(TransactionBehavior::Immediate).map_err(app_commands::core_error)?;
    // Match the CLI's guard boundary, including its fast verdict operations.
    if matches!(op, "change" | "sweep" | "answer") {
        tx.execute("INSERT OR REPLACE INTO meta(key,value) VALUES (?1,'1')", [crate::schema::WRITER_KEY])?;
    } else {
        crate::schema::begin_writing(&tx).map_err(app_commands::core_error)?;
    }
    if matches!(op, "change" | "reassign") {
        let selection = body.clone();
        // Defer business selection refusals to the core so its entity/argument check order is preserved.
        match app_commands::selected_scope(&tx, context, &selection, false) {
            Ok(ids) => args["asked"] = json!(ids),
            Err(Error::Invalid(message)) => args["asked_refused"] = json!(message),
            Err(e) => return Err(e),
        }
    }
    let out = character_commands::command(&tx, &args).map_err(app_commands::core_error)?;
    tx.execute("INSERT INTO backend_effects(kind) VALUES ('verdict')", [])?;
    crate::schema::commit_writing(tx).map_err(app_commands::core_error)?;
    http_read::register_names(c)?;
    // A later-only answer has no action, even if an earlier undoable action exists.
    let undo =
        if op == "answer" && out["action"].is_null() { Json::from(Value::Null) } else { app_commands::last_action(c)? };
    crate::app_effects::drain(&context.db)?;
    Ok(Json::object([("undo", undo)]))
}

// Non-representable numbers are truthy, non-string arguments. The core refuses them
// wherever a string/list is required, while HTTP's `value or []` retains Python truthiness.
fn argument(value: Option<&Json>) -> Value {
    match value {
        None => Value::Null,
        Some(Json::Scalar(v)) => v.clone(),
        Some(Json::Float(v)) if v.is_finite() => json!(v),
        Some(Json::Float(_)) => json!(true),
        Some(Json::Integer(v)) => serde_json::from_str(v).unwrap_or(json!(true)),
        Some(Json::Array(values)) => Value::Array(values.iter().map(|v| argument(Some(v))).collect()),
        Some(Json::Object(fields)) => {
            Value::Object(fields.iter().map(|(k, v)| (k.clone(), argument(Some(v)))).collect())
        }
    }
}
