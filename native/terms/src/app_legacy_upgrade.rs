//! Historical application/source upgrades are built off-line and published in one attached,
//! rollback-journal transaction. Original databases stay locked and unchanged until publication.
use crate::{app_library::Library, Error, Result};
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior};
use std::{fs, path::Path};

fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}
fn meta(c: &Connection, key: &str) -> Result<Option<String>> {
    if !c.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='meta')", [], |r| {
        r.get::<_, bool>(0)
    })? {
        return Ok(None);
    }
    Ok(c.query_row("SELECT value FROM meta WHERE key=?", [key], |r| r.get(0)).optional()?)
}
pub(crate) fn validate(c: &Connection) -> Result<()> {
    crate::app_library::check_database(c)?;
    let version = meta(c, "schema_version")?;
    if let Some(v) = &version {
        if !(1..=36).any(|n| v == &n.to_string()) {
            return Err(Error::Schema("应用库版本较新或无效，已停止迁移".into()));
        }
    }
    if version.as_deref() != Some("36") && meta(c, "terms_characters")?.as_deref() == Some("core") {
        return Err(Error::Schema("角色已切到词条核心：旧应用 schema 不允许修改角色表".into()));
    }
    let terms = crate::schema::version(c)?;
    if !(0..=crate::SCHEMA_VERSION).contains(&terms) {
        return Err(Error::Schema("词条版本较新，已停止迁移".into()));
    }
    for key in ["terms_artists", "terms_labels", "terms_characters", "terms_names"] {
        if let Some(v) = meta(c, key)? {
            if v != "core" {
                return Err(Error::Schema(format!("未完成的切换标记：{key}")));
            }
        }
    }
    if !matches!(meta(c, "identity_policy")?.as_deref(), None | Some("") | Some("hybrid") | Some("v2")) {
        return Err(Error::Schema("未知身份策略，已停止迁移".into()));
    }
    let mut guarded = Vec::new();
    if terms >= 2 {
        guarded.extend_from_slice(&crate::schema::PRIVATE);
    }
    if meta(c, "terms_characters")?.as_deref() == Some("core") {
        guarded.extend_from_slice(&crate::legacy_characters::GUARDED);
    }
    if meta(c, "terms_names")?.as_deref() == Some("core") {
        guarded.extend_from_slice(&crate::legacy_names::GUARDED);
    }
    {
        for table in guarded {
            for event in ["insert", "update", "delete"] {
                let name = format!("{table}_writer_{event}");
                if !c.query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='trigger' AND name=? AND tbl_name=?)",
                    [name.as_str(), table],
                    |r| r.get::<_, bool>(0),
                )? {
                    return Err(Error::Schema(format!("核心写保护未完成：{name}")));
                }
            }
        }
    }
    Ok(())
}
pub(crate) fn required(lib: &Library) -> Result<bool> {
    let c = crate::app_library::read_connection(&lib.db())?;
    for key in ["indexed", "general_indexed", "rating_indexed", "names_version"] {
        if meta(&c, key)?.is_none() {
            return Ok(true);
        }
    }
    let s = crate::app_library::read_connection(&lib.source())?;
    Ok(s.prepare("SELECT source_id,preset FROM assets LIMIT 0").is_err()
        || s.prepare("SELECT asset,system,key,source_id,current FROM asset_links LIMIT 0").is_err()
        || s.prepare("SELECT source_id,path,size,mtime_ns,md5,asset FROM files LIMIT 0").is_err())
}
fn copy_db(from: &Path, to: &Path) -> Result<()> {
    let source = crate::app_library::read_connection(from)?;
    let mut target = crate::sqlite::open(to)?;
    let b = rusqlite::backup::Backup::new(&source, &mut target)?;
    loop {
        match b.step(256)? {
            rusqlite::backup::StepResult::Done => break,
            rusqlite::backup::StepResult::More => {}
            _ => return Err(Error::Conflict("升级备份被占用，未执行迁移".into())),
        }
    }
    drop(b);
    target.pragma_update(None, "journal_mode", "DELETE")?;
    crate::app_library::check_database(&target)
}
/// Copy schema and every cell, retaining implicit rowids. All destructive operations are on
/// the original's single attached transaction; readers never observe a partially published pair.
fn publish(c: &Connection, dst: &str, src: &str) -> Result<()> {
    let objects: Vec<(String,String)> = c.prepare(&format!("SELECT type,name FROM {dst}.sqlite_schema WHERE name NOT LIKE 'sqlite_%' ORDER BY CASE type WHEN 'trigger' THEN 0 WHEN 'view' THEN 1 WHEN 'index' THEN 2 ELSE 3 END"))?
        .query_map([],|r|Ok((r.get(0)?,r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
    for (kind, name) in objects {
        c.execute_batch(&format!("DROP {kind} {dst}.{}", quote(&name)))?;
    }
    let schema: Vec<(String,String,String)> = c.prepare(&format!("SELECT type,name,sql FROM {src}.sqlite_schema WHERE sql IS NOT NULL AND name NOT LIKE 'sqlite_%' ORDER BY rowid"))?
        .query_map([],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?.collect::<rusqlite::Result<_>>()?;
    for (kind, name, sql) in schema.iter().filter(|(kind, _, _)| kind == "table") {
        let _ = kind;
        c.execute_batch(&qualified(sql, dst, "table")?)?;
        let cols: Vec<String> = c
            .prepare(&format!("PRAGMA {src}.table_info({})", quote(name)))?
            .query_map([], |r| r.get(1))?
            .collect::<rusqlite::Result<_>>()?;
        let mut cols = cols.iter().map(|s| quote(s)).collect::<Vec<_>>();
        if !sql.to_uppercase().contains("WITHOUT ROWID") {
            cols.insert(0, "rowid".into());
        }
        let cols = cols.join(",");
        c.execute_batch(&format!(
            "INSERT INTO {dst}.{}({cols}) SELECT {cols} FROM {src}.{}",
            quote(name),
            quote(name)
        ))?;
    }
    let sequence: bool = c.query_row(
        &format!("SELECT EXISTS(SELECT 1 FROM {src}.sqlite_schema WHERE name='sqlite_sequence')"),
        [],
        |r| r.get(0),
    )?;
    if sequence {
        c.execute_batch(&format!(
            "DELETE FROM {dst}.sqlite_sequence; INSERT INTO {dst}.sqlite_sequence SELECT * FROM {src}.sqlite_sequence;"
        ))?;
    }
    for (kind, _, sql) in schema.iter().filter(|(kind, _, _)| kind != "table") {
        c.execute_batch(&qualified(sql, dst, kind)?)?;
    }
    Ok(())
}
fn qualified(sql: &str, dst: &str, kind: &str) -> Result<String> {
    let upper = sql.to_uppercase();
    let end = upper.find(&kind.to_uppercase()).ok_or_else(|| Error::Schema("无法发布升级结构".into()))? + kind.len();
    let rest = sql[end..].trim_start();
    let rest = if rest.to_uppercase().starts_with("IF NOT EXISTS ") { &rest[14..] } else { rest };
    Ok(format!("{} {dst}.{rest}", &sql[..end]))
}

pub(crate) fn upgrade(
    lib: &Library,
) -> std::result::Result<crate::app_upgrade::UpgradeReport, crate::app_upgrade::UpgradeFailure> {
    upgrade_with(lib, |_| Ok(()))
}
fn upgrade_with<F: FnOnce(&Connection) -> Result<()>>(
    lib: &Library,
    before_commit: F,
) -> std::result::Result<crate::app_upgrade::UpgradeReport, crate::app_upgrade::UpgradeFailure> {
    let mut backup = None;
    let mut committed = false;
    let result = (|| -> Result<crate::app_upgrade::UpgradeReport> {
        let current = Library::open_for_upgrade(lib.path())?;
        if current.config() != lib.config() || current.instance_path() != lib.instance_path() {
            return Err(Error::Conflict("升级预检后图库包已改变".into()));
        }
        let mut c = crate::sqlite::open_with_flags(lib.db(), OpenFlags::SQLITE_OPEN_READ_WRITE)?;
        c.busy_timeout(std::time::Duration::from_secs(5))?;
        // Publication reconstructs a complete, validated schema including its FK graph.
        // SQLite DROP TABLE performs implicit deletes; do not apply those to old child rows.
        c.pragma_update(None, "foreign_keys", false)?;
        c.execute(
            "ATTACH DATABASE ? AS original_source",
            [crate::sqlite::path(lib.source()).to_str().ok_or_else(|| Error::Invalid("来源路径不是 UTF-8".into()))?],
        )?;
        // Super-journal provides atomic crash recovery across both files; WAL cannot do so.
        c.pragma_update(None, "journal_mode", "DELETE")?;
        c.pragma_update(Some(rusqlite::DatabaseName::Attached("original_source")), "journal_mode", "DELETE")?;
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        validate(&tx)?;
        let before = crate::schema::version(&tx)?;
        let root = lib.path().join("library-backups").join(format!("application-before-{}", crate::store::new_key()?));
        fs::create_dir_all(lib.path().join("library-backups"))?;
        fs::create_dir(&root)?;
        copy_db(&lib.db(), &root.join("library.sqlite"))?;
        copy_db(&lib.source(), &root.join("source.sqlite"))?;
        fs::copy(lib.path().join("library.json"), root.join("library.json"))?;
        backup = Some(root.join("library.sqlite"));
        let stage = root.join("upgrade.pilib");
        fs::create_dir(&stage)?;
        let built = (|| -> Result<()> {
            copy_db(&root.join("library.sqlite"), &stage.join("library.sqlite"))?;
            copy_db(&root.join("source.sqlite"), &stage.join("source.sqlite"))?;
            fs::copy(root.join("library.json"), stage.join("library.json"))?;
            let mapping = lib.path().join("album-char-map.json");
            if mapping.is_file() {
                fs::copy(mapping, stage.join("album-char-map.json"))?;
            }
            crate::app_migrate::source(&stage.join("source.sqlite"), lib.config())?;
            crate::app_migrate::application(&stage, lib)?;
            let completed = Library::open(&stage)?;
            if required(&completed)? {
                return Err(Error::Schema("升级后 source 或应用初始化仍未完成".into()));
            }
            tx.execute(
                "ATTACH DATABASE ? AS upgraded_app",
                [crate::sqlite::path(stage.join("library.sqlite"))
                    .to_str()
                    .expect("app_legacy_upgrade internal invariant")],
            )?;
            tx.execute(
                "ATTACH DATABASE ? AS upgraded_source",
                [crate::sqlite::path(stage.join("source.sqlite"))
                    .to_str()
                    .expect("app_legacy_upgrade internal invariant")],
            )?;
            publish(&tx, "main", "upgraded_app").map_err(|e| Error::Schema(format!("发布应用库失败：{e}")))?;
            publish(&tx, "original_source", "upgraded_source")
                .map_err(|e| Error::Schema(format!("发布 source 失败：{e}")))?;
            before_commit(&tx)?;
            Ok(())
        })();
        if let Err(e) = built {
            drop(tx);
            drop(c);
            fs::remove_dir_all(&stage)?;
            return Err(e);
        }
        tx.commit()?;
        committed = true;
        drop(c);
        fs::remove_dir_all(stage)?;
        Ok(crate::app_upgrade::UpgradeReport {
            library: Library::open(lib.path())?,
            before,
            after: crate::SCHEMA_VERSION,
            backup: backup.clone().expect("app_legacy_upgrade internal invariant"),
            maintenance_pending: false,
        })
    })();
    result.map_err(|error| crate::app_upgrade::UpgradeFailure { error, backup, committed })
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture(std::path::PathBuf);
    impl Fixture {
        fn new() -> Self {
            Self::at(
                std::env::temp_dir().join(format!("illustash-publication-{}.pilib", crate::store::new_key().unwrap())),
            )
        }
        fn at(p: std::path::PathBuf) -> Self {
            fs::create_dir_all(&p).unwrap();
            fs::write(p.join("library.json"), r#"{"format":1,"id":"synthetic","name":"合成","sources":[]}"#).unwrap();
            let c = crate::sqlite::open(p.join("library.sqlite")).unwrap();
            c.execute_batch(include_str!("app_create_schema.sql")).unwrap();
            c.execute_batch("UPDATE meta SET value='35' WHERE key='schema_version'; INSERT INTO meta VALUES ('indexed','synthetic'),('general_indexed','synthetic'),('rating_indexed','synthetic'),('names_version','0'); CREATE TABLE retained_rows(value BLOB); INSERT INTO retained_rows(rowid,value) VALUES (27,zeroblob(100000));").unwrap();
            let s = crate::sqlite::open(p.join("source.sqlite")).unwrap();
            s.execute_batch("CREATE TABLE assets(uuid TEXT,filename TEXT,src_path TEXT,width INTEGER,height INTEGER,added_at TEXT,src_kind TEXT,in_scope INTEGER,trashed INTEGER); CREATE TABLE labels(uuid TEXT,dim TEXT,value TEXT,conf REAL); CREATE TABLE decisions(uuid TEXT,dim TEXT,value TEXT,note TEXT); CREATE TABLE characters(char_id TEXT); CREATE TABLE character_aliases(char_id TEXT,alias TEXT); CREATE TABLE runs(id INTEGER);").unwrap();
            Self(p)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn failed_publication_rolls_back_both_files_and_holds_both_writer_locks() {
        let f = Fixture::new();
        let lib = Library::open_for_upgrade(&f.0).unwrap();
        let failure = upgrade_with(&lib, |tx| {
            assert_eq!(meta(tx, "schema_version")?.as_deref(), Some("36"));
            tx.prepare("SELECT source_id,preset FROM original_source.assets")?;
            for path in [lib.db(), lib.source()] {
                let peer = crate::sqlite::open(path)?;
                peer.busy_timeout(std::time::Duration::ZERO)?;
                assert!(peer.execute("DELETE FROM assets", []).is_err());
            }
            Err(Error::Invalid("synthetic publication failure".into()))
        })
        .unwrap_err();
        assert!(!failure.committed);
        let c = crate::app_library::read_connection(&lib.db()).unwrap();
        assert_eq!(meta(&c, "schema_version").unwrap().as_deref(), Some("35"));
        assert_eq!(
            c.query_row("SELECT rowid,length(value) FROM retained_rows", [], |r| Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?
            )))
            .unwrap(),
            (27, 100000)
        );
        let s = crate::app_library::read_connection(&lib.source()).unwrap();
        assert!(s.prepare("SELECT source_id FROM assets").is_err());
        assert!(failure.backup.unwrap().is_file());
        assert_eq!(upgrade(&lib).unwrap().after, 7);
    }
    fn snapshot(path: &Path) -> Vec<Vec<rusqlite::types::Value>> {
        let c = crate::sqlite::open(path).unwrap();
        let mut result = c
            .prepare("SELECT type,name,tbl_name,sql FROM sqlite_master ORDER BY type,name")
            .unwrap()
            .query_map([], |r| (0..4).map(|i| r.get::<_, rusqlite::types::Value>(i)).collect())
            .unwrap()
            .collect::<rusqlite::Result<Vec<Vec<rusqlite::types::Value>>>>()
            .unwrap();
        let tables = c
            .prepare("SELECT name,sql FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        for (name, sql) in tables {
            let table = quote(&name);
            let mut q = if sql.to_uppercase().contains("WITHOUT ROWID") {
                c.prepare(&format!("SELECT * FROM {table}")).unwrap()
            } else {
                c.prepare(&format!("SELECT rowid,* FROM {table} ORDER BY rowid")).unwrap()
            };
            let n = q.column_count();
            result.extend(
                q.query_map([], |r| (0..n).map(|i| r.get(i)).collect())
                    .unwrap()
                    .collect::<rusqlite::Result<Vec<Vec<rusqlite::types::Value>>>>()
                    .unwrap(),
            );
        }
        result
    }
    #[test]
    fn v2_identity_upgrade_keeps_the_pair_backup_and_transactional_retirement() {
        let f = Fixture::new();
        let mut c = crate::open(&f.0.join("library.sqlite")).unwrap();
        crate::schema::migrate_to(&mut c, 5).unwrap();
        c.execute("INSERT OR REPLACE INTO meta(key,value) VALUES ('identity_policy','v2')", []).unwrap();
        let old = snapshot(&f.0.join("library.sqlite"));
        drop(c);
        let lib = Library::open_for_upgrade(&f.0).unwrap();
        let failure =
            upgrade_with(&lib, |_| Err(Error::Invalid("synthetic v2 publication failure".into()))).unwrap_err();
        assert!(!failure.committed);
        assert_eq!(snapshot(&f.0.join("library.sqlite")), old);
        let report = upgrade(&lib).unwrap();
        let current = crate::open(&f.0.join("library.sqlite")).unwrap();
        assert_eq!(meta(&current, "identity_policy").unwrap().as_deref(), Some("hybrid"));
        assert_eq!(
            current
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE name IN ('identity_claims','identity_calibration')",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        assert_eq!(
            current
                .query_row("SELECT rowid,length(value) FROM retained_rows", [], |r| Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?
                )))
                .unwrap(),
            (27, 100000)
        );
        let backup = crate::app_library::read_connection(&report.backup).unwrap();
        assert_eq!(meta(&backup, "identity_policy").unwrap().as_deref(), Some("v2"));
        assert_eq!(crate::schema::version(&backup).unwrap(), 5);
        assert!(!report.backup.parent().unwrap().join("upgrade.pilib").exists());
    }
    #[test]
    fn pre_character_core_versions_upgrade_without_inventing_later_guards() {
        for version in [1, 2] {
            let f = Fixture::new();
            let mut c = crate::open(&f.0.join("library.sqlite")).unwrap();
            crate::schema::migrate_to(&mut c, version).unwrap();
            drop(c);
            let lib = Library::open_for_upgrade(&f.0).unwrap();
            assert_eq!(upgrade(&lib).unwrap().after, 7);
        }
    }
    #[test]
    fn long_path_upgrade_and_preupgrade_pair_restore() {
        let root = std::env::temp_dir().join(format!("illustash-long-upgrade-{}", crate::store::new_key().unwrap()));
        let mut parent = root.clone();
        while parent.as_os_str().len() < 200 {
            parent = parent.join("synthetic-segment");
        }
        let f = Fixture::at(parent.join("synthetic.pilib"));
        let before_app = snapshot(&f.0.join("library.sqlite"));
        let before_source = snapshot(&f.0.join("source.sqlite"));
        let report = upgrade(&Library::open_for_upgrade(&f.0).unwrap()).unwrap();
        assert_eq!(report.after, 7);
        let saved = report.backup.parent().unwrap();
        // Restore the saved pair into a separate package (never overwrite an open library).
        let restored = parent.join("restored.pilib");
        fs::create_dir(&restored).unwrap();
        for name in ["library.sqlite", "source.sqlite", "library.json"] {
            fs::copy(saved.join(name), restored.join(name)).unwrap();
        }
        assert!(snapshot(&restored.join("library.sqlite")) == before_app, "restored application schema/rows differ");
        assert!(snapshot(&restored.join("source.sqlite")) == before_source, "restored source schema/rows differ");
        assert_eq!(upgrade(&Library::open_for_upgrade(&restored).unwrap()).unwrap().after, 7);
        println!("synthetic library path units: {}", f.0.as_os_str().len());
        drop(f);
        fs::remove_dir_all(root).unwrap();
    }
    // Invoked in an isolated test subprocess: process exit bypasses Rust rollback/destructors.
    #[test]
    fn crash_helper() {
        let Some(path) = std::env::var_os("ILLUSTASH_UPGRADE_CRASH_FIXTURE") else {
            return;
        };
        let lib = Library::open_for_upgrade(Path::new(&path)).unwrap();
        let _ = upgrade_with(&lib, |tx| {
            tx.execute_batch("PRAGMA main.cache_size=1; PRAGMA original_source.cache_size=1;
                CREATE TABLE main.crash_spill(payload BLOB); INSERT INTO main.crash_spill VALUES(zeroblob(1048576));
                CREATE TABLE original_source.crash_spill(payload BLOB); INSERT INTO original_source.crash_spill VALUES(zeroblob(1048576));")?;
            std::process::exit(91);
        });
        panic!("did not reach crash point");
    }
    #[test]
    fn interrupted_publication_recovers_the_original_pair() {
        let f = Fixture::new();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "app_legacy_upgrade::tests::crash_helper", "--nocapture"])
            .env("ILLUSTASH_UPGRADE_CRASH_FIXTURE", &f.0)
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(91));
        // SQLite's writable open performs hot-journal recovery; ordinary open stays read-only.
        let c = crate::sqlite::open(f.0.join("library.sqlite")).unwrap();
        assert_eq!(meta(&c, "schema_version").unwrap().as_deref(), Some("35"));
        let s = crate::sqlite::open(f.0.join("source.sqlite")).unwrap();
        assert!(s.prepare("SELECT source_id FROM assets").is_err());
        for db in [&c, &s] {
            assert_eq!(
                db.query_row("SELECT count(*) FROM sqlite_master WHERE name='crash_spill'", [], |r| r.get::<_, i64>(0))
                    .unwrap(),
                0
            );
            crate::app_library::check_database(db).unwrap();
        }
        drop((c, s));
        assert_eq!(upgrade(&Library::open_for_upgrade(&f.0).unwrap()).unwrap().after, 7);
    }
}
