//! S3: execute the existing Python ownership SQL, without translating its rules.
//! The SQL files are exact captures of Store._refresh_*; contract tests compare them
//! against SQL captured from the live Python functions (including bound parameters).
use crate::{Error, Result};
use rusqlite::{params_from_iter, Connection, OptionalExtension};

const DEFAULT: &str = include_str!("character_sql/evidence_default.sql");
const HYBRID: &str = include_str!("character_sql/evidence_hybrid.sql");
const MEMBERS: &str = include_str!("character_sql/memberships.sql");
const TAGS: &str = include_str!("character_sql/tag_memberships.sql");
const FOLD: &str = include_str!("character_sql/fold.sql");
const SINGLE: &str = include_str!("character_sql/single_assets.sql");
const LEAD: &str = include_str!("character_sql/lead_assets.sql");
const MULTI: &[&str] = &[
    "multiple_girls",
    "multiple_boys",
    "multiple_others",
    "2girls",
    "3girls",
    "4girls",
    "5girls",
    "6+girls",
    "2boys",
    "3boys",
    "4boys",
    "5boys",
    "6+boys",
];

pub fn policy(c: &Connection) -> Result<bool> {
    let mode: Option<String> =
        c.query_row("SELECT value FROM meta WHERE key='identity_policy'", [], |r| r.get(0)).optional()?;
    match mode.as_deref() {
        None | Some("") => Ok(false),
        Some("hybrid") => Ok(true),
        _ => Err(Error::Conflict("角色核心只支持未设置身份策略或 hybrid".into())),
    }
}

fn touch(c: &Connection, uuids: Option<&[String]>) -> Result<()> {
    if let Some(uuids) = uuids {
        c.execute_batch("CREATE TEMP TABLE IF NOT EXISTS touched(uuid TEXT PRIMARY KEY); DELETE FROM touched;")?;
        let mut put = c.prepare("INSERT OR IGNORE INTO touched VALUES (?)")?;
        for uuid in uuids {
            put.execute([uuid])?;
        }
    }
    Ok(())
}

pub fn evidence(c: &Connection, uuids: Option<&[String]>) -> Result<()> {
    evidence_in(c, uuids, policy(c)?)
}
pub fn legacy_evidence(c: &Connection) -> Result<()> {
    let mode: Option<String> =
        c.query_row("SELECT value FROM meta WHERE key='identity_policy'", [], |r| r.get(0)).optional()?;
    evidence_in(c, None, mode.as_deref() == Some("hybrid"))
}
fn evidence_in(c: &Connection, uuids: Option<&[String]>, hybrid: bool) -> Result<()> {
    let sql = if hybrid { HYBRID } else { DEFAULT };
    touch(c, uuids)?;
    if uuids.is_some() {
        c.execute("DELETE FROM family_evidence WHERE uuid IN (SELECT uuid FROM touched)", [])?;
        c.execute(
            &sql.replace(
                "                  GROUP BY",
                "WHERE v.uuid IN (SELECT uuid FROM touched)\n                  GROUP BY",
            ),
            [],
        )?;
    } else {
        c.execute("DELETE FROM family_evidence", [])?;
        c.execute(sql, [])?;
    }
    Ok(())
}

pub fn single(c: &Connection, uuids: Option<&[String]>) -> Result<()> {
    touch(c, uuids)?;
    for (table, sql, tag) in [("single_assets", SINGLE, "solo"), ("lead_assets", LEAD, "1girl")] {
        let scope = if uuids.is_some() { " WHERE uuid IN (SELECT uuid FROM touched)" } else { "" };
        c.execute(&format!("DELETE FROM {table}{scope}"), [])?;
        let sql = if uuids.is_some() {
            sql.replace(&format!("value='{tag}'"), &format!("value='{tag}' AND g.uuid IN (SELECT uuid FROM touched)"))
        } else {
            sql.to_owned()
        };
        c.execute(&sql, params_from_iter(MULTI))?;
    }
    Ok(())
}

pub fn memberships(c: &Connection, uuids: Option<&[String]>) -> Result<()> {
    memberships_in(c, uuids, true)
}
pub fn legacy_memberships(c: &Connection) -> Result<()> {
    memberships_in(c, None, false)
}
fn memberships_in(c: &Connection, uuids: Option<&[String]>, core: bool) -> Result<()> {
    touch(c, uuids)?;
    let ids = uuids.map(|u| u.iter().collect::<std::collections::BTreeSet<_>>());
    if uuids.is_none() {
        // 两张归属表共享同一组规则结果；原规则及排序键原样计算，临时集合仅存在于这次事务。
        c.execute_batch("DROP TABLE IF EXISTS temp.projected_membership_rules;")?;
        let select = MEMBERS
            .split_once("SELECT ")
            .expect("归属 SQL 包含 SELECT")
            .1
            .rsplit_once(" WHERE NOT EXISTS(SELECT 1 FROM entities t")
            .expect("归属 SQL 包含标签过滤")
            .0;
        c.execute(&format!("CREATE TEMP TABLE projected_membership_rules AS SELECT {select}"), [])?;
    }
    for (table, sql) in [("memberships", MEMBERS), ("tag_memberships", TAGS)] {
        if let Some(ids) = &ids {
            if ids.len() <= 10 {
                let mut delete = c.prepare(&format!("DELETE FROM {table} WHERE uuid=?"))?;
                let mut insert = c.prepare(&format!("{} AND m.uuid=?", sql.trim_end()))?;
                for uuid in ids {
                    delete.execute([uuid])?;
                    insert.execute([uuid])?;
                }
                continue;
            }
            c.execute(&format!("DELETE FROM {table} WHERE uuid IN (SELECT uuid FROM touched)"), [])?;
            c.execute(&format!("{} AND m.uuid IN (SELECT uuid FROM touched)", sql.trim_end()), [])?;
        } else {
            c.execute(&format!("DELETE FROM {table}"), [])?;
            let materialized = sql.split_once("FROM membership_rules m WHERE ").expect("归属 SQL 包含标签过滤").1;
            c.execute(
                &format!("INSERT INTO {table} SELECT m.* FROM projected_membership_rules m WHERE {materialized}"),
                [],
            )?;
        }
    }
    if uuids.is_none() {
        c.execute_batch("DROP TABLE temp.projected_membership_rules;")?;
    }
    let scope = if uuids.is_some() {
        " WHERE d.grp IN (SELECT grp FROM duplicates WHERE uuid IN (SELECT uuid FROM touched))"
    } else {
        ""
    };
    c.execute(&format!("{}{scope}", FOLD.trim_end()), [])?;
    if core {
        mirror(c, uuids.is_some())
    } else {
        Ok(())
    }
}

