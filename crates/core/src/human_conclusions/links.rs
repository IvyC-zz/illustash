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
                && c.query_row(
                    "SELECT EXISTS(SELECT 1 FROM term_links WHERE term_id=?1 AND system='danbooru' AND current=1)",
                    [term],
                    |r| r.get::<_, bool>(0),
                )?;
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
