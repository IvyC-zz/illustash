//! 第 9 版的人工状态表；升级只读取库内历史，不读取原图。
use super::state::{self, Domain, Mark};
use crate::{schema, Result};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

const TABLES: &str = "
CREATE TABLE human_state_marks(
 domain TEXT NOT NULL CHECK(domain IN ('curation','deferred','uncertain')),
 asset_key TEXT NOT NULL CHECK(length(asset_key) BETWEEN 1 AND 256),
 state TEXT NOT NULL,
 owner_action_id INTEGER NOT NULL REFERENCES actions(id),
 origin TEXT NOT NULL CHECK(origin IN ('user','album','legacy')),
 source_ref TEXT,
 created_at TEXT NOT NULL,
 PRIMARY KEY(domain,asset_key),
 CHECK((domain='curation' AND state='reset') OR (domain='deferred' AND state='resumed')
    OR (domain='uncertain' AND state='cleared')));
CREATE INDEX human_state_marks_action ON human_state_marks(owner_action_id);
CREATE TABLE human_state_mark_undo(
 action_id INTEGER NOT NULL REFERENCES actions(id),
 domain TEXT NOT NULL CHECK(domain IN ('curation','deferred','uncertain')),
 asset_key TEXT NOT NULL,
 before_json TEXT NOT NULL CHECK(json_valid(before_json)),
 after_json TEXT NOT NULL CHECK(json_valid(after_json)),
 PRIMARY KEY(action_id,domain,asset_key));
INSERT INTO meta(key,value) VALUES ('human_revision','0');
CREATE TABLE human_backup_state(id INTEGER PRIMARY KEY CHECK(id=1),state_json TEXT NOT NULL CHECK(json_valid(state_json)));
CREATE TABLE human_import_reports(id TEXT PRIMARY KEY,action_id INTEGER REFERENCES actions(id),effect_id INTEGER,summary_json TEXT NOT NULL CHECK(json_valid(summary_json)));
CREATE TABLE human_import_report_rows(report_id TEXT NOT NULL REFERENCES human_import_reports(id),seq INTEGER NOT NULL,detail_json TEXT NOT NULL CHECK(json_valid(detail_json)),PRIMARY KEY(report_id,seq));
";

pub(crate) fn migrate(c: &Connection) -> Result<()> {
    c.execute_batch(TABLES)?;
    backfill(c)?;
    super::visibility::migrate(c)?;
    super::relations::migrate(c)?;
    // 一次导入可带多个人物的检查记录，原 action_id 单列主键不足以表达；旧三列逐行原样保留。
    if c.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='reviews')", [], |r| {
        r.get::<_, bool>(0)
    })? {
        c.execute_batch("CREATE TABLE human_reviews_upgrade(entity TEXT NOT NULL,reviewed_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            action_id INTEGER NOT NULL,PRIMARY KEY(action_id,entity));
            INSERT INTO human_reviews_upgrade SELECT entity,reviewed_at,action_id FROM reviews;
            DROP TABLE reviews;
            ALTER TABLE human_reviews_upgrade RENAME TO reviews;")?;
    }
    for table in ["human_state_marks", "human_state_mark_undo", "human_import_reports", "human_import_report_rows"] {
        schema::guard(c, table)?;
    }
    // 仅当前标记改变使快照变脏，撤销账本本身不成为导出内容。
    for event in ["INSERT", "UPDATE", "DELETE"] {
        c.execute_batch(&format!(
            "CREATE TRIGGER human_marks_revision_{event} AFTER {event} ON human_state_marks
             BEGIN UPDATE meta SET value=CAST(value AS INTEGER)+1 WHERE key='human_revision'; END;"
        ))?;
    }
    super::revision::migrate(c)?;
    super::hash_cache::migrate(c)?;
    Ok(())
}

struct Step {
    action: i64,
    at: String,
    before_present: Option<bool>,
    effect: Option<Effect>,
}

#[derive(Clone, Copy)]
enum Effect {
    Clear,
    Preserve,
}

fn keys(v: &Value, expected: &[&str]) -> bool {
    v.as_object().is_some_and(|o| o.len() == expected.len() && expected.iter().all(|k| o.contains_key(*k)))
}

fn asset_key(v: &Value) -> Option<&str> {
    v.as_str().filter(|s| !s.is_empty() && s.chars().count() <= 256)
}

fn json_strings(v: &Value) -> bool {
    v.as_str()
        .and_then(|s| crate::strict_json::parse(s).ok())
        .is_some_and(|v| v.as_array().is_some_and(|a| a.iter().all(Value::is_string)))
}

fn identity_snapshot(v: &Value) -> bool {
    keys(v, &["identity_scopes", "identity_suspended", "identity_uncertain"])
        && [("identity_scopes", 1, 0), ("identity_suspended", 2, 1), ("identity_uncertain", 3, 0)].into_iter().all(
            |(field, width, encoded)| {
                v[field].is_null()
                    || v[field].as_array().is_some_and(|a| {
                        a.len() == width && a.iter().all(Value::is_string) && json_strings(&a[encoded])
                    })
            },
        )
}

