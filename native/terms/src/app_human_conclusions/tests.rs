use super::*;
use crate::{http::LibraryContext, http_json::Json};
use illustash_core::human_conclusions::state::{self, Domain, State};
use rusqlite::params;
use serde_json::json;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture {
    root: PathBuf,
    db: PathBuf,
    id: String,
}
impl Fixture {
    fn new(id: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "illustash-human-roundtrip-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let library = root.join("synthetic.pilib");
        crate::app_create::create_folder(&library, &root, Some("合成图库"), Some("other"), None).unwrap();
        let db = library.join("library.sqlite");
        Self { root, db, id: id.into() }
    }
    fn asset(&self, key: &str, filename: &str, color: u8) -> PathBuf {
        let path = self.root.join(filename);
        image::RgbImage::from_pixel(2, 2, image::Rgb([color, 10, 20])).save(&path).unwrap();
        let c = crate::open(&self.db).unwrap();
        c.execute(
            "INSERT INTO assets(uuid,filename,src_path,src_kind,added_at) VALUES (?1,?2,?3,'orig','2026-11-15')",
            params![key, filename, path.to_string_lossy()],
        )
        .unwrap();
        path
    }
    fn term(&self, key: &str, link: &str) {
        let c = crate::open(&self.db).unwrap();
        let term = crate::store::create_term(&c, crate::Kind::Artist, Some(key), "package:synthetic").unwrap();
        crate::store::add_link(&c, term, "danbooru", link, "", "synthetic").unwrap();
        crate::store::add_name(&c, term, "zh", "同名合成画师", "display", "synthetic").unwrap();
    }
    fn source(&self) {
        for (key, color) in [("a", 1), ("b", 2), ("c", 3), ("d", 4)] {
            self.asset(key, &format!("source-{key}.png"), color);
        }
        self.term("source-artist", "synthetic_artist");
        let mut c = crate::open(&self.db).unwrap();
        let tx = c.transaction().unwrap();
        crate::schema::begin_writing(&tx).unwrap();
        tx.execute("INSERT INTO actions(id,label,before_json) VALUES (1,'合成人工操作','[]')", []).unwrap();
        let term = crate::store::term_by_key(&tx, "source-artist").unwrap().unwrap().0;
        for (key, verdict) in [("a", "yes"), ("b", "no"), ("c", "clear")] {
            let asset = crate::store::library_asset(&tx, key).unwrap().unwrap();
            tx.execute("INSERT INTO term_verdicts(action_id,anchor,asset_id,term_id,verdict,origin,via,reason,created_at) VALUES (1,1,?1,?2,?3,'user','review','合成理由','2026-11-01')",
                params![asset,term,verdict]).unwrap();
        }
        tx.execute(
            "INSERT INTO curation(uuid,status,origin,created_at) VALUES ('a','included','review','2026-11-01')",
            [],
        )
        .unwrap();
        state::absent(&tx, Domain::Curation, "b", 1).unwrap();
        tx.execute("INSERT INTO deferred(uuid) VALUES ('c')", []).unwrap();
        state::absent(&tx, Domain::Deferred, "d", 1).unwrap();
        state::absent(&tx, Domain::Uncertain, "a", 1).unwrap();
        crate::schema::commit_writing(tx).unwrap();
    }
    fn target(&self) {
        for (key, color) in [("new-a", 1), ("new-b", 2), ("new-c", 3), ("new-d", 4)] {
            self.asset(key, &format!("moved-and-renamed-{color}.png"), color);
        }
        self.term("target-artist", "synthetic_artist");
        self.term("same-name-different-artist", "other_artist");
    }
    fn export(&self) -> PathBuf {
        let snapshot = capture(&self.db, &self.id, &|| Ok(())).unwrap();
        assert_eq!(snapshot.coverage().verified, 4);
        let path = self.root.join("synthetic.illustash-human.jsonl");
        snapshot.write(&mut File::create(&path).unwrap()).unwrap();
        let data = fs::read_to_string(&path).unwrap();
        assert!(!data.contains(self.root.to_string_lossy().as_ref()));
        assert!(!data.contains("source-a.png"));
        path
    }
    fn undo(&self, id: i64) -> Result<Json> {
        let context = LibraryContext {
            db: self.db.clone(),
            generation: "synthetic".into(),
            library: Some(self.root.clone()),
            source: None,
            tags: self.root.join("unused.csv"),
            token: Some("synthetic".into()),
            read_only: None,
        };
        crate::app_commands::request(
            &context,
            "/api/undo",
            &crate::http_json::parse_request(&json!({"id":id}).to_string()).unwrap(),
        )
    }
    fn count(&self, sql: &str) -> i64 {
        crate::open(&self.db).unwrap().query_row(sql, [], |r| r.get(0)).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn real_files_export_rebuild_import_and_ordinary_undo() {
    let source = Fixture::new("source-library");
    source.source();
    let path = source.export();
    let target = Fixture::new("rebuilt-library");
    target.target();
    let report = import_file(&target.db, &target.id, &path, &|| Ok(())).unwrap();
    assert_eq!(report.records, BTreeMap::from([("applied".into(), 8)]));
    assert_eq!(report.assets["matched"], 4);
    assert_eq!(report.action_id, Some(1));
    let c = crate::open(&target.db).unwrap();
    for (asset, verdict) in [("new-a", "yes"), ("new-b", "no"), ("new-c", "clear")] {
        let row:(String,String,String,String)=c.query_row("SELECT t.key,v.verdict,v.reason,v.created_at FROM term_verdicts v JOIN terms t ON t.id=v.term_id JOIN term_assets a ON a.id=v.asset_id WHERE a.key=?1",[asset],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).unwrap();
        assert_eq!(row, ("target-artist".into(), verdict.into(), "合成理由".into(), "2026-11-01".into()));
    }
    assert!(
        matches!(state::read(&c, Domain::Curation, "new-a").unwrap(), State::Positive(v) if v["status"]=="included")
    );
    for (domain, asset) in [(Domain::Curation, "new-b"), (Domain::Deferred, "new-d"), (Domain::Uncertain, "new-a")] {
        assert!(
            matches!(state::read(&c,domain,asset).unwrap(),State::Absent(m) if m.owner_action_id==1 && m.source_ref.is_some())
        );
    }
    assert!(matches!(state::read(&c, Domain::Deferred, "new-c").unwrap(), State::Positive(_)));
    let repeated = import_file(&target.db, &target.id, &path, &|| Ok(())).unwrap();
    assert_eq!(repeated.action_id, None);
    assert_eq!(repeated.records, BTreeMap::from([("already_present".into(), 8)]));
    assert_eq!(target.count("SELECT count(*) FROM actions"), 1);
    target.undo(1).unwrap();
    for (domain, asset) in [
        (Domain::Curation, "new-a"),
        (Domain::Curation, "new-b"),
        (Domain::Deferred, "new-c"),
        (Domain::Deferred, "new-d"),
        (Domain::Uncertain, "new-a"),
    ] {
        assert_eq!(state::read(&c, domain, asset).unwrap(), State::Undecided);
    }
    assert_eq!(target.count("SELECT count(*) FROM term_effective_verdicts"), 0);
    assert_eq!(target.count("SELECT count(*) FROM term_verdicts"), 3);
    assert_eq!(target.count("SELECT count(*) FROM terms"), 2);
    assert_eq!(target.count("SELECT count(*) FROM actions WHERE undone=0"), 0);
    let reapplied = import_file(&target.db, &target.id, &path, &|| Ok(())).unwrap();
    assert_eq!(reapplied.action_id, Some(2));
    assert_eq!(reapplied.records["applied"], 8);
    assert_eq!(target.count("SELECT count(*) FROM term_verdicts WHERE action_id=2"), 3);
}

#[test]
fn imported_definition_disappears_from_business_reads_and_reimport_owns_new_history() {
    let source = Fixture::new("source-library");
    source.source();
    let path = source.export();
    let target = Fixture::new("rebuilt-library");
    for (key, color) in [("new-a", 1), ("new-b", 2), ("new-c", 3), ("new-d", 4)] {
        target.asset(key, &format!("moved-{color}.png"), color);
    }
    let report = import_file(&target.db, &target.id, &path, &|| Ok(())).unwrap();
    assert_eq!(report.records["applied"], 8);
    assert_eq!(report.definitions["applied"], 1);
    let c = crate::open(&target.db).unwrap();
    let first: i64 = c.query_row("SELECT id FROM terms", [], |r| r.get(0)).unwrap();
    assert_eq!(import_file(&target.db, &target.id, &path, &|| Ok(())).unwrap().action_id, None);
    target.undo(report.action_id.unwrap()).unwrap();
    for table in ["terms", "term_relations", "term_names", "term_links", "term_projection", "term_effective_verdicts"] {
        assert_eq!(target.count(&format!("SELECT count(*) FROM {table}")), 0, "{table}");
    }
    assert_eq!(target.count("SELECT count(*) FROM term_records"), 1);
    assert_eq!(target.count("SELECT count(*) FROM term_verdicts"), 3);
    assert_eq!(target.count("SELECT count(*) FROM pragma_foreign_key_check"), 0);
    let empty = capture(&target.db, &target.id, &|| Ok(())).unwrap();
    let mut encoded = Vec::new();
    empty.write(&mut encoded).unwrap();
    assert_eq!(String::from_utf8(encoded).unwrap().lines().count(), 2);
    let repeated = import_file(&target.db, &target.id, &path, &|| Ok(())).unwrap();
    assert_eq!(repeated.action_id, Some(2));
    let second: i64 = c.query_row("SELECT id FROM terms", [], |r| r.get(0)).unwrap();
    assert_ne!(first, second);
    assert_eq!(target.count("SELECT count(*) FROM actions WHERE id=1 AND undone=1"), 1);
    assert_eq!(target.count("SELECT count(*) FROM term_verdicts WHERE action_id=1"), 3);
    assert_eq!(target.count("SELECT count(*) FROM term_effective_verdicts WHERE action_id=1"), 0);
    assert_eq!(target.count("SELECT count(*) FROM term_links WHERE key='synthetic_artist'"), 1);
}

#[test]
fn independent_evidence_keeps_imported_definition_and_rolls_back_entire_undo() {
    let source = Fixture::new("source-library");
    source.source();
    let path = source.export();
    let target = Fixture::new("rebuilt-library");
    for (key, color) in [("new-a", 1), ("new-b", 2), ("new-c", 3), ("new-d", 4)] {
        target.asset(key, &format!("moved-{color}.png"), color);
    }
    let report = import_file(&target.db, &target.id, &path, &|| Ok(())).unwrap();
    let mut c = crate::open(&target.db).unwrap();
    let tx = c.transaction().unwrap();
    crate::schema::begin_writing(&tx).unwrap();
    tx.execute(
        "INSERT INTO term_evidence(asset_id,term_id,source,method,version,fingerprint)
        SELECT a.id,t.id,'wd','synthetic','1',zeroblob(16) FROM term_assets a,terms t WHERE a.key='new-a'",
        [],
    )
    .unwrap();
    crate::schema::commit_writing(tx).unwrap();
    let before = capture(&target.db, &target.id, &|| Ok(())).unwrap().semantic_sha256().unwrap();
    assert!(matches!(target.undo(report.action_id.unwrap()), Err(Error::Conflict(_))));
    assert_eq!(capture(&target.db, &target.id, &|| Ok(())).unwrap().semantic_sha256().unwrap(), before);
    assert_eq!(target.count("SELECT count(*) FROM actions WHERE undone=0"), 1);
    assert_eq!(target.count("SELECT count(*) FROM curation"), 1);
    assert_eq!(target.count("SELECT count(*) FROM deferred"), 1);
    assert_eq!(target.count("SELECT count(*) FROM term_links"), 1);
}

#[test]
fn standalone_user_definition_imports_and_undoes_without_assets() {
    let source = Fixture::new("source-library");
    let c = crate::open(&source.db).unwrap();
    let id = crate::store::create_term(&c, crate::Kind::Concept, Some("synthetic-tag"), "user").unwrap();
    crate::store::add_name(&c, id, "zh", "合成标签", "alias", "user").unwrap();
    let path = source.root.join("standalone.illustash-human.jsonl");
    capture(&source.db, &source.id, &|| Ok(())).unwrap().write(&mut File::create(&path).unwrap()).unwrap();
    let target = Fixture::new("target-library");
    let report = import_file(&target.db, &target.id, &path, &|| Ok(())).unwrap();
    assert!(report.records.is_empty());
    assert_eq!(report.definitions["applied"], 1);
    assert_eq!(target.count("SELECT count(*) FROM term_names WHERE value='合成标签' AND source='user'"), 1);
    target.undo(report.action_id.unwrap()).unwrap();
    assert_eq!(target.count("SELECT count(*) FROM terms"), 0);
    assert_eq!(target.count("SELECT count(*) FROM term_records"), 1);
}

#[test]
fn target_human_state_wins_and_independent_change_refuses_undo_atomically() {
    let source = Fixture::new("source-library");
    source.source();
    let path = source.export();
    let target = Fixture::new("target-library");
    target.target();
    let c = crate::open(&target.db).unwrap();
    c.execute("INSERT INTO curation VALUES ('new-a','excluded','review','2026-11-10')", []).unwrap();
    let report = import_file(&target.db, &target.id, &path, &|| Ok(())).unwrap();
    assert_eq!(report.records["conflict"], 1);
    assert_eq!(report.records["applied"], 7);
    assert!(matches!(state::read(&c,Domain::Curation,"new-a").unwrap(),State::Positive(v) if v["status"]=="excluded"));
    c.execute("DELETE FROM deferred WHERE uuid='new-c'", []).unwrap();
    let before = capture(&target.db, &target.id, &|| Ok(())).unwrap().semantic_sha256().unwrap();
    assert!(matches!(target.undo(1), Err(Error::Conflict(_))));
    assert_eq!(capture(&target.db, &target.id, &|| Ok(())).unwrap().semantic_sha256().unwrap(), before);
    assert_eq!(target.count("SELECT count(*) FROM actions WHERE undone=0"), 1);
    assert_eq!(target.count("SELECT count(*) FROM human_state_marks"), 3);
}

#[test]
fn same_library_replaced_or_offline_id_cannot_redirect_conclusions_to_a_copy() {
    let source = Fixture::new("same-library");
    source.source();
    let path = source.export();
    let target = Fixture::new("same-library");
    let replaced = target.asset("a", "replaced.png", 9);
    target.asset("copy-a", "old-content-copy.png", 1);
    target.term("source-artist", "synthetic_artist");
    let changed = import_file(&target.db, &target.id, &path, &|| Ok(())).unwrap();
    assert_eq!(changed.action_id, None);
    assert_eq!(changed.assets["content_changed"], 1);
    assert_eq!(changed.records["content_changed"], 3);
    assert_eq!(target.count("SELECT count(*) FROM actions"), 0);
    fs::remove_file(replaced).unwrap();
    let offline = import_file(&target.db, &target.id, &path, &|| Ok(())).unwrap();
    assert_eq!(offline.action_id, None);
    assert_eq!(offline.records["unverified_asset"], 8);
    assert_eq!(offline.details.iter().filter(|d| d.reason.as_deref() == Some("same_id_unverified")).count(), 3);
    assert_eq!(target.count("SELECT count(*) FROM actions"), 0);
    assert_eq!(target.count("SELECT count(*) FROM term_verdicts"), 0);
    // 原 ID 真正离开候选集后，才允许按唯一内容认回。
    crate::open(&target.db).unwrap().execute("DELETE FROM assets WHERE uuid='a'", []).unwrap();
    let absent = import_file(&target.db, &target.id, &path, &|| Ok(())).unwrap();
    assert_eq!(absent.records["applied"], 3);
    assert_eq!(target.count("SELECT count(*) FROM curation WHERE uuid='copy-a'"), 1);
}

#[test]
fn generic_cli_refuses_human_import_without_partially_undoing_exclusion() {
    let source = Fixture::new("source-library");
    source.source();
    crate::open(&source.db).unwrap().execute("UPDATE curation SET status='excluded' WHERE uuid='a'", []).unwrap();
    let path = source.export();
    let target = Fixture::new("target-library");
    target.target();
    assert_eq!(import_file(&target.db, &target.id, &path, &|| Ok(())).unwrap().action_id, Some(1));
    assert_eq!(target.count("SELECT count(*) FROM excluded WHERE uuid='new-a'"), 1);
    let code = crate::cli::run(&[
        "undo".into(),
        "--db".into(),
        target.db.to_string_lossy().into_owned(),
        "--action".into(),
        "1".into(),
    ]);
    assert_eq!(code, 3);
    assert_eq!(target.count("SELECT count(*) FROM actions WHERE undone=0"), 1);
    assert_eq!(target.count("SELECT count(*) FROM curation WHERE status='excluded' AND uuid='new-a'"), 1);
    assert_eq!(target.count("SELECT count(*) FROM excluded WHERE uuid='new-a'"), 1);
    target.undo(1).unwrap();
    assert_eq!(target.count("SELECT count(*) FROM curation"), 0);
    assert_eq!(target.count("SELECT count(*) FROM excluded"), 0);
}

#[test]
fn incomplete_candidates_and_cancellation_do_not_create_partial_actions() {
    let source = Fixture::new("source-library");
    source.source();
    let path = source.export();
    let target = Fixture::new("target-library");
    target.target();
    let missing = target.asset("offline", "offline.png", 1);
    fs::remove_file(missing).unwrap();
    let report = import_file(&target.db, &target.id, &path, &|| Ok(())).unwrap();
    assert_eq!(report.records["unverified_asset"], 8);
    assert_eq!(report.action_id, None);
    assert_eq!(target.count("SELECT count(*) FROM actions"), 0);
    let c = crate::open(&target.db).unwrap();
    c.execute("DELETE FROM assets WHERE uuid='offline'", []).unwrap();
    let cancelled = std::cell::Cell::new(false);
    let check = || {
        // 读文件阶段的取消不产生空历史。
        if cancelled.replace(true) {
            Err(Error::Conflict("合成取消".into()))
        } else {
            Ok(())
        }
    };
    assert!(import_file(&target.db, &target.id, &path, &check).is_err());
    assert_eq!(target.count("SELECT count(*) FROM actions"), 0);
    assert_eq!(target.count("SELECT count(*) FROM term_verdicts"), 0);
    c.execute_batch("CREATE TRIGGER synthetic_failure BEFORE INSERT ON term_verdicts BEGIN SELECT RAISE(ABORT,'synthetic failure'); END").unwrap();
    assert!(import_file(&target.db, &target.id, &path, &|| Ok(())).is_err());
    // 裁决写入之前已处理过收录与跳过，后续 SQL 失败必须连同历史、标记一起回滚。
    for table in ["actions", "curation", "deferred", "human_state_marks", "human_state_mark_undo"] {
        assert_eq!(target.count(&format!("SELECT count(*) FROM {table}")), 0);
    }
}
