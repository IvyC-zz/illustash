//! 客户端只链接 Core + rusqlite，在进程内调用公开 API。
#![allow(clippy::unwrap_used, reason = "集成测试里 unwrap 就是断言")]
use illustash_core::{
    backup::RequiredBackup,
    package::{Package, FORMAT},
    schema, store, strict_json, Error, Kind,
};
use rusqlite::Connection;
use serde_json::json;

fn memory() -> Connection {
    let mut c = Connection::open_in_memory().unwrap();
    illustash_core::configure(&c).unwrap();
    let storage = RequiredBackup::new(
        |_, _| panic!("memory must not reserve"),
        |_| panic!("memory must not open"),
        |_| panic!("memory must not clean"),
    );
    schema::migrate_with_backup(&mut c, &storage).unwrap();
    c
}
fn scalar(c: &Connection, sql: &str) -> i64 {
    c.query_row(sql, [], |r| r.get(0)).unwrap()
}

#[test]
fn evidence_human_projection_cross_type_undo_and_atomic_failure() {
    let mut c = memory();
    let asset = store::register_asset(&c, "synthetic", "A", None).unwrap();
    let row = json!({"id":"e1","asset":{"namespace":"synthetic","id":"A"},
        "term":{"kind":"artist","link":{"system":"danbooru","key":"synthetic_artist"},"name":"Synthetic Artist"},
        "source":"provenance","method":"iqdb","version":"synthetic-v1","score":{"value":0.0,"metric":"hamming_distance"},
        "observed_at":"2026-10-03T00:00:00Z","data":{}});
    let evidence = strict_json::canonical(&row) + "\n";
    let manifest = json!({"format":FORMAT,"library_id":"synthetic-lib","producer":{"name":"provenance","version":"synthetic-v1"},
        "created_at":"2026-10-03T00:00:00Z","assets":[{"asset":{"namespace":"synthetic","id":"A"},"status":"completed","error":null}],
        "files":{"evidence.jsonl":{"bytes":evidence.len(),"sha256":strict_json::sha256_hex(evidence.as_bytes())}}});
    let pkg = Package::from_parts(&strict_json::canonical(&manifest), &evidence).unwrap();
    store::import_package(&mut c, &pkg, "synthetic-lib").unwrap();
    let artist = store::term_by_link(&c, "danbooru", "synthetic_artist").unwrap().unwrap();
    assert_eq!(store::state(&c, asset, artist).unwrap().as_deref(), Some("machine"));
    let label = store::create_term(&c, Kind::Label, Some("synthetic-label"), "user").unwrap();
    let a = store::judge(
        &mut c,
        &[store::VerdictTarget::Image { asset, term: artist }],
        store::Verdict::Yes,
        "user",
        "",
        "artist yes",
    )
    .unwrap();
    let b = store::judge(
        &mut c,
        &[store::VerdictTarget::Image { asset, term: label }],
        store::Verdict::Yes,
        "user",
        "",
        "label yes",
    )
    .unwrap();
    assert_eq!(store::state(&c, asset, artist).unwrap().as_deref(), Some("confirmed"));
    let before = (
        scalar(&c, "SELECT COUNT(*) FROM actions"),
        scalar(&c, "SELECT COUNT(*) FROM term_verdicts"),
        scalar(&c, "SELECT COUNT(*) FROM term_projection"),
    );
    assert!(store::judge(
        &mut c,
        &[store::VerdictTarget::Image { asset, term: artist }, store::VerdictTarget::Image { asset, term: 999999 }],
        store::Verdict::No,
        "user",
        "",
        "invalid second target"
    )
    .is_err());
    assert_eq!(
        before,
        (
            scalar(&c, "SELECT COUNT(*) FROM actions"),
            scalar(&c, "SELECT COUNT(*) FROM term_verdicts"),
            scalar(&c, "SELECT COUNT(*) FROM term_projection")
        )
    );
    assert!(matches!(store::undo(&mut c, Some(a)), Err(Error::Conflict(_))));
    assert!(matches!(store::undo(&mut c,Some(b)).unwrap(),store::Undo::Undone{action,..} if action==b));
    assert_eq!(store::state(&c, asset, label).unwrap(), None);
    assert_eq!(store::state(&c, asset, artist).unwrap().as_deref(), Some("confirmed"));
    assert!(matches!(store::undo(&mut c,Some(a)).unwrap(),store::Undo::Undone{action,..} if action==a));
    assert_eq!(store::state(&c, asset, artist).unwrap().as_deref(), Some("machine"));
    assert_eq!(scalar(&c, "SELECT COUNT(*) FROM term_evidence"), 1);
    assert_eq!(scalar(&c, "SELECT COUNT(*) FROM term_verdicts"), 2);
}

