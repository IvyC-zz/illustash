//! 合并两个身份（`Store.merge`）。hybrid 下是一条 `duplicate_of` 决议（交给 character_identity）；
//! 策略未设置时是破坏式合并，逐行照旧代码，撤销照 `Store._undo_merge`。
use crate::character_commands::{
    begin_action, delete_assertion, finish_action, hybrid, put_assertion, relabel, require_entity, text, At,
};
use crate::character_entities::{ENTITIES, OF};
use crate::character_labels::Shown;
use crate::error::invalid;
use crate::{character_names, character_projection, Error, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::collections::BTreeSet;

fn rows3(c: &Connection, sql: &str, arg: &[&dyn rusqlite::ToSql]) -> Result<Vec<(String, String, Value)>> {
    Ok(c.prepare(sql)?
        .query_map(arg, |r| {
            let third = match r.get_ref(2)? {
                rusqlite::types::ValueRef::Real(v) => json!(v),
                rusqlite::types::ValueRef::Integer(v) => json!(v),
                rusqlite::types::ValueRef::Text(t) => json!(String::from_utf8_lossy(t)),
                _ => Value::Null,
            };
            Ok((r.get(0)?, r.get(1)?, third))
        })?
        .collect::<rusqlite::Result<_>>()?)
}

pub fn merge(c: &Connection, args: &Value) -> Result<Value> {
    if hybrid(c)? {
        let decide = json!({"op": "decide", "a": args["source"], "b": args["target"], "kind": "duplicate_of",
                            "status": "accepted", "reason": "人工确认重复"});
        return crate::character_identity::command(c, &decide);
    }
    let (source, source_parent) = require_entity(c, &args["source"])?;
    let (target, target_parent) = require_entity(c, &args["target"])?;
    if source_parent.is_some() || target_parent.is_some() || source == target {
        return invalid("请选择两个不同的角色（不能是子标签）");
    }
    let (mut moved, mut kept) = (Vec::new(), Vec::new());
    for (u, status, origin) in rows3(c, "SELECT uuid,status,origin FROM assertions WHERE entity=?", &[&source])? {
        let has: bool = c
            .query_row("SELECT 1 FROM assertions WHERE uuid=? AND entity=?", [&u, &target], |_| Ok(()))
            .optional()?
            .is_some();
        let row = json!([u, status, origin]);
        if has {
            kept.push(row);
        } else {
            moved.push(row);
        }
    }
    // 文案里的名字是合并之后的（旧代码在最后读），先占位，最后改。
    let action = begin_action(c, "已合并")?;
    for row in moved.iter().chain(&kept) {
        delete_assertion(
            c,
            At::Action(action),
            row[0].as_str().expect("validated internal text field"),
            &source,
            "merge",
        )?;
    }
    for row in &moved {
        let (u, st, o) = (
            row[0].as_str().expect("validated internal text field"),
            row[1].as_str().expect("validated internal text field"),
            row[2].as_str().expect("validated internal text field"),
        );
        put_assertion(c, At::Action(action), u, &target, st, o)?;
    }
    let cands = rows3(c, "SELECT uuid,source,score FROM candidates WHERE entity=?", &[&source])?;
    let prev = rows3(
        c,
        "SELECT t.uuid,t.source,t.score FROM candidates t JOIN candidates s ON s.uuid=t.uuid AND s.source=t.source AND s.entity=?1 WHERE t.entity=?2",
        &[&source, &target],
    )?;
    let variants: Vec<String> = c
        .prepare(&format!("SELECT id FROM {ENTITIES} WHERE parent_id=? ORDER BY id"))?
        .query_map([&source], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    c.execute("UPDATE term_tree SET parent_id=? WHERE parent_id=?", [&target, &source])?;
    let mut carried: Vec<(String, String, String)> = c
        .prepare(&format!("SELECT lang, value, source FROM term_names WHERE term_id = {}", character_names::TERM_OF))?
        .query_map([&source], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for key in c
        .prepare(&format!(
            "SELECT l.key FROM {} l WHERE l.entity_id = ? AND l.core_system IN ('danbooru', 'wd_character')",
            crate::character_links::LINKS
        ))?
        .query_map([&source], |r| r.get::<_, String>(0))?
    {
        carried.push((String::new(), key?, "merge".into()));
    }
    let mut added = Vec::new();
    for (lang, value, src) in carried {
        if character_names::alias(c, &target, &value, &src, &lang)? {
            added.push(json!([lang, value, src]));
        }
    }
    let pins: Option<String> = c.query_row("SELECT value FROM prefs WHERE key='pins'", [], |r| r.get(0)).optional()?;
    if let Some(p) = &pins {
        let list: Vec<String> = serde_json::from_str(p).map_err(|e| Error::Invalid(e.to_string()))?;
        if list.contains(&source) {
            let mut seen = BTreeSet::new();
            let new: Vec<&String> = list
                .iter()
                .map(|p| if *p == source { &target } else { p })
                .filter(|p| seen.insert(p.to_string()))
                .collect();
            c.execute("UPDATE prefs SET value=? WHERE key='pins'", [crate::character_json::dump(&json!(new), true)])?;
        }
    }
    let reviews: Vec<i64> = c
        .prepare("SELECT action_id FROM reviews WHERE entity=?")?
        .query_map([&source], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    c.execute("UPDATE reviews SET entity=? WHERE entity=?", [&target, &source])?;
    c.execute(&format!("UPDATE term_tree SET merged_into=?2 WHERE {OF}"), [&source, &target])?;
    let mut touched: BTreeSet<String> = BTreeSet::new();
    for r in moved.iter().chain(&kept) {
        touched.insert(r[0].as_str().expect("validated internal text field").to_owned());
    }
    for (u, _, _) in &cands {
        touched.insert(u.clone());
    }
    for v in &variants {
        for u in c.prepare("SELECT uuid FROM candidates WHERE entity=?")?.query_map([v], |r| r.get::<_, String>(0))? {
            touched.insert(u?);
        }
    }
    let touched: Vec<String> = touched.into_iter().collect();
    // 候选跟着合并走：派生表 `candidates` 由证据按合并解析算出，等于旧版的「搬到目标、同键取最大」。
    character_projection::refresh_candidates(c, &touched)?;
    character_projection::evidence(c, Some(&touched))?;
    character_projection::memberships(c, Some(&touched))?;
    let before = vec![json!({"merge": {
        "source": source, "target": target, "moved": moved, "kept": kept,
        "candidates": cands.iter().map(|(u, k, s)| json!([u, k, s])).collect::<Vec<_>>(),
        "target_prev": prev.iter().map(|(u, k, s)| json!([u, k, s])).collect::<Vec<_>>(),
        "variants": variants, "aliases": added, "pins": pins, "reviews": reviews, "uuids": touched,
    }})];
    let mut shown = Shown::new();
    let label = format!(
        "已将 {left} 合并到 {right}（{moved} 条人工结论）",
        left = shown.name(c, &source)?,
        right = shown.name(c, &target)?,
        moved = moved.len()
    );
    relabel(c, action, &label)?;
    finish_action(c, action, "merge", before)?;
    Ok(json!({"action": action}))
}

/// 撤销合并。hybrid 下（包括激活前留下的旧合并）：关系退回 proposed（交给 character_identity）；
/// 策略未设置时照 `_undo_merge` 恢复。人工结论由裁决撤销自然回退，这里把派生表 `assertions` 对齐到有效裁决。
pub fn undo(c: &Connection, action: i64, m: &Value) -> Result<()> {
    if hybrid(c)? {
        return crate::character_identity::undo_merge(c, m);
    }
    let source = text(&m["source"], "合并快照不完整")?;
    let target = text(&m["target"], "合并快照不完整")?;
    let list = |k: &str| m[k].as_array().cloned().unwrap_or_default();
    c.execute(&format!("UPDATE term_tree SET merged_into=NULL WHERE {OF}"), [source])?;
    for row in list("moved") {
        crate::character_commands::refresh_assertion(c, text(&row[0], "合并快照不完整")?, target)?;
    }
    for row in list("moved").iter().chain(&list("kept")) {
        crate::character_commands::refresh_assertion(c, text(&row[0], "合并快照不完整")?, source)?;
    }
    for v in list("variants") {
        c.execute(
            &format!("UPDATE term_tree SET parent_id=?2 WHERE {OF}"),
            params![text(&v, "合并快照不完整")?, source],
        )?;
    }
    for a in list("aliases") {
        let (lang, value, src) = match a.as_array() {
            Some(x) if x.len() == 3 => {
                (x[0].as_str().unwrap_or(""), x[1].as_str().unwrap_or(""), x[2].as_str().unwrap_or(""))
            }
            _ => ("", a.as_str().unwrap_or(""), "legacy"),
        };
        c.execute(
            &format!(
                "DELETE FROM term_names
                 WHERE term_id = {} AND role = 'alias' AND lang = ? AND value = ? AND source = ?",
                character_names::TERM_OF
            ),
            [target, lang, value, src],
        )?;
    }
    if let Some(p) = m["pins"].as_str() {
        c.execute("UPDATE prefs SET value=? WHERE key='pins'", [p])?;
    }
    for r in list("reviews") {
        c.execute("UPDATE reviews SET entity=? WHERE action_id=?", params![source, r.as_i64()])?;
    }
    let uuids: Vec<String> = list("uuids").iter().filter_map(|u| u.as_str().map(str::to_owned)).collect();
    restore_candidates(c, action, m, source, target)?;
    character_projection::evidence(c, Some(&uuids))?;
    character_projection::memberships(c, Some(&uuids))
}

type Cand = (String, String, String); // (uuid, entity, source)

/// `_undo_merge` 的候选：旧版删掉目标上这些 (图, 来源) 的行，放回目标原来的行和源身份的行。
/// 合并在核心里做的，证据本来就在源身份上，解除合并后自然算回来；合并发生在收编之前时，旧表里只有搬到目标的行，
/// 就按旧版的结果为这几张图另起一个证据批次（覆盖这几张图、这两个来源）。
fn restore_candidates(c: &Connection, action: i64, m: &Value, source: &str, target: &str) -> Result<()> {
    let list = |k: &str| m[k].as_array().cloned().unwrap_or_default();
    let mut uuids: BTreeSet<String> = BTreeSet::new();
    for r in list("candidates").iter().chain(&list("target_prev")) {
        uuids.insert(text(&r[0], "合并快照不完整")?.to_owned());
    }
    if uuids.is_empty() {
        return Ok(());
    }
    let scope: Vec<String> = uuids.into_iter().collect();
    let current = |c: &Connection| -> Result<std::collections::BTreeMap<Cand, f64>> {
        let mut out = std::collections::BTreeMap::new();
        for u in &scope {
            for row in c
                .prepare_cached("SELECT uuid,entity,source,score FROM candidates WHERE uuid=?")?
                .query_map([u], |r| Ok(((r.get(0)?, r.get(1)?, r.get(2)?), r.get(3)?)))?
            {
                let (k, v) = row?;
                out.insert(k, v);
            }
        }
        Ok(out)
    };
    let mut expected = current(c)?;
    for r in list("candidates") {
        expected.remove(&(text(&r[0], "")?.to_owned(), target.to_owned(), text(&r[1], "")?.to_owned()));
    }
    for (rows, owner) in [(list("target_prev"), target), (list("candidates"), source)] {
        for r in rows {
            expected.insert(
                (text(&r[0], "")?.to_owned(), owner.to_owned(), text(&r[1], "")?.to_owned()),
                r[2].as_f64().unwrap_or(0.0),
            );
        }
    }
    character_projection::refresh_candidates(c, &scope)?;
    if current(c)? == expected {
        return Ok(());
    }
    crate::character_evidence::replace(c, &format!("undo-merge/{action}"), &scope, &expected)?;
    character_projection::refresh_candidates(c, &scope)
}
