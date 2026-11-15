//! 只有导入新建定义受 action 贡献约束；不可变历史引用物理记录，现行读者统一读取有效视图。
use crate::{Error, Result};
use rusqlite::{Connection, OptionalExtension};

const TABLES: &str = "
CREATE TABLE human_object_contributions(
 kind TEXT NOT NULL CHECK(kind IN ('term','relation')),
 object_id INTEGER NOT NULL,
 owner_action_id INTEGER NOT NULL REFERENCES actions(id),
 definition_json TEXT NOT NULL DEFAULT '{}' CHECK(json_valid(definition_json)),
 PRIMARY KEY(kind,object_id));
CREATE INDEX human_object_contributions_owner ON human_object_contributions(owner_action_id);
CREATE TABLE human_entity_origins(
 source_library TEXT NOT NULL,
 source_key TEXT NOT NULL,
 target_key TEXT NOT NULL REFERENCES term_records(key),
 owner_action_id INTEGER NOT NULL REFERENCES actions(id),
 PRIMARY KEY(source_library,source_key,owner_action_id));
CREATE INDEX human_entity_origins_target ON human_entity_origins(target_key);
CREATE VIEW terms AS SELECT t.* FROM term_records t
 WHERE NOT EXISTS(SELECT 1 FROM human_object_contributions o JOIN actions a ON a.id=o.owner_action_id
   WHERE o.kind='term' AND o.object_id=t.id AND a.undone<>0);
CREATE VIEW term_relations AS SELECT r.* FROM term_relation_records r
 WHERE EXISTS(SELECT 1 FROM terms WHERE id=r.a) AND EXISTS(SELECT 1 FROM terms WHERE id=r.b)
 AND NOT EXISTS(SELECT 1 FROM human_object_contributions o JOIN actions a ON a.id=o.owner_action_id
   WHERE o.kind='relation' AND o.object_id=r.id AND a.undone<>0);
CREATE TRIGGER effective_terms_insert INSTEAD OF INSERT ON terms BEGIN
 INSERT INTO term_records(id,key,kind,origin,attrs,created_at)
 VALUES(NEW.id,NEW.key,NEW.kind,NEW.origin,coalesce(NEW.attrs,'{}'),coalesce(NEW.created_at,CURRENT_TIMESTAMP));
END;
CREATE TRIGGER effective_terms_update INSTEAD OF UPDATE ON terms BEGIN
 UPDATE term_records SET key=NEW.key,kind=NEW.kind,origin=NEW.origin,attrs=NEW.attrs,created_at=NEW.created_at WHERE id=OLD.id;
END;
CREATE TRIGGER effective_terms_delete INSTEAD OF DELETE ON terms BEGIN
 DELETE FROM term_records WHERE id=OLD.id;
END;
CREATE TRIGGER effective_relations_insert INSTEAD OF INSERT ON term_relations BEGIN
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM terms WHERE id=NEW.a)
   OR NOT EXISTS(SELECT 1 FROM terms WHERE id=NEW.b) THEN RAISE(ABORT,'relation endpoints must be active') END;
 INSERT INTO term_relation_records(id,a,b,kind,created_at)
 VALUES(NEW.id,NEW.a,NEW.b,NEW.kind,coalesce(NEW.created_at,CURRENT_TIMESTAMP));
END;
CREATE TRIGGER effective_relations_update INSTEAD OF UPDATE ON term_relations BEGIN
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM terms WHERE id=NEW.a)
   OR NOT EXISTS(SELECT 1 FROM terms WHERE id=NEW.b) THEN RAISE(ABORT,'relation endpoints must be active') END;
 UPDATE term_relation_records SET a=NEW.a,b=NEW.b,kind=NEW.kind,created_at=NEW.created_at WHERE id=OLD.id;
END;
CREATE TRIGGER effective_relations_delete INSTEAD OF DELETE ON term_relations BEGIN
 DELETE FROM term_relation_records WHERE id=OLD.id;
END;
";

