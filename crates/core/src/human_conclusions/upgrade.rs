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
";

pub(crate) fn migrate(c: &Connection) -> Result<()> {
    c.execute_batch(TABLES)?;
    backfill(c)?;
    for table in ["human_state_marks", "human_state_mark_undo"] {
        schema::guard(c, table)?;
    }
    // 仅当前标记改变使快照变脏，撤销账本本身不成为导出内容。
    for event in ["INSERT", "UPDATE", "DELETE"] {
        c.execute_batch(&format!(
            "CREATE TRIGGER human_marks_revision_{event} AFTER {event} ON human_state_marks
             BEGIN UPDATE meta SET value=CAST(value AS INTEGER)+1 WHERE key='human_revision'; END;"
        ))?;
    }
    Ok(())
}

struct Step {
    action: i64,
    at: String,
    before_present: Option<bool>,
    effect: Option<Effect>,
    explicit_absence: bool,
}

#[derive(Clone, Copy)]
enum Effect {
    Present,
    Clear,
    Preserve,
}

fn keys(v: &Value, expected: &[&str]) -> bool {
    v.as_object().is_some_and(|o| o.len() == expected.len() && expected.iter().all(|k| o.contains_key(*k)))
}

fn asset_key(v: &Value) -> Option<&str> {
    v.as_str().filter(|s| !s.is_empty() && s.chars().count() <= 256)
}

fn counted(label: &str, prefix: &str, suffix: &str) -> bool {
    label
        .strip_prefix(prefix)
        .and_then(|s| s.strip_suffix(suffix))
        .is_some_and(|n| !n.is_empty() && n.chars().all(|c| crate::compat_unicode::decimal(c).is_some()))
}

