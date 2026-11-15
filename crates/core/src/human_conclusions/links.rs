//! 补充尚未占用的公共链接；既有归属和显式 current 状态始终保留。
use super::format::{Entity, Link};
use crate::{Error, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Added {
    term: i64,
    link: Link,
}

// v8 普通资料快照能证明此角色曾有当前链接；现行唯一会清空该槽的入口是人工资料/作品编辑。
// 只接受仍有效的已知资料载荷及仍属于本词条的那条链接；不把无链接或任意旧载荷推断为清空。
fn cleared(c: &Connection, term: i64, key: &str) -> Result<bool> {
    let work_clear:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM term_tree WHERE term_id=?1 AND review_status='confirmed' AND review_note='外部链接已由用户清空')",[term],|r|r.get(0))?;
    if work_clear {
        return Ok(true);
    }
    for row in c
        .prepare("SELECT before_json FROM actions WHERE undone=0 ORDER BY id DESC")?
        .query_map([], |r| r.get::<_, String>(0))?
    {
        let Ok(value) = crate::strict_json::parse(&row?) else { continue };
        let Ok(Some(items)) = crate::character_commands::owns(&value) else { continue };
        for item in items {
            let profile = &item["profile"];
            if profile["entity"] != key {
                continue;
            }
            for link in profile["links"].as_array().into_iter().flatten() {
                if link[0] == "danbooru" && link[2] == 1 {
                    if let Some(old) = link[1].as_str() {
                        let inactive:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM term_links WHERE term_id=?1 AND system='danbooru' AND key=?2 AND current=0)",params![term,old],|r|r.get(0))?;
                        if inactive {
                            return Ok(true);
                        }
                    }
                }
            }
        }
    }
    Ok(false)
}

pub(crate) fn plan(
    c: &Connection,
    key: &str,
    source: &Entity,
    counts: &mut BTreeMap<String, usize>,
) -> Result<Vec<Added>> {
    let term = c.query_row("SELECT id FROM terms WHERE key=?1", [key], |r| r.get(0))?;
    let mut added = Vec::new();
    for link in &source.links {
        let existing: Option<(i64, bool)> = c
            .query_row(
                "SELECT term_id,current FROM term_links WHERE system=?1 AND key=?2",
                params![link.system, link.key],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let result = if let Some((owner, current)) = existing {
            if owner == term && current == link.current {
                "link_already_present"
            } else {
                "link_conflict"
            }
        } else {
            let occupied = source.tree.is_some()
                && link.system == "danbooru"
                && link.current
                && (c.query_row(
                    "SELECT EXISTS(SELECT 1 FROM term_links WHERE term_id=?1 AND system='danbooru' AND current=1)",
                    [term],
                    |r| r.get::<_, bool>(0),
                )? || cleared(c, term, key)?);
            if occupied {
                "link_conflict"
            } else {
                added.push(Added { term, link: link.clone() });
                "link_applied"
            }
        };
        *counts.entry(result.into()).or_default() += 1;
    }
    Ok(added)
}

pub(crate) fn apply(c: &Connection, added: &[Added]) -> Result<()> {
    for a in added {
        c.execute(
            "INSERT INTO term_links(system,key,term_id,current,source,added_at) VALUES (?1,?2,?3,?4,?5,?6)",
            params![a.link.system, a.link.key, a.term, a.link.current, a.link.source, a.link.added_at],
        )?;
    }
    Ok(())
}

pub(crate) fn undo(c: &Connection, added: &[Added]) -> Result<()> {
    for a in added {
        let same: bool = c.query_row(
            "SELECT EXISTS(SELECT 1 FROM term_links WHERE system=?1 AND key=?2 AND term_id=?3
            AND current=?4 AND source=?5 AND added_at=?6 AND url='')",
            params![a.link.system, a.link.key, a.term, a.link.current, a.link.source, a.link.added_at],
            |r| r.get(0),
        )?;
        if !same {
            return Err(Error::Conflict("导入的公共链接已有独立变化，无法安全撤销".into()));
        }
    }
    for a in added {
        c.execute("DELETE FROM term_links WHERE system=?1 AND key=?2", params![a.link.system, a.link.key])?;
    }
    Ok(())
}