fn character_snapshot(v: &Value) -> bool {
    let assertion = &v["assertion"];
    (keys(v, &["uuid", "entity", "assertion", "deferred"])
        || (keys(v, &["uuid", "entity", "assertion", "deferred", "identity"]) && identity_snapshot(&v["identity"])))
        && asset_key(&v["uuid"]).is_some()
        && (v["entity"].is_null() || v["entity"].is_string())
        && v["deferred"].is_boolean()
        && (assertion.is_null()
            || (keys(assertion, &["status", "origin"])
                && matches!(assertion["status"].as_str(), Some("confirmed" | "rejected"))
                && assertion["origin"].is_string()))
}

// 用户限定只采用 v8 已存的种类和数量，不再由文案、裁决或检查记录反推旧操作。
fn character_effect(kind: Option<&str>, count: Option<&str>) -> Option<Effect> {
    use crate::undo_labels::UndoLabel;
    let count = count?;
    if count.is_empty() || !count.chars().all(|c| crate::compat_unicode::decimal(c).is_some()) {
        return None;
    }
    match UndoLabel::parse(kind?)? {
        UndoLabel::Sweep => Some(Effect::Preserve),
        _ => Some(Effect::Clear),
    }
}

fn backfill(c: &Connection) -> Result<()> {
    let tables: BTreeSet<String> = c
        .prepare("SELECT name FROM sqlite_master WHERE type='table'")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let available: BTreeSet<Domain> = [(Domain::Deferred, "deferred"), (Domain::Uncertain, "identity_uncertain")]
        .into_iter()
        .filter(|(_, table)| tables.contains(*table))
        .map(|(domain, _)| domain)
        .collect();
    let mut histories: BTreeMap<(Domain, String), Vec<Step>> = BTreeMap::new();
    let mut actions =
        c.prepare("SELECT id,before_json,ts,label_kind,label_count FROM actions WHERE undone=0 ORDER BY id")?;
    let rows = actions.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, Option<String>>(3)?,
            r.get::<_, Option<String>>(4)?,
        ))
    })?;
    for row in rows {
        let (action, body, at, kind, count) = row?;
        let Ok(body) = crate::strict_json::parse(&body) else { continue };
        let mut seen = BTreeSet::new();
        let mut add = |domain, key: &str, before_present, effect| {
            if available.contains(&domain) && seen.insert((domain, key.to_owned())) {
                histories.entry((domain, key.to_owned())).or_default().push(Step {
                    action,
                    at: at.clone(),
                    before_present,
                    effect,
                });
            }
        };
        let Ok(Some(items)) = crate::character_commands::owns(&body) else { continue };
        let envelope = body[0].get("terms");
        let valid_envelope = envelope.is_none_or(|v| keys(v, &["v", "type", "before"]) && v["type"] == "snapshot");
        let valid = valid_envelope && !items.is_empty() && items.iter().all(character_snapshot);
        let effect = valid.then(|| character_effect(kind.as_deref(), count.as_deref())).flatten();
        for item in items {
            let Some(key) = asset_key(&item["uuid"]) else { continue };
            add(Domain::Deferred, key, valid.then(|| item["deferred"].as_bool()).flatten(), effect);
            if let Some(value) = item.get("identity").and_then(|v| v.get("identity_uncertain")) {
                add(Domain::Uncertain, key, valid.then(|| !value.is_null()), effect);
            }
        }
    }
    for ((domain, key), steps) in histories {
        let current_present = state::positive(c, domain, &key)?.is_some();
        let mut previous: Option<Mark> = None;
        let mut undo = Vec::new();
        for (index, step) in steps.iter().enumerate() {
            let after_present = steps.get(index + 1).map_or(Some(current_present), |next| next.before_present);
            if step.before_present != Some(false) {
                previous = None;
            }
            let after = match (step.before_present, after_present, step.effect) {
                (Some(before), Some(false), Some(effect))
                    if matches!(effect, Effect::Clear) || (!before && matches!(effect, Effect::Preserve)) =>
                {
                    if before {
                        Some(Mark {
                            owner_action_id: step.action,
                            origin: "legacy".into(),
                            source_ref: None,
                            created_at: step.at.clone(),
                        })
                    } else {
                        previous.clone()
                    }
                }
                _ => None,
            };
            // 未知或不一致的步骤不产生新结论。有效 before 仍证明它撤销后应恢复什么，
            // 因此记录已知标记到未决定的边界，并保留此前已证明的账本供连续撤销使用。
            if previous != after {
                undo.push((step.action, json!(previous).to_string(), json!(after).to_string()));
            }
            previous = after;
        }
        for (action, before, after) in undo {
            c.execute(
                "INSERT INTO human_state_mark_undo(action_id,domain,asset_key,before_json,after_json) VALUES (?1,?2,?3,?4,?5)",
                params![action, domain.key(), key, before, after],
            )?;
        }
        state::replace(c, domain, &key, previous.as_ref())?;
    }
    Ok(())
}