#[test]
fn a_storage_adapter_cannot_acknowledge_a_file_backup_with_a_memory_connection() {
    let root = std::env::temp_dir().join(format!("illustash-core-noop-{}", store::new_key().unwrap()));
    std::fs::create_dir(&root).unwrap();
    let mut c = Connection::open(root.join("synthetic.sqlite")).unwrap();
    illustash_core::configure(&c).unwrap();
    let fake = RequiredBackup::new(
        |source, _| Ok(source.with_file_name("claimed-backup.sqlite")),
        |_| Connection::open_in_memory().map_err(Error::from),
        |_| {},
    );
    schema::migrate_to_with_backup(&mut c, 5, &fake).unwrap();
    let before = scalar(&c, "SELECT COUNT(*) FROM sqlite_master");
    assert!(schema::prepare_upgrade_backup(&c, 5, &fake).is_err());
    assert!(schema::migrate_with_backup(&mut c, &fake).is_err());
    assert_eq!(schema::version(&c).unwrap(), 5);
    assert_eq!(scalar(&c, "SELECT COUNT(*) FROM sqlite_master"), before);
    assert!(!root.join("claimed-backup.sqlite").exists());
    drop(c);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn prepared_upgrade_refuses_invalid_targets_before_any_schema_write() {
    let mut c = Connection::open_in_memory().unwrap();
    illustash_core::configure(&c).unwrap();
    let storage = RequiredBackup::new(
        |_, _| panic!("memory must not reserve"),
        |_| panic!("memory must not open"),
        |_| panic!("memory must not clean"),
    );
    let prepared = schema::prepare_upgrade_backup(&c, 0, &storage).unwrap();
    let tx = c.transaction().unwrap();
    let before = scalar(&tx, "SELECT COUNT(*) FROM sqlite_master");
    assert_eq!(before, 0);
    for target in [-1, schema::SCHEMA_VERSION + 1, i64::MAX] {
        assert!(matches!(schema::migrate_prepared_in(&tx, target, &prepared), Err(Error::Invalid(_))));
        assert_eq!(scalar(&tx, "SELECT COUNT(*) FROM sqlite_master"), before);
        assert_eq!(schema::version(&tx).unwrap(), 0);
    }
    // 被拒绝的请求之后，调用方持有的事务仍然可用。
    assert_eq!(schema::migrate_prepared_in(&tx, schema::SCHEMA_VERSION, &prepared).unwrap(), 0);
    tx.commit().unwrap();
    assert_eq!(schema::version(&c).unwrap(), schema::SCHEMA_VERSION);
}

#[test]
fn identity_retirement_uses_a_prepared_backup_and_keeps_the_original_step() {
    let mut c = memory();
    c.execute_batch("CREATE TABLE identity_claims(value); INSERT INTO identity_claims VALUES ('synthetic'); CREATE TABLE identity_calibration(value); INSERT INTO identity_calibration VALUES ('synthetic'); INSERT INTO meta(key,value) VALUES ('identity_calibration_active','synthetic'),('identity_bank_error','synthetic'),('unrelated','retain');").unwrap();
    let storage = RequiredBackup::new(
        |_, _| panic!("memory must not reserve"),
        |_| panic!("memory must not open"),
        |_| panic!("memory must not clean"),
    );
    let prepared = schema::prepare_upgrade_backup(&c, schema::SCHEMA_VERSION, &storage).unwrap();
    let tx = c.transaction().unwrap();
    schema::retire_identity_calibration_in(&tx, &prepared).unwrap();
    assert_eq!(
        scalar(&tx, "SELECT COUNT(*) FROM sqlite_master WHERE name IN ('identity_claims','identity_calibration')"),
        0
    );
    assert_eq!(
        scalar(&tx, "SELECT COUNT(*) FROM meta WHERE key IN ('identity_calibration_active','identity_bank_error')"),
        0
    );
    assert_eq!(scalar(&tx, "SELECT COUNT(*) FROM meta WHERE key='unrelated' AND value='retain'"), 1);
    tx.rollback().unwrap();
    assert_eq!(scalar(&c, "SELECT COUNT(*) FROM identity_claims"), 1);
    assert_eq!(scalar(&c, "SELECT COUNT(*) FROM identity_calibration"), 1);
}