/// 切换时整片建一次人物 / 皮肤的投影镜像（此前只有核心命令按受影响的图增量维护；S5 真实副本上发现切换后这一片是空的）。
/// 只按当前兼容表镜像，不重算派生表：旧版保留下来的增量结果（第 16 节已批准的 ≤1e-6 例外）原样进镜像。
pub(crate) fn mirror_all(c: &Connection) -> Result<()> {
    mirror(c, false)
}

/// Only the character/skin slice is replaced; artist/label projections stay intact.
fn mirror(c: &Connection, touched: bool) -> Result<()> {
    // S2 registers assets referenced by old evidence/verdicts. A prototype can be
    // the first evidence for another existing library image: register it here.
    let scope = if touched { " WHERE uuid IN (SELECT uuid FROM touched)" } else { "" };
    // library_asset's semantics, in the same sorted order: live registrations stay
    // untouched, missing/removed ones are registered only if present in assets.
    c.execute(
        &format!(
            "INSERT INTO term_assets(namespace,key)
          SELECT 'library',u.uuid FROM (
            SELECT uuid FROM memberships{scope} UNION SELECT uuid FROM tag_memberships{scope}
          ) u JOIN assets a ON a.uuid=u.uuid
          WHERE NOT EXISTS(SELECT 1 FROM term_assets t
            WHERE t.namespace='library' AND t.key=u.uuid AND t.removed_at IS NULL)
          ORDER BY u.uuid
          ON CONFLICT(namespace,key) DO UPDATE SET removed_at=NULL"
        ),
        [],
    )?;
    let scope = if touched {
        " AND asset_id IN (SELECT id FROM term_assets WHERE namespace='library' AND key IN (SELECT uuid FROM touched))"
    } else {
        ""
    };
    // Two IN lists can make SQLite probe every (term, touched asset) pair.
    // Drive the delete by asset_id and test character ownership by primary key.
    let owned = if touched {
        "EXISTS (SELECT 1 FROM term_tree r WHERE r.term_id=term_projection.term_id)"
    } else {
        "term_id IN (SELECT term_id FROM term_tree)"
    };
    c.execute(&format!("DELETE FROM term_projection WHERE {owned}{scope}"), [])?;
    let scope = if touched { " AND m.uuid IN (SELECT uuid FROM touched)" } else { "" };
    c.execute(
        &format!(
            "INSERT INTO term_projection(asset_id,term_id,state,score,sources)
      SELECT a.id,t.id,m.status,m.score,m.sources FROM (
        SELECT uuid,entity,status,score,sources FROM memberships
        UNION ALL SELECT uuid,entity,status,score,sources FROM tag_memberships
        UNION ALL SELECT v.uuid,v.entity,v.status,m.score,m.sources FROM variant_memberships v
          JOIN memberships m ON m.uuid=v.uuid AND m.entity=v.parent_id
      ) m JOIN terms t ON t.key=m.entity JOIN term_tree r ON r.term_id=t.id
      JOIN term_assets a ON a.namespace='library' AND a.key=m.uuid
      WHERE a.removed_at IS NULL{scope}"
        ),
        [],
    )?;
    Ok(())
}

pub fn rebuild(c: &Connection, uuids: Option<&[String]>) -> Result<()> {
    evidence(c, uuids)?;
    single(c, uuids)?;
    memberships(c, uuids)
}

/// 刷新旧规则允许留下延迟派生的图片，供新建与资料撤销共用。
pub(crate) fn refresh_delayed(c: &Connection) -> Result<()> {
    // 旧版允许学习微小分差及身份拆分留下延迟派生，新建会把它们刷新。
    // 这些差异只可能落在学习 / 身份声明涉及的图片，保留刷新语义，不扫无关图片。
    let uuids = c
        .prepare_cached(
            "SELECT uuid FROM proto_claims
             UNION SELECT uuid FROM identity_scopes UNION SELECT uuid FROM identity_suspended",
        )?
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if !uuids.is_empty() {
        evidence(c, Some(&uuids))?;
        memberships(c, Some(&uuids))?;
    }
    Ok(())
}

/// Refresh folded identities inside the caller's existing transaction.
pub fn fold(c: &Connection) -> Result<()> {
    c.execute(FOLD, [])?;
    Ok(())
}
