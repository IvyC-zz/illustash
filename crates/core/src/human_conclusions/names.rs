//! 已有词条的人工名字组补齐；按语言和角色保护目标人工选择，撤销只恢复本次触及的组。
use super::format::{Entity, Name};
use crate::{Error, Result};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Row {
    value: String,
    source: String,
    preferred: bool,
    position: i64,
    seq: Option<i64>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Edit {
    term: i64,
    lang: String,
    role: String,
    before: Vec<Row>,
    after: Vec<Row>,
    aliases: Vec<Name>,
}

pub(crate) struct Planned {
    term: i64,
    lang: String,
    role: String,
    before: Vec<Row>,
    names: Vec<Name>,
}

fn rows(c: &Connection, term: i64, lang: &str, role: &str) -> Result<Vec<Row>> {
    Ok(c.prepare(
        "SELECT value,source,preferred,position,seq FROM term_names
        WHERE term_id=?1 AND lang=?2 AND role=?3 ORDER BY seq,source,value",
    )?
    .query_map(params![term, lang, role], |r| {
        Ok(Row { value: r.get(0)?, source: r.get(1)?, preferred: r.get(2)?, position: r.get(3)?, seq: r.get(4)? })
    })?
    .collect::<rusqlite::Result<_>>()?)
}

pub(crate) fn plan(
    c: &Connection,
    key: &str,
    source: &Entity,
    current: Option<&Entity>,
    counts: &mut BTreeMap<String, usize>,
) -> Result<Vec<Planned>> {
    let term = c.query_row("SELECT id FROM terms WHERE key=?1", [key], |r| r.get(0))?;
    let mut groups = BTreeMap::<(&str, &str), Vec<Name>>::new();
    for name in &source.names {
        if name.provenance != "description" {
            groups.entry((&name.lang, &name.role)).or_default().push(name.clone());
        }
    }
    let mut planned = Vec::new();
    for ((lang, role), names) in groups {
        let before = rows(c, term, lang, role)?;
        let target: Vec<_> = current
            .into_iter()
            .flat_map(|e| &e.names)
            .filter(|n| n.provenance != "description" && n.lang == lang && n.role == role)
            .collect();
        if !target.is_empty() || before.iter().any(|n| n.source == "user") {
            let same = target.len() == names.len() && target.iter().zip(&names).all(|(a, b)| *a == b);
            *counts
                .entry(if same { "name_group_already_present" } else { "name_group_conflict" }.into())
                .or_default() += 1;
            continue;
        }
        // 机器行占了完全相同的存储键却不能证明同一人工来源时，不覆盖它或伪造出处。
        if names.iter().any(|n| before.iter().any(|r| r.value == n.value && r.source == n.source)) {
            *counts.entry("name_group_conflict".into()).or_default() += 1;
            continue;
        }
        *counts.entry("name_group_applied".into()).or_default() += 1;
        planned.push(Planned { term, lang: lang.into(), role: role.into(), before, names });
    }
    Ok(planned)
}

pub(crate) fn apply(c: &Connection, planned: Vec<Planned>) -> Result<Vec<Edit>> {
    let mut edits = Vec::new();
    for p in planned {
        if p.names.iter().any(|n| n.preferred) {
            c.execute(
                "UPDATE term_names SET preferred=0 WHERE term_id=?1 AND lang=?2 AND role=?3",
                params![p.term, p.lang, p.role],
            )?;
        }
        for name in &p.names {
            c.execute(
                "INSERT INTO term_names(term_id,lang,value,role,source,preferred,position,seq)
                VALUES (?1,?2,?3,?4,?5,?6,?7,(SELECT coalesce(max(seq),0)+1 FROM term_names))",
                params![p.term, name.lang, name.value, name.role, name.source, name.preferred, name.position],
            )?;
        }
        let after = rows(c, p.term, &p.lang, &p.role)?;
        edits.push(Edit {
            term: p.term,
            lang: p.lang,
            role: p.role,
            before: p.before,
            after,
            aliases: p.names.into_iter().filter(|n| n.provenance == "legacy_merge").collect(),
        });
    }
    Ok(edits)
}

pub(crate) fn undo(c: &Connection, edits: &[Edit]) -> Result<()> {
    for edit in edits {
        if rows(c, edit.term, &edit.lang, &edit.role)? != edit.after {
            return Err(Error::Conflict("导入后的名字组已有独立变化，无法安全撤销".into()));
        }
    }
    for edit in edits {
        c.execute(
            "DELETE FROM term_names WHERE term_id=?1 AND lang=?2 AND role=?3",
            params![edit.term, edit.lang, edit.role],
        )?;
        for row in &edit.before {
            c.execute("INSERT INTO term_names(term_id,lang,value,role,source,preferred,position,seq) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                params![edit.term,edit.lang,row.value,edit.role,row.source,row.preferred,row.position,row.seq])?;
        }
    }
    Ok(())
}

pub(crate) fn aliases(edits: &[Edit]) -> Vec<(i64, Name)> {
    edits.iter().flat_map(|e| e.aliases.iter().cloned().map(|n| (e.term, n))).collect()
}
