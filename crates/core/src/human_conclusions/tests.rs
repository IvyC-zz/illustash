use super::state::{self, Domain, State};
use crate::schema;
use rusqlite::Connection;
use serde_json::json;

fn fixture(version: i64) -> Connection {
    let mut c = Connection::open_in_memory().unwrap();
    crate::configure(&c).unwrap();
    c.execute_batch(
        "CREATE TABLE curation(uuid TEXT PRIMARY KEY,status TEXT NOT NULL,origin TEXT NOT NULL,created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
         CREATE TABLE deferred(uuid TEXT PRIMARY KEY);
         CREATE TABLE identity_uncertain(uuid TEXT PRIMARY KEY,options_json TEXT NOT NULL,reason TEXT NOT NULL,created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);"
    ).unwrap();
    crate::schema_rules::migrate_to(&mut c, version).unwrap();
    c
}

fn action(c: &Connection, body: &str) -> i64 {
    named_action(c, "合成操作", body)
}

fn named_action(c: &Connection, label: &str, body: &str) -> i64 {
    c.execute("INSERT INTO actions(label,before_json) VALUES (?1,?2)", [label, body]).unwrap();
    c.last_insert_rowid()
}

#[test]
fn absent_is_persistent_and_undo_restores_all_three_states() {
    let mut c = fixture(schema::SCHEMA_VERSION);
    let tx = c.transaction().unwrap();
    schema::begin_writing(&tx).unwrap();
    let reset = action(&tx, "[]");
    state::absent(&tx, Domain::Curation, "a", reset).unwrap();
    schema::commit_writing(tx).unwrap();
    assert!(matches!(state::read(&c, Domain::Curation, "a").unwrap(), State::Absent(m) if m.owner_action_id == reset));
    assert_eq!(state::read(&c, Domain::Curation, "b").unwrap(), State::Undecided);

    let tx = c.transaction().unwrap();
    schema::begin_writing(&tx).unwrap();
    let include = action(&tx, "[]");
    tx.execute("INSERT INTO curation(uuid,status,origin) VALUES ('a','included','review')", []).unwrap();
    state::present(&tx, Domain::Curation, "a", include).unwrap();
    schema::commit_writing(tx).unwrap();
    assert!(matches!(state::read(&c, Domain::Curation, "a").unwrap(), State::Positive(v) if v["status"] == "included"));

    let tx = c.transaction().unwrap();
    schema::begin_writing(&tx).unwrap();
    tx.execute("DELETE FROM curation", []).unwrap();
    tx.execute("UPDATE actions SET undone=1 WHERE id=?1", [include]).unwrap();
    state::undo(&tx, include).unwrap();
    schema::commit_writing(tx).unwrap();
    assert!(matches!(state::read(&c, Domain::Curation, "a").unwrap(), State::Absent(m) if m.owner_action_id == reset));

    let tx = c.transaction().unwrap();
    schema::begin_writing(&tx).unwrap();
    tx.execute("UPDATE actions SET undone=1 WHERE id=?1", [reset]).unwrap();
    state::undo(&tx, reset).unwrap();
    schema::commit_writing(tx).unwrap();
    assert_eq!(state::read(&c, Domain::Curation, "a").unwrap(), State::Undecided);
}

