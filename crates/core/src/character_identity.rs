//! S3 relation commands: append-only core decisions, legacy-shaped compatibility
//! rows, full graph validation, and undo audit. The caller owns the transaction.
use crate::{character_graph, character_json, legacy_characters, Error, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

fn invalid(s: &str) -> Error {
    Error::Invalid(s.into())
}
fn text<'a>(v: &'a Value, message: &str) -> Result<&'a str> {
    v.as_str().ok_or_else(|| invalid(message))
}
fn relation_core(c: &Connection, id: i64) -> Result<i64> {
    c.query_row("SELECT core_id FROM term_legacy_ids WHERE kind='relation' AND legacy_id=?", [id], |r| r.get(0))
        .optional()?
        .ok_or_else(|| invalid("关系候选不存在"))
}
fn next_id(c: &Connection, kind: &str) -> Result<i64> {
    Ok(c.query_row("SELECT coalesce(max(legacy_id),0)+1 FROM term_legacy_ids WHERE kind=?", [kind], |r| r.get(0))?)
}
fn semantic(v: &Value) -> Value {
    match v {
        Value::Object(o) => Value::Object(
            o.iter()
                .filter(|(k, _)| !matches!(k.as_str(), "response_ids" | "response_id" | "fetched_at"))
                .map(|(k, v)| (k.clone(), semantic(v)))
                .collect(),
        ),
        Value::Array(a) => Value::Array(a.iter().map(semantic).collect()),
        _ => v.clone(),
    }
}

pub fn propose(c: &Connection, a: &str, b: &str, kind: &str, evidence: Option<&Value>, source: &str) -> Result<i64> {
    if a == b || !matches!(kind, "duplicate_of" | "form_of" | "different_from") {
        return Err(invalid("无效身份关系"));
    }
    let count: i64 =
        c.query_row("SELECT count(*) FROM entities WHERE id IN (?,?) AND kind<>'work'", params![a, b], |r| r.get(0))?;
    if count != 2 {
        return Err(invalid("关系端点不存在"));
    }
    let (a, b) = if kind == "different_from" && a > b { (b, a) } else { (a, b) };
    let existing: Option<i64> = if kind == "duplicate_of" {
        c.query_row(
            "SELECT id FROM identity_relations WHERE kind=?1 AND ((a=?2 AND b=?3) OR (a=?3 AND b=?2))",
            params![kind, a, b],
            |r| r.get(0),
        )
        .optional()?
    } else {
        c.query_row("SELECT id FROM identity_relations WHERE a=? AND b=? AND kind=?", params![a, b, kind], |r| r.get(0))
            .optional()?
    };
    let id = match existing {
        Some(id) => id,
        None => {
            let ta: i64 = c.query_row("SELECT id FROM terms WHERE key=?", [a], |r| r.get(0))?;
            let tb: i64 = c.query_row("SELECT id FROM terms WHERE key=?", [b], |r| r.get(0))?;
            use crate::schema::RelationKind;
            let (ta, tb, relation_kind) = match kind {
                "form_of" => (tb, ta, RelationKind::Contains),
                "duplicate_of" => (ta, tb, RelationKind::SameAs),
                _ => (ta, tb, RelationKind::DifferentFrom),
            };
            let core = crate::store::relation(c, ta, tb, relation_kind)?;
            let id = next_id(c, "relation")?;
            c.execute(
                "INSERT INTO term_legacy_ids(kind,legacy_id,core_id) VALUES ('relation',?,?)",
                params![id, core],
            )?;
            id
        }
    };
    if let Some(evidence) = evidence {
        let core = relation_core(c, id)?;
        let payload = character_json::dump(evidence, false);
        let fp = crate::strict_json::sha256_hex(
            format!("{source}\n{}", character_json::dump(&semantic(evidence), false)).as_bytes(),
        );
        let exists:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM term_evidence WHERE relation_id=? AND method='identity_evidence' AND version=?)",params![core,fp],|r|r.get(0))?;
        if !exists {
            let stamp: String = c.query_row("SELECT CURRENT_TIMESTAMP", [], |r| r.get(0))?;
            let data = json!({"legacy_fingerprint":fp,"payload":payload,"created_at":stamp});
            c.execute("INSERT INTO term_evidence(relation_id,source,method,version,data,fingerprint) VALUES (?,?,'identity_evidence',?,?,?)",params![core,source,fp,data.to_string(),legacy_characters::fingerprint16(&fp)])?;
        }
    }
    legacy_characters::project_relation(c, id)?;
    Ok(id)
}

