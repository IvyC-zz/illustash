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
    before_present: bool,
    explicit_absence: bool,
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
        let mut add = |domain, key: &str, before_present, explicit_absence| {
            if available.contains(&domain) && seen.insert((domain, key.to_owned())) {
                histories.entry((domain, key.to_owned())).or_default().push(Step {
                    action,
                    at: at.clone(),
                    before_present,
                    explicit_absence,
                });
            }
        };
        // 应用层只接受完整的收录载荷；不把混合或未知所有者的载荷解释为 reset。
        if let Some(items) = body.as_array().filter(|items| {
            !items.is_empty()
                && items.iter().all(|v| {
                    v.as_object().is_some_and(|o| {
                        o.len() == 2 && o.contains_key("curation") && o.get("uuid").is_some_and(Value::is_string)
                    })
                })
        }) {
            for item in items {
                if let Some(key) = item["uuid"].as_str() {
                    add(Domain::Curation, key, !item["curation"].is_null(), true);
                }
            }
        }
        let Ok(Some(items)) = crate::character_commands::owns(&body) else { continue };
        let resume =
            label.strip_prefix("已恢复 ").and_then(|v| v.strip_suffix(" 张")).is_some_and(|v| v.parse::<u64>().is_ok());
        for item in items {
            let (Some(key), Some(deferred)) = (item["uuid"].as_str(), item["deferred"].as_bool()) else { continue };
            add(Domain::Deferred, key, deferred, resume);
            if let Some(value) = item.get("identity").and_then(|v| v.get("identity_uncertain")) {
                if value.is_null() || value.is_array() {
                    add(Domain::Uncertain, key, !value.is_null(), false);
                }
            }
        }
    }
    for ((domain, key), steps) in histories {
        let current_present = state::positive(c, domain, &key)?.is_some();
        let mut previous: Option<Mark> = None;
        for (index, step) in steps.iter().enumerate() {
            if step.before_present {
                previous = None;
            }
            let after_present = steps.get(index + 1).map_or(current_present, |next| next.before_present);
            let after = if after_present {
                None
            } else if step.explicit_absence || step.before_present {
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
                c.execute(
                    "INSERT INTO human_state_mark_undo(action_id,domain,asset_key,before_json,after_json) VALUES (?1,?2,?3,?4,?5)",
                    params![step.action, domain.key(), key, json!(previous).to_string(), json!(after).to_string()],
                )?;
            }
            previous = after;
        }
        state::replace(c, domain, &key, previous.as_ref())?;
    }
    Ok(())
}