// 只认历史产出方写过、普通撤销能完整恢复的结构；字段存在本身不证明它保存过一个状态。
fn curation_snapshot(v: &Value) -> bool {
    let old = &v["curation"];
    keys(v, &["uuid", "curation"])
        && asset_key(&v["uuid"]).is_some()
        && (old.is_null()
            || (keys(old, &["status", "origin", "created_at"])
                && matches!(old["status"].as_str(), Some("included" | "excluded"))
                && old["origin"].is_string()
                && old["created_at"].is_string()))
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

// 句式只用来区分已验证快照的操作语义，不能单独凭「已恢复」或「交回自动规则」制造结论。
fn character_effects(label: &str) -> Option<(Effect, Effect, bool)> {
    use crate::undo_labels::UndoLabel;
    if counted(label, "已恢复 ", " 张") {
        Some((Effect::Clear, Effect::Preserve, true))
    } else if counted(label, "已跳过 ", " 张") {
        Some((Effect::Present, Effect::Preserve, false))
    } else if label == "已保留身份未定" {
        Some((Effect::Preserve, Effect::Present, false))
    } else if let Some((kind, _)) = UndoLabel::recognize(label) {
        let effect = if kind == UndoLabel::Sweep { Effect::Preserve } else { Effect::Clear };
        Some((effect, effect, false))
    } else if label
        .strip_prefix("已确认 ")
        .and_then(|s| s.split_once(" 张是 "))
        .is_some_and(|(n, _)| !n.is_empty() && n.chars().all(|c| crate::compat_unicode::decimal(c).is_some()))
        || label
            .strip_prefix("随机抽查：")
            .is_some_and(|s| s.split('，').all(|part| counted(part, "", " 张是") || counted(part, "", " 张不是")))
    {
        Some((Effect::Clear, Effect::Clear, false))
    } else {
        None
    }
}

fn backfill(c: &Connection) -> Result<()> {
    let tables: BTreeSet<String> = c
        .prepare("SELECT name FROM sqlite_master WHERE type='table'")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let available: BTreeSet<Domain> =
        [(Domain::Curation, "curation"), (Domain::Deferred, "deferred"), (Domain::Uncertain, "identity_uncertain")]
            .into_iter()
            .filter(|(_, table)| tables.contains(*table))
            .map(|(domain, _)| domain)
            .collect();
    let mut histories: BTreeMap<(Domain, String), Vec<Step>> = BTreeMap::new();
    let mut actions = c.prepare("SELECT id,label,before_json,ts FROM actions WHERE undone=0 ORDER BY id")?;
    let rows = actions.query_map([], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?))
    })?;
    for row in rows {
        let (action, label, body, at) = row?;
        let Ok(body) = crate::strict_json::parse(&body) else { continue };
        let mut seen = BTreeSet::new();
        let mut add = |domain, key: &str, before_present, effect, explicit_absence| {
            if available.contains(&domain) && seen.insert((domain, key.to_owned())) {
                histories.entry((domain, key.to_owned())).or_default().push(Step {
                    action,
                    at: at.clone(),
                    before_present,
                    effect,
                    explicit_absence,
                });
            }
        };
        if let Some(items) = body.as_array() {
            let effect = if counted(&label, "已将 ", " 张交回自动规则") {
                Some(Effect::Clear)
            } else if counted(&label, "已收录 ", " 张") || counted(&label, "已排除 ", " 张") {
                Some(Effect::Present)
            } else {
                None
            };
            let valid = !items.is_empty() && items.iter().all(curation_snapshot);
            for item in items {
                if let Some(key) = asset_key(&item["uuid"]).filter(|_| item.get("curation").is_some()) {
                    // 不支持的收录载荷也作为边界，不能把其后的空行归到更早的已知步骤。
                    add(
                        Domain::Curation,
                        key,
                        valid.then(|| !item["curation"].is_null()),
                        effect.filter(|_| valid),
                        matches!(effect, Some(Effect::Clear)),
                    );
                }
            }
        }
        let Ok(Some(items)) = crate::character_commands::owns(&body) else { continue };
        let envelope = body[0].get("terms");
        let valid_envelope = envelope.is_none_or(|v| keys(v, &["v", "type", "before"]) && v["type"] == "snapshot");
        let valid = valid_envelope && !items.is_empty() && items.iter().all(character_snapshot);
        let effects = character_effects(&label).filter(|_| valid);
        for item in items {
            let Some(key) = asset_key(&item["uuid"]) else { continue };
            add(
                Domain::Deferred,
                key,
                valid.then(|| item["deferred"].as_bool()).flatten(),
                effects.map(|v| v.0),
                effects.is_some_and(|v| v.2),
            );
            if let Some(value) = item.get("identity").and_then(|v| v.get("identity_uncertain")) {
                add(Domain::Uncertain, key, valid.then(|| !value.is_null()), effects.map(|v| v.1), false);
            }
        }
    }
    for ((domain, key), steps) in histories {
        let current_present = state::positive(c, domain, &key)?.is_some();
        let mut previous: Option<Mark> = None;
        let mut undo = Vec::new();
        for (index, step) in steps.iter().enumerate() {
            let after_present = steps.get(index + 1).map_or(Some(current_present), |next| next.before_present);
            let (Some(before_present), Some(after_present), Some(effect)) =
                (step.before_present, after_present, step.effect)
            else {
                previous = None;
                undo.clear();
                continue;
            };
            let expected = match effect {
                Effect::Present => true,
                Effect::Clear => false,
                Effect::Preserve => before_present,
            };
            // 快照、操作语义和后续状态必须一致；不把中途发生的未知改动归给这一步。
            if expected != after_present {
                previous = None;
                undo.clear();
                continue;
            }
            if before_present {
                previous = None;
            }
            let after = if after_present {
                None
            } else if step.explicit_absence || before_present {
                Some(Mark {
                    owner_action_id: step.action,
                    origin: "legacy".into(),
                    source_ref: None,
                    created_at: step.at.clone(),
                })
            } else {
                previous.clone()
            };
            if previous != after {
                undo.push((step.action, json!(previous).to_string(), json!(after).to_string()));
            }
            previous = after;
        }
        // 只为最后一段可连续还原的历史记账；未知边界之前不能留下当前标记无法匹配的撤销账本。
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