pub(crate) fn migrate(c: &Connection) -> Result<()> {
    let legacy: bool = c.pragma_query_value(None, "legacy_alter_table", |r| r.get(0))?;
    c.pragma_update(None, "legacy_alter_table", true)?;
    // 正常连接开启 FK，重命名让不可变历史外键跟随物理记录；已有业务视图仍引用原有效名称。
    let result = c.execute_batch(
        "ALTER TABLE terms RENAME TO term_records;
         ALTER TABLE term_relations RENAME TO term_relation_records;",
    );
    c.pragma_update(None, "legacy_alter_table", legacy)?;
    result?;
    c.execute_batch(TABLES)?;
    crate::schema::guard(c, "human_entity_origins")?;
    // 只有导入所有者能建立或转移贡献。普通关系入口可重新使用已经撤销的关系记录，旧裁决仍属于 undone action。
    for event in ["INSERT", "UPDATE"] {
        c.execute_batch(&format!(
            "CREATE TRIGGER human_contribution_writer_{event} BEFORE {event} ON human_object_contributions
             WHEN NOT EXISTS(SELECT 1 FROM meta WHERE key='{}')
             BEGIN SELECT RAISE(ABORT,'human contributions are written by the terms core'); END;",
            crate::schema::WRITER_KEY
        ))?;
    }
    c.execute_batch(
        "CREATE TRIGGER human_contribution_delete BEFORE DELETE ON human_object_contributions
         WHEN EXISTS(SELECT 1 FROM actions WHERE id=OLD.owner_action_id AND undone=0)
         BEGIN SELECT RAISE(ABORT,'an active import contribution cannot be detached'); END;",
    )?;
    Ok(())
}

/// 名字维护等表触发器安装在物理记录上；旧版升级过程仍使用当时的物理表名。
pub(crate) fn storage<'a>(c: &Connection, table: &'a str) -> Result<&'a str> {
    if !matches!(table, "terms" | "term_relations") {
        return Ok(table);
    }
    let kind: Option<String> =
        c.query_row("SELECT type FROM sqlite_master WHERE name=?1", [table], |r| r.get(0)).optional()?;
    Ok(if kind.as_deref() == Some("view") {
        if table == "terms" {
            "term_records"
        } else {
            "term_relation_records"
        }
    } else {
        table
    })
}

