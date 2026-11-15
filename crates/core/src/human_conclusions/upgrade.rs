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

// 句式只确定支持的操作族。v8 的 metadata 也可能由 v7 文案误认，不能作为清除或保留的证据。
fn character_effects(
    c: &Connection,
    action: i64,
    label: &str,
    metadata: (Option<&str>, Option<&str>),
    items: &[Value],
    has_reviews: bool,
) -> Result<Option<(Effect, Effect, bool)>> {
    use crate::undo_labels::UndoLabel;
    let counted_operation = match metadata {
        (Some(kind), Some(count)) if UndoLabel::parse(kind).is_some() && counted(count, "", "") => true,
        (None, None) => UndoLabel::ALL.into_iter().any(|kind| kind.count(label).is_some()),
        _ => return Ok(None),
    };
    if counted_operation {
        return counted_effect(c, action, items, has_reviews)
            .map(|effect| effect.map(|effect| (effect, effect, false)));
    }
    Ok(if counted(label, "已恢复 ", " 张") {
        Some((Effect::Clear, Effect::Preserve, true))
    } else if counted(label, "已跳过 ", " 张") {
        Some((Effect::Present, Effect::Preserve, false))
    } else if label == "已保留身份未定" {
        Some((Effect::Preserve, Effect::Present, false))
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
    })
}

fn counted_effect(c: &Connection, action: i64, items: &[Value], has_reviews: bool) -> Result<Option<Effect>> {
    if !has_reviews {
        return Ok(None);
    }
    // 六类计数命令的有限矩阵：set/remove/add/drop/swap 都写 review 裁决并清除状态；
    // sweep 写 sweep/yes 裁决和一条 reviews，并保留状态。两种文案误认方向用同一证据核验。
    // 要求裁决与完整撤销快照逐对吻合，不能用同 action 下无关、缺失或额外的裁决作证。
    let snapshots: Option<BTreeSet<(&str, &str)>> =
        items.iter().map(|item| Some((item["uuid"].as_str()?, item["entity"].as_str()?))).collect();
    let Some(snapshots) = snapshots.filter(|s| !s.is_empty() && s.len() == items.len()) else {
        return Ok(None);
    };
    let reviews: i64 = c.query_row("SELECT count(*) FROM reviews WHERE action_id=?1", [action], |r| r.get(0))?;
    let (via, effect) = match reviews {
        0 => ("review", Effect::Clear),
        1 => ("sweep", Effect::Preserve),
        _ => return Ok(None),
    };
    let mut rows = c.prepare(
        "SELECT a.key,t.key,v.via,v.verdict FROM term_verdicts v
         LEFT JOIN term_assets a ON a.id=v.asset_id AND a.namespace='library'
         LEFT JOIN terms t ON t.id=v.term_id WHERE v.action_id=?1",
    )?;
    let rows = rows.query_map([action], |r| {
        Ok((
            r.get::<_, Option<String>>(0)?,
            r.get::<_, Option<String>>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
        ))
    })?;
    let mut witnessed = BTreeSet::new();
    for row in rows {
        let (Some(asset), Some(term), actual_via, verdict) = row? else { return Ok(None) };
        if actual_via != via
            || !(verdict == "yes" || (reviews == 0 && verdict == "no"))
            || !snapshots.contains(&(asset.as_str(), term.as_str()))
            || !witnessed.insert((asset, term))
        {
            return Ok(None);
        }
    }
    Ok((witnessed.len() == snapshots.len()).then_some(effect))
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
    let mut actions =
        c.prepare("SELECT id,label,before_json,ts,label_kind,label_count FROM actions WHERE undone=0 ORDER BY id")?;
    let rows = actions.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, Option<String>>(4)?,
            r.get::<_, Option<String>>(5)?,
        ))
    })?;
    for row in rows {
        let (action, label, body, at, kind, count) = row?;
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
        let effects = if valid {
            character_effects(
                c,
                action,
                &label,
                (kind.as_deref(), count.as_deref()),
                &items,
                tables.contains("reviews"),
            )?
        } else {
            None
        };
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
            if step.before_present != Some(false) {
                previous = None;
            }
            let after = match (step.before_present, after_present, step.effect) {
                (Some(before), Some(false), Some(effect))
                    if matches!(effect, Effect::Clear) || (!before && matches!(effect, Effect::Preserve)) =>
                {
                    if step.explicit_absence || before {
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