#[test]
fn one_action_keeps_initial_before_and_final_after_and_guards_foreign_writes() {
    let mut c = fixture(schema::SCHEMA_VERSION);
    let tx = c.transaction().unwrap();
    schema::begin_writing(&tx).unwrap();
    let id = action(&tx, "[]");
    for domain in [Domain::Curation, Domain::Deferred, Domain::Uncertain] {
        state::absent(&tx, domain, "a", id).unwrap();
        state::present(&tx, domain, "a", id).unwrap();
        state::absent(&tx, domain, "a", id).unwrap();
    }
    assert_eq!(
        tx.query_row("SELECT count(*) FROM human_state_mark_undo WHERE before_json='null'", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        3
    );
    schema::commit_writing(tx).unwrap();
    assert!(c.execute("DELETE FROM human_state_marks", []).is_err());
    assert!(state::absent(&c, Domain::Curation, "a", id).is_err());
    let tx = c.transaction().unwrap();
    schema::begin_writing(&tx).unwrap();
    state::undo(&tx, id).unwrap();
    assert_eq!(tx.query_row("SELECT count(*) FROM human_state_marks", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
}

#[test]
fn revision_tracks_mark_changes_and_rolls_back_with_them() {
    let mut c = fixture(schema::SCHEMA_VERSION);
    let revision = |c: &Connection| {
        c.query_row("SELECT CAST(value AS INTEGER) FROM meta WHERE key='human_revision'", [], |r| r.get::<_, i64>(0))
            .unwrap()
    };
    assert_eq!(revision(&c), 0);
    let tx = c.transaction().unwrap();
    schema::begin_writing(&tx).unwrap();
    let id = action(&tx, "[]");
    state::absent(&tx, Domain::Curation, "a", id).unwrap();
    let changed = revision(&tx);
    assert!(changed > 0);
    state::absent(&tx, Domain::Curation, "a", id).unwrap();
    assert_eq!(revision(&tx), changed);
    schema::commit_writing(tx).unwrap();
    let tx = c.transaction().unwrap();
    schema::begin_writing(&tx).unwrap();
    state::present(&tx, Domain::Curation, "a", id).unwrap();
    assert!(revision(&tx) > changed);
    tx.rollback().unwrap();
    assert_eq!(revision(&c), changed);
    assert!(matches!(state::read(&c, Domain::Curation, "a").unwrap(), State::Absent(_)));
}

#[test]
fn changed_mark_rejects_undo_and_the_callers_transaction_restores_every_row() {
    let mut c = fixture(schema::SCHEMA_VERSION);
    let tx = c.transaction().unwrap();
    schema::begin_writing(&tx).unwrap();
    let id = action(&tx, "[]");
    state::absent(&tx, Domain::Deferred, "a", id).unwrap();
    state::absent(&tx, Domain::Uncertain, "a", id).unwrap();
    schema::commit_writing(tx).unwrap();
    let tx = c.transaction().unwrap();
    schema::begin_writing(&tx).unwrap();
    tx.execute("UPDATE human_state_marks SET source_ref='changed' WHERE domain='uncertain'", []).unwrap();
    schema::commit_writing(tx).unwrap();
    let tx = c.transaction().unwrap();
    schema::begin_writing(&tx).unwrap();
    assert!(state::undo(&tx, id).is_err());
    tx.rollback().unwrap();
    assert_eq!(c.query_row("SELECT count(*) FROM human_state_marks", [], |r| r.get::<_, i64>(0)).unwrap(), 2);
}

#[test]
fn v8_curation_backfill_supports_continuous_old_undo_without_forging_unknown_absence() {
    let mut c = fixture(8);
    let first = named_action(&c, "已将 1 张交回自动规则", &json!([{"uuid":"a","curation":null}]).to_string());
    let second = named_action(&c, "已收录 1 张", &json!([{"uuid":"a","curation":null}]).to_string());
    let third = named_action(
        &c,
        "已将 1 张交回自动规则",
        &json!([{"uuid":"a","curation":{"status":"included","origin":"review","created_at":"old"}}]).to_string(),
    );
    let unknown = action(&c, &json!([{"uuid":"unknown","entity":null,"assertion":null,"deferred":false,"identity":{"identity_uncertain":null}}]).to_string());
    let undone = action(&c, &json!([{"uuid":"undone","curation":null}]).to_string());
    c.execute("UPDATE actions SET undone=1 WHERE id=?1", [undone]).unwrap();
    crate::schema_rules::migrate_to(&mut c, schema::SCHEMA_VERSION).unwrap();
    assert!(matches!(state::read(&c, Domain::Curation, "a").unwrap(), State::Absent(m) if m.owner_action_id == third));
    for domain in [Domain::Curation, Domain::Deferred, Domain::Uncertain] {
        assert_eq!(state::read(&c, domain, "unknown").unwrap(), State::Undecided);
        assert_eq!(state::read(&c, domain, "undone").unwrap(), State::Undecided);
    }
    let tx = c.transaction().unwrap();
    schema::begin_writing(&tx).unwrap();
    tx.execute("UPDATE actions SET undone=1 WHERE id=?1", [unknown]).unwrap();
    state::undo(&tx, unknown).unwrap();
    tx.execute("INSERT INTO curation VALUES ('a','included','review','old')", []).unwrap();
    state::undo(&tx, third).unwrap();
    tx.execute("UPDATE actions SET undone=1 WHERE id=?1", [third]).unwrap();
    assert!(matches!(state::read(&tx, Domain::Curation, "a").unwrap(), State::Positive(_)));
    tx.execute("DELETE FROM curation", []).unwrap();
    state::undo(&tx, second).unwrap();
    tx.execute("UPDATE actions SET undone=1 WHERE id=?1", [second]).unwrap();
    assert!(matches!(state::read(&tx, Domain::Curation, "a").unwrap(), State::Absent(m) if m.owner_action_id == first));
    state::undo(&tx, first).unwrap();
    tx.execute("UPDATE actions SET undone=1 WHERE id=?1", [first]).unwrap();
    schema::commit_writing(tx).unwrap();
    assert_eq!(state::read(&c, Domain::Curation, "a").unwrap(), State::Undecided);
    assert_eq!(c.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
}

#[test]
fn v8_only_provable_deferred_and_uncertain_clears_are_backfilled() {
    let mut c = fixture(8);
    let cleared = named_action(&c, "已将 2 张设为 合成身份", &json!([{"terms":{"v":2,"type":"snapshot","before":[
        {"uuid":"known","entity":null,"assertion":null,"deferred":true,"identity":{"identity_scopes":null,"identity_suspended":null,"identity_uncertain":["[\"a\",\"b\"]","reason","old"]}},
        {"uuid":"empty","entity":null,"assertion":null,"deferred":false,"identity":{"identity_scopes":null,"identity_suspended":null,"identity_uncertain":null}}
    ]}}]).to_string());
    crate::schema_rules::migrate_to(&mut c, schema::SCHEMA_VERSION).unwrap();
    for domain in [Domain::Deferred, Domain::Uncertain] {
        assert!(matches!(state::read(&c, domain, "known").unwrap(), State::Absent(m) if m.owner_action_id == cleared));
        assert_eq!(state::read(&c, domain, "empty").unwrap(), State::Undecided);
    }
}

#[test]
fn malformed_unknown_and_inconsistent_history_never_fabricates_absence() {
    let mut c = fixture(8);
    let invalid = [
        json!(123),
        json!({"status":"included"}),
        json!({"status":"other","origin":"review","created_at":"old"}),
        json!({"status":"included","origin":null,"created_at":"old"}),
        json!({"status":"included","origin":"review","created_at":123}),
        json!({"status":"included","origin":"review","created_at":"old","unknown":true}),
    ];
    for (i, before) in invalid.into_iter().enumerate() {
        let key = format!("bad-{i}");
        // 后来的未知载荷也阻断更早的 reset，不能隔着这个边界保留推定结论。
        named_action(&c, "已将 1 张交回自动规则", &json!([{"uuid":key,"curation":null}]).to_string());
        named_action(&c, "已将 1 张交回自动规则", &json!([{"uuid":key,"curation":before}]).to_string());
    }
    for (label, key) in [("合成未知操作", "unknown"), ("已收录 1 张", "inconsistent")] {
        named_action(&c, label, &json!([{"uuid":key,"curation":null}]).to_string());
    }
    for (i, identity) in [
        json!({"identity_uncertain": ["[]","reason","old"]}),
        json!({"identity_scopes":null,"identity_suspended":null,"identity_uncertain":[]}),
        json!({"identity_scopes":null,"identity_suspended":null,"identity_uncertain":["invalid","reason","old"]}),
        json!({"identity_scopes":null,"identity_suspended":null,"identity_uncertain":["[]",123,"old"]}),
    ]
    .into_iter()
    .enumerate()
    {
        named_action(
            &c,
            "已恢复 1 张",
            &json!([{"terms":{"v":2,"type":"snapshot","before":[
                {"uuid":format!("identity-{i}"),"entity":null,"assertion":null,"deferred":true,"identity":identity}
            ]}}])
            .to_string(),
        );
    }
    // sweep 只写确认与复核，不取消跳过；后续空行不能归因于 sweep。
    named_action(
        &c,
        "已将 合成身份 标为已检查（1 张）",
        &json!([
            {"uuid":"sweep","entity":"e","assertion":null,"deferred":true}
        ])
        .to_string(),
    );
    crate::schema_rules::migrate_to(&mut c, schema::SCHEMA_VERSION).unwrap();
    for table in ["human_state_marks", "human_state_mark_undo"] {
        assert_eq!(c.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    }
}

#[test]
fn genuine_legacy_resume_payload_and_v2_envelope_preserve_explicit_empty_resumes() {
    let mut c = fixture(8);
    let snapshot = |key| json!({"uuid":key,"entity":null,"assertion":null,"deferred":false});
    let legacy = named_action(&c, "已恢复 1 张", &json!([snapshot("legacy")]).to_string());
    let current = named_action(
        &c,
        "已恢复 1 张",
        &json!([{"terms":{"v":2,"type":"snapshot","before":[snapshot("v2")]}}]).to_string(),
    );
    crate::schema_rules::migrate_to(&mut c, schema::SCHEMA_VERSION).unwrap();
    for (key, action) in [("legacy", legacy), ("v2", current)] {
        assert!(
            matches!(state::read(&c, Domain::Deferred, key).unwrap(), State::Absent(m) if m.owner_action_id == action)
        );
    }
}