/// 现行普通关系入口重新建立已撤销的关系；不会重新启用旧 action 或它的裁决。
pub(crate) fn ordinary_relation(c: &Connection, a: i64, b: i64, kind: &str) -> Result<Option<i64>> {
    if storage(c, "term_relations")? == "term_relations" {
        return Ok(None);
    }
    let raw: Option<i64> = c
        .query_row(
            "SELECT id FROM term_relation_records WHERE kind=?3 AND
         ((a=?1 AND b=?2) OR (?3='same_as' AND a=?2 AND b=?1))",
            rusqlite::params![a, b, kind],
            |r| r.get(0),
        )
        .optional()?;
    let Some(id) = raw else { return Ok(None) };
    let active: i64 = c.query_row("SELECT count(*) FROM terms WHERE id IN (?1,?2)", [a, b], |r| r.get(0))?;
    if active != 2 {
        return Err(Error::Conflict("关系端点已经失效".into()));
    }
    c.execute("DELETE FROM human_object_contributions WHERE kind='relation' AND object_id=?1", [id])?;
    Ok(Some(id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        schema::{self, Kind, RelationKind},
        store,
    };
    use rusqlite::params;

    #[test]
    fn inactive_import_definitions_leave_history_referentially_intact() {
        let mut c = Connection::open_in_memory().unwrap();
        crate::configure(&c).unwrap();
        crate::schema_rules::migrate_to(&mut c, schema::SCHEMA_VERSION).unwrap();
        let tx = c.transaction().unwrap();
        schema::begin_writing(&tx).unwrap();
        let original = store::create_term(&tx, Kind::Artist, Some("original"), "user").unwrap();
        let imported = store::create_term(&tx, Kind::Artist, Some("imported"), "user").unwrap();
        let asset = store::register_asset(&tx, "library", "a", None).unwrap();
        let relation = store::relation(&tx, original, imported, RelationKind::SameAs).unwrap();
        tx.execute("INSERT INTO actions(id,label,before_json) VALUES(1,'导入','[]')", []).unwrap();
        tx.execute(
            "INSERT INTO human_object_contributions(kind,object_id,owner_action_id) VALUES('term',?1,1),('relation',?2,1)",
            params![imported, relation],
        )
        .unwrap();
        tx.execute("INSERT INTO term_verdicts(action_id,anchor,asset_id,term_id,verdict,origin) VALUES(1,1,?1,?2,'yes','user')",params![asset,imported]).unwrap();
        tx.execute(
            "INSERT INTO term_verdicts(action_id,anchor,relation_id,verdict,origin) VALUES(1,1,?1,'yes','user')",
            [relation],
        )
        .unwrap();
        tx.execute("UPDATE actions SET undone=1 WHERE id=1", []).unwrap();
        assert!(store::term_by_key(&tx, "imported").unwrap().is_none());
        assert!(store::term_by_key(&tx, "original").unwrap().is_some());
        assert_eq!(tx.query_row("SELECT count(*) FROM term_relations", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        assert_eq!(tx.query_row("SELECT count(*) FROM term_records", [], |r| r.get::<_, i64>(0)).unwrap(), 2);
        assert_eq!(tx.query_row("SELECT count(*) FROM term_verdicts", [], |r| r.get::<_, i64>(0)).unwrap(), 2);
        assert_eq!(
            tx.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| r.get::<_, i64>(0)).unwrap(),
            0
        );
        let later = store::create_term(&tx, Kind::Artist, Some("later"), "user").unwrap();
        assert!(later > imported);
        tx.execute("UPDATE terms SET kind='concept' WHERE key='original'", []).unwrap();
        assert_eq!(store::term_by_key(&tx, "original").unwrap().unwrap().1, Kind::Concept);
        assert!(tx.execute("UPDATE terms SET key='changed' WHERE key='original'", []).is_err());
        schema::commit_writing(tx).unwrap();
    }

    #[test]
    fn normal_relation_creation_reuses_inactive_identity_without_reviving_old_verdict() {
        let mut c = Connection::open_in_memory().unwrap();
        crate::configure(&c).unwrap();
        crate::schema_rules::migrate_to(&mut c, schema::SCHEMA_VERSION).unwrap();
        let tx = c.transaction().unwrap();
        schema::begin_writing(&tx).unwrap();
        let a = store::create_term(&tx, Kind::Artist, Some("a"), "user").unwrap();
        let b = store::create_term(&tx, Kind::Artist, Some("b"), "user").unwrap();
        let relation = store::relation(&tx, a, b, RelationKind::SameAs).unwrap();
        tx.execute("INSERT INTO actions(id,label,before_json) VALUES(1,'导入','[]')", []).unwrap();
        tx.execute(
            "INSERT INTO human_object_contributions(kind,object_id,owner_action_id) VALUES('relation',?1,1)",
            [relation],
        )
        .unwrap();
        tx.execute(
            "INSERT INTO term_verdicts(action_id,anchor,relation_id,verdict,origin) VALUES(1,1,?1,'yes','user')",
            [relation],
        )
        .unwrap();
        assert!(tx.execute("DELETE FROM human_object_contributions", []).is_err());
        tx.execute("UPDATE actions SET undone=1 WHERE id=1", []).unwrap();
        schema::commit_writing(tx).unwrap();
        assert_eq!(store::relation(&c, a, b, RelationKind::SameAs).unwrap(), relation);
        assert_eq!(
            c.query_row("SELECT state FROM term_relation_states WHERE id=?1", [relation], |r| r.get::<_, String>(0))
                .unwrap(),
            "maybe"
        );
        assert_eq!(c.query_row("SELECT count(*) FROM term_verdicts", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
        assert_eq!(c.query_row("SELECT count(*) FROM term_effective_verdicts", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    }
}
