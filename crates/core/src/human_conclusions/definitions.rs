//! 导入新定义的有效贡献及精确撤销保护；不修改目标原有定义。
use super::format::Entity;
use crate::{schema::Kind, store, Error, Result};
use rusqlite::{params, types::ValueRef, Connection};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub(crate) struct New {
    pub source_key: String,
    pub target_key: String,
    pub definition: Entity,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Created {
    pub id: i64,
    pub key: String,
    pub signature: String,
}

pub(crate) fn create(c: &Connection, action: i64, source_library: &str, definitions: &[New]) -> Result<Vec<Created>> {
    let mut created = Vec::new();
    for new in definitions {
        let e = &new.definition;
        if e.tree.is_some() || e.baseline.is_some() {
            return Err(Error::invalid("结构定义需要专用结构写入"));
        }
        let kind = Kind::parse(&e.kind).ok_or_else(|| Error::invalid("定义类型不正确"))?;
        let id = store::create_term(c, kind, Some(&new.target_key), &e.origin)?;
        c.execute("INSERT INTO human_object_contributions(kind,object_id,owner_action_id,definition_json) VALUES ('term',?1,?2,?3)",
            params![id,action,json!(e).to_string()])?;
        c.execute("INSERT INTO human_entity_origins(source_library,source_key,target_key,owner_action_id) VALUES (?1,?2,?3,?4)",
            params![source_library,new.source_key,new.target_key,action])?;
        for name in &e.names {
            c.execute(
                "INSERT INTO term_names(term_id,lang,value,role,source,preferred,position,seq)
                VALUES (?1,?2,?3,?4,?5,?6,?7,(SELECT coalesce(max(seq),0)+1 FROM term_names))",
                params![id, name.lang, name.value, name.role, name.source, name.preferred, name.position],
            )?;
        }
        for link in &e.links {
            c.execute(
                "INSERT INTO term_links(system,key,term_id,current,added_at,source) VALUES (?1,?2,?3,?4,?5,?6)",
                params![link.system, link.key, id, link.current, link.added_at, link.source],
            )?;
        }
        created.push(Created { id, key: new.target_key.clone(), signature: signature(c, id)? });
    }
    Ok(created)
}

fn rows(c: &Connection, sql: &str, id: i64) -> Result<Value> {
    let mut statement = c.prepare(sql)?;
    let columns = statement.column_count();
    let mut rows = statement.query([id])?;
    let mut values = Vec::new();
    while let Some(row) = rows.next()? {
        let mut cells = Vec::new();
        for column in 0..columns {
            let cell = match row.get_ref(column)? {
                ValueRef::Null => json!(["null"]),
                ValueRef::Integer(v) => json!(["integer", v]),
                ValueRef::Real(v) => json!(["real", format!("{:016x}", v.to_bits())]),
                ValueRef::Text(v) => {
                    json!(["text", std::str::from_utf8(v).map_err(|_| Error::invalid("定义含非 UTF-8 文本"))?])
                }
                ValueRef::Blob(v) => json!(["blob", v]),
            };
            cells.push(cell);
        }
        values.push(cells);
    }
    Ok(json!(values))
}

fn signature(c: &Connection, id: i64) -> Result<String> {
    let mut value = Vec::new();
    for sql in [
        "SELECT * FROM term_records WHERE id=?1",
        "SELECT * FROM term_names WHERE term_id=?1 ORDER BY role,lang,source,value",
        "SELECT * FROM term_links WHERE term_id=?1 ORDER BY system,key",
        "SELECT * FROM term_tree WHERE term_id=?1",
    ] {
        value.push(rows(c, sql, id)?);
    }
    Ok(crate::strict_json::sha256_hex(json!(value).to_string().as_bytes()))
}

pub(crate) fn check_undo(c: &Connection, action: i64, created: &[Created]) -> Result<()> {
    for object in created {
        let owner: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM human_object_contributions WHERE kind='term' AND object_id=?1 AND owner_action_id=?2)",
            params![object.id,action],|r|r.get(0))?;
        let independent: bool = c.query_row("SELECT
            EXISTS(SELECT 1 FROM term_evidence WHERE term_id=?1)
            OR EXISTS(SELECT 1 FROM term_verdicts v LEFT JOIN actions a ON a.id=v.action_id
              WHERE v.term_id=?1 AND (v.batch_id IS NOT NULL OR (v.action_id<>?2 AND a.undone=0)))
            OR EXISTS(SELECT 1 FROM term_relation_records WHERE a=?1 OR b=?1)
            OR EXISTS(SELECT 1 FROM term_tree WHERE parent_id=?3 OR work_id=?3 OR merged_into=?3)
            OR EXISTS(SELECT 1 FROM reviews WHERE entity=?3)
            OR EXISTS(SELECT 1 FROM identity_scopes WHERE entity=?3 OR EXISTS(SELECT 1 FROM json_each(members_json) WHERE value=?3))
            OR EXISTS(SELECT 1 FROM identity_suspended WHERE entity=?3 OR EXISTS(SELECT 1 FROM json_each(options_json) WHERE value=?3))
            OR EXISTS(SELECT 1 FROM identity_uncertain WHERE EXISTS(SELECT 1 FROM json_each(options_json) WHERE value=?3))
            OR EXISTS(SELECT 1 FROM identity_anchor_decisions WHERE entity=?3)",
            params![object.id,action,object.key], |r|r.get(0))?;
        if !owner || independent || signature(c, object.id)? != object.signature {
            return Err(Error::Conflict("导入新建定义已有独立变化或依赖，无法安全撤销".into()));
        }
    }
    Ok(())
}

pub(crate) fn remove(c: &Connection, created: &[Created]) -> Result<()> {
    for object in created {
        // 物理定义和来源映射保留历史引用。名字、外链和投影撤去有效贡献，外链唯一归属随之释放。
        c.execute("DELETE FROM term_names WHERE term_id=?1", [object.id])?;
        c.execute("DELETE FROM term_links WHERE term_id=?1", [object.id])?;
        c.execute("DELETE FROM term_projection WHERE term_id=?1", [object.id])?;
    }
    Ok(())
}
