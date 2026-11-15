//! 便携关系贡献与审阅指纹；不把源机器证据伪装成本地证据。
use super::format::Record;
use crate::{schema, store, Error, Result};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeSet;

pub(crate) fn migrate(c: &Connection) -> Result<()> {
    c.execute_batch(
        "CREATE TABLE human_relation_review_context(
        verdict_id INTEGER NOT NULL REFERENCES term_verdicts(id),
        fingerprint TEXT NOT NULL,
        reviewed INTEGER NOT NULL CHECK(reviewed IN (0,1)),
        PRIMARY KEY(verdict_id,fingerprint));",
    )?;
    schema::guard(c, "human_relation_review_context")
}

pub(crate) fn context(c: &Connection, verdict: i64) -> Result<(BTreeSet<String>, BTreeSet<String>)> {
    let (mut seen, mut reviewed) = (BTreeSet::new(), BTreeSet::new());
    for row in c
        .prepare("SELECT fingerprint,reviewed FROM human_relation_review_context WHERE verdict_id=?1")?
        .query_map([verdict], |r| Ok((r.get::<_, String>(0)?, r.get::<_, bool>(1)?)))?
    {
        let (fingerprint, acknowledged) = row?;
        if acknowledged {
            reviewed.insert(fingerprint.clone());
        }
        seen.insert(fingerprint);
    }
    Ok((seen, reviewed))
}

/// 普通复核接管前一有效决议携带的便携指纹。升级中的 v8 调用尚未建立此表。
pub(crate) fn acknowledge(c: &Connection, verdict: i64) -> Result<()> {
    let available: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='human_relation_review_context')",
        [],
        |r| r.get(0),
    )?;
    if available {
        c.execute(
            "INSERT INTO human_relation_review_context(verdict_id,fingerprint,reviewed)
            SELECT ?1,h.fingerprint,1 FROM human_relation_review_context h WHERE h.verdict_id=(
                SELECT v.id FROM term_verdicts v LEFT JOIN actions a ON a.id=v.action_id
                WHERE v.relation_id=(SELECT relation_id FROM term_verdicts WHERE id=?1)
                AND v.id<>?1 AND (v.batch_id IS NOT NULL OR a.undone=0)
                ORDER BY v.anchor DESC,v.batch_id IS NOT NULL DESC,v.id DESC LIMIT 1)",
            [verdict],
        )?;
    }
    Ok(())
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Created {
    id: i64,
    signature: String,
}

fn signature(c: &Connection, id: i64) -> Result<String> {
    let value = c.query_row(
        "SELECT r.a,r.b,r.kind,r.created_at,m.legacy_id FROM term_relation_records r
        LEFT JOIN term_legacy_ids m ON m.kind='relation' AND m.core_id=r.id WHERE r.id=?1",
        [id],
        |r| {
            Ok(json!([
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Option<i64>>(4)?
            ]))
        },
    )?;
    Ok(crate::strict_json::sha256_hex(value.to_string().as_bytes()))
}

pub(crate) fn write(c: &Connection, action: i64, record: &Record) -> Result<Option<Created>> {
    let Record::RelationVerdict { a, b, kind, identity_relation, decision, seen, reviewed, .. } = record else {
        return Err(Error::invalid("需要关系记录"));
    };
    let ta = store::term_by_key(c, a)?.ok_or_else(|| Error::Conflict("关系端点已移除".into()))?.0;
    let tb = store::term_by_key(c, b)?.ok_or_else(|| Error::Conflict("关系端点已移除".into()))?.0;
    let relation_kind = schema::RelationKind::parse(kind).ok_or_else(|| Error::invalid("关系类型不正确"))?;
    let (ta, tb) = if relation_kind.symmetric() && tb < ta { (tb, ta) } else { (ta, tb) };
    let existed: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM term_relations WHERE kind=?3 AND
        ((a=?1 AND b=?2) OR (?3='same_as' AND a=?2 AND b=?1)))",
        params![ta, tb, kind],
        |r| r.get(0),
    )?;
    let id = store::relation(c, ta, tb, relation_kind)?;
    let orientation: bool =
        c.query_row("SELECT a=?2 AND b=?3 FROM term_relations WHERE id=?1", params![id, ta, tb], |r| r.get(0))?;
    if !orientation {
        return Err(Error::Conflict("已有关系的代表方向不同".into()));
    }
    let has_identity: bool =
        c.query_row("SELECT EXISTS(SELECT 1 FROM term_legacy_ids WHERE kind='relation' AND core_id=?1)", [id], |r| {
            r.get(0)
        })?;
    if existed && has_identity != *identity_relation {
        return Err(Error::Conflict("已有关系的身份用途不同".into()));
    }
    if *identity_relation && !has_identity {
        c.execute(
            "INSERT INTO term_legacy_ids(kind,legacy_id,core_id) VALUES ('relation',
            (SELECT coalesce(max(legacy_id),0)+1 FROM term_legacy_ids WHERE kind='relation'),?1)",
            [id],
        )?;
    }
    if !existed {
        c.execute(
            "INSERT INTO human_object_contributions(kind,object_id,owner_action_id,definition_json)
            VALUES ('relation',?1,?2,?3)",
            params![id, action, json!([a, b, kind, identity_relation]).to_string()],
        )?;
    }
    c.execute(
        "INSERT INTO term_verdicts(action_id,anchor,relation_id,verdict,origin,via,reason,created_at)
        VALUES (?1,?1,?2,?3,?4,?5,?6,?7)",
        params![action, id, decision.verdict, decision.origin, decision.via, decision.reason, decision.created_at],
    )?;
    let verdict = c.last_insert_rowid();
    let reviewed: BTreeSet<_> = reviewed.iter().collect();
    for fingerprint in seen {
        c.execute(
            "INSERT INTO human_relation_review_context(verdict_id,fingerprint,reviewed) VALUES (?1,?2,?3)",
            params![verdict, fingerprint, reviewed.contains(fingerprint)],
        )?;
    }
    if *identity_relation {
        c.execute(
            "INSERT INTO term_legacy_ids(kind,legacy_id,core_id) VALUES ('decision',
            (SELECT coalesce(max(legacy_id),0)+1 FROM term_legacy_ids WHERE kind='decision'),?1)",
            [verdict],
        )?;
    }
    Ok(if existed { None } else { Some(Created { id, signature: signature(c, id)? }) })
}

pub(crate) fn check_undo(c: &Connection, action: i64, created: &[Created]) -> Result<()> {
    for object in created {
        let independent: bool = c.query_row("SELECT
            NOT EXISTS(SELECT 1 FROM human_object_contributions WHERE kind='relation' AND object_id=?1 AND owner_action_id=?2)
            OR EXISTS(SELECT 1 FROM term_evidence WHERE relation_id=?1)
            OR EXISTS(SELECT 1 FROM term_verdicts v LEFT JOIN actions a ON a.id=v.action_id
                WHERE v.relation_id=?1 AND (v.batch_id IS NOT NULL OR (v.action_id<>?2 AND a.undone=0)))",
            params![object.id,action], |r|r.get(0))?;
        if independent || signature(c, object.id)? != object.signature {
            return Err(Error::Conflict("导入新建关系已有独立变化或证据，无法安全撤销".into()));
        }
    }
    Ok(())
}