/// None action is a non-user batch, not a fabricated undo-stack entry.
pub fn decide(
    c: &Connection,
    id: i64,
    status: &str,
    reason: &str,
    action: Option<i64>,
    batch_kind: &str,
) -> Result<Value> {
    if !matches!(status, "accepted" | "rejected" | "proposed") {
        return Err(invalid("请保存明确的关系决议及理由"));
    }
    let core = relation_core(c, id)?;
    let previous: String = c.query_row("SELECT status FROM identity_relations WHERE id=?", [id], |r| r.get(0))?;
    let reviewed: Vec<i64> = c
        .prepare("SELECT id FROM term_evidence WHERE relation_id=? AND method='identity_evidence' ORDER BY id")?
        .query_map([core], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let (batch, anchor) = match action {
        Some(a) => (None, a),
        None => {
            let anchor: i64 = c.query_row("SELECT coalesce(max(id),0) FROM actions", [], |r| r.get(0))?;
            c.execute(
                "INSERT INTO term_batches(kind,anchor,label) VALUES (?,?,?)",
                params![batch_kind, anchor, reason],
            )?;
            (Some(c.last_insert_rowid()), anchor)
        }
    };
    let verdict = match status {
        "accepted" => "yes",
        "rejected" => "no",
        _ => "clear",
    };
    c.execute(
        "INSERT INTO term_verdicts(action_id,batch_id,anchor,relation_id,verdict,origin,reason,reviewed,via)
        VALUES (?,?,?,?,?,?,?,?,?)",
        params![
            action,
            batch,
            anchor,
            core,
            verdict,
            if action.is_none() && batch_kind == "migration" { "legacy" } else { "user" },
            reason,
            json!(reviewed).to_string(),
            if batch_kind == "undo_audit" {
                "undo"
            } else if action.is_none() {
                "migration"
            } else {
                "review"
            }
        ],
    )?;
    let decision_core = c.last_insert_rowid();
    let decision = next_id(c, "decision")?;
    c.execute(
        "INSERT INTO term_legacy_ids(kind,legacy_id,core_id) VALUES ('decision',?,?)",
        params![decision, decision_core],
    )?;
    legacy_characters::project_relation(c, id)?;
    Ok(json!({"relation_id":id,"previous_status":previous,"decision_id":decision,"status":status}))
}

pub fn command(c: &Connection, args: &Value) -> Result<Value> {
    if !crate::character_projection::policy(c)? {
        return Err(invalid("请先完成两级身份迁移"));
    }
    let op = text(&args["op"], "请选择身份命令")?;
    match op {
        "rebuild" => character_graph::rebuild(c),
        "propose" => {
            let id = propose(
                c,
                text(&args["a"], "无效身份关系")?,
                text(&args["b"], "无效身份关系")?,
                text(&args["kind"], "无效身份关系")?,
                args.get("evidence").filter(|v| !v.is_null()),
                args.get("source").and_then(Value::as_str).unwrap_or("manual"),
            )?;
            Ok(json!({"relation_id":id}))
        }
        "decide" => {
            // 与 Store.decide_identity 的检查顺序一致（去掉预演后，同时有几处不对时报哪一条也要相同）：
            // 关系 id 的类型（Python 在调用前查）→ 策略（上面）→ 没给 id 时先提议（端点）→ 状态与理由 → 关系存在。
            let id = match args.get("relation_id").filter(|v| !v.is_null()) {
                Some(v) => v.as_i64().filter(|i| *i > 0).ok_or_else(|| invalid("关系 ID 不正确"))?,
                None => {
                    let fallback = json!({"manual_review": args.get("reason").cloned().unwrap_or(Value::Null)});
                    propose(
                        c,
                        text(&args["a"], "无效身份关系")?,
                        text(&args["b"], "无效身份关系")?,
                        text(&args["kind"], "无效身份关系")?,
                        Some(args.get("evidence").filter(|v| !v.is_null()).unwrap_or(&fallback)),
                        "manual",
                    )?
                }
            };
            // Python 的默认值是 proposed；给了但不是字符串（含 null）照旧拒绝。
            let status = match args.get("status") {
                None => "proposed",
                Some(Value::String(s)) => s.as_str(),
                Some(_) => return Err(invalid("请保存明确的关系决议及理由")),
            };
            let reason = args.get("reason").and_then(Value::as_str).unwrap_or("");
            if !matches!(status, "accepted" | "rejected" | "proposed") || reason.trim().is_empty() {
                return Err(invalid("请保存明确的关系决议及理由"));
            }
            c.execute("INSERT INTO actions(label,before_json) VALUES ('已保存身份关系决议','[]')", [])?;
            let action = c.last_insert_rowid();
            let result = decide(c, id, status, reason.trim(), Some(action), "migration")?;
            character_graph::rebuild(c)?;
            let before = json!([{"terms":{"v":2,"type":"identity_relation","before":[{"identity_relation":result}]}}]);
            c.execute("UPDATE actions SET before_json=? WHERE id=?", params![before.to_string(), action])?;
            Ok(json!({"action":action,"decision":result}))
        }
        "accept-many" => {
            character_graph::baseline(c)?;
            let ids = args["ids"].as_array().ok_or_else(|| invalid("关系 ID 应为数组"))?;
            let reason = text(&args["reason"], "请保存明确的关系决议及理由")?;
            let mut accepted = Vec::new();
            let mut skipped = Vec::new();
            for id in ids {
                let id = id.as_i64().ok_or_else(|| invalid("关系 ID 不正确"))?;
                let previous: Option<String> =
                    c.query_row("SELECT status FROM identity_relations WHERE id=?", [id], |r| r.get(0)).optional()?;
                if previous.as_deref() != Some("proposed") {
                    continue;
                }
                c.execute_batch("SAVEPOINT accept_relation")?;
                decide(c, id, "accepted", reason, None, "migration")?;
                match character_graph::graph(c) {
                    Ok(_) => {
                        c.execute_batch("RELEASE accept_relation")?;
                        accepted.push(id);
                    }
                    Err(Error::Invalid(reason)) => {
                        c.execute_batch("ROLLBACK TO accept_relation;RELEASE accept_relation")?;
                        skipped.push(json!({"relation_id":id,"reason":reason}));
                    }
                    Err(e) => return Err(e),
                }
            }
            if !accepted.is_empty() {
                character_graph::rebuild(c)?;
            }
            Ok(json!({"accepted":accepted,"skipped":skipped}))
        }
        "undo" => crate::character_commands::undo(c, args),
        _ => Err(invalid("未知身份命令")),
    }
}

/// 撤销一条人工关系决议（旧形状 `identity_relation`）：追加回到 previous_status 的撤销审计决议，
/// 记下当时关系上的全部证据，再重建图（= 旧版 `decide(previous)`）。调用方已把这一步标为撤销。
pub fn undo_relation(c: &Connection, r: &Value) -> Result<()> {
    if !keys(r, &["relation_id", "previous_status", "decision_id", "status"])
        || r["decision_id"].as_i64().is_none_or(|i| i <= 0)
        || !matches!(r["status"].as_str(), Some("accepted" | "rejected" | "proposed"))
        || !matches!(r["previous_status"].as_str(), Some("accepted" | "rejected" | "proposed"))
    {
        return Err(invalid("身份撤销快照不完整"));
    }
    let relation = r["relation_id"].as_i64().ok_or_else(|| invalid("身份撤销快照不完整"))?;
    decide(c, relation, text(&r["previous_status"], "身份撤销快照不完整")?, "撤销人工关系决议", None, "undo_audit")?;
    character_graph::rebuild(c)?;
    Ok(())
}

/// hybrid 下撤销激活前留下的破坏式合并：对应的 `duplicate_of` 退回 proposed（撤销审计），搬走的结论不回来。
pub fn undo_merge(c: &Connection, m: &Value) -> Result<()> {
    if !keys(
        m,
        &[
            "source",
            "target",
            "moved",
            "kept",
            "candidates",
            "target_prev",
            "variants",
            "aliases",
            "pins",
            "reviews",
            "uuids",
        ],
    ) || ["moved", "kept", "candidates", "target_prev", "variants", "aliases", "reviews", "uuids"]
        .iter()
        .any(|k| !m[k].is_array())
        || !(m["pins"].is_null() || m["pins"].is_string())
    {
        return Err(invalid("合并快照不完整"));
    }
    let (a, b) = (text(&m["source"], "合并快照不完整")?, text(&m["target"], "合并快照不完整")?);
    let relation: Option<i64> = c
        .query_row(
            "SELECT id FROM identity_relations WHERE kind='duplicate_of' AND ((a=?1 AND b=?2) OR (a=?2 AND b=?1))",
            params![a, b],
            |r| r.get(0),
        )
        .optional()?;
    let relation = relation.ok_or_else(|| invalid("旧合并缺少迁移记录，请先核验备份"))?;
    decide(c, relation, "proposed", "撤销历史合并；原归属无法区分的认领待确认", None, "undo_audit")?;
    character_graph::rebuild(c)?;
    Ok(())
}

fn keys(v: &Value, expected: &[&str]) -> bool {
    v.as_object().is_some_and(|o| o.len() == expected.len() && expected.iter().all(|k| o.contains_key(*k)))
}
