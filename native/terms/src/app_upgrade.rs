//! Explicit upgrades of format-1 packages. Initialized core-5/6 packages keep the terms-only
//! workflow; historical application/source schemas and initialization use staged pair publication.
use crate::{
    app_library::{self, Library},
    Error, Result,
};
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior};
use std::{
    fmt,
    path::{Path, PathBuf},
};

#[derive(Debug)]
pub(crate) struct ValidatedUpgrade {
    library: Library,
    before: i64,
    legacy: bool,
}
#[derive(Debug)]
pub(crate) struct UpgradeReport {
    pub library: Library,
    pub before: i64,
    pub after: i64,
    pub backup: PathBuf,
    pub maintenance_pending: bool,
}
#[derive(Debug)]
pub(crate) struct UpgradeFailure {
    pub error: Error,
    pub backup: Option<PathBuf>,
    pub committed: bool,
}
impl fmt::Display for UpgradeFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.committed {
            match &self.backup {
                Some(backup) => write!(
                    f,
                    "升级已提交，后续处理失败：{error}；升级前备份：{backup}",
                    error = self.error,
                    backup = backup.display()
                ),
                None => write!(f, "升级已提交，后续处理失败：{error}", error = self.error),
            }
        } else {
            match &self.backup {
                Some(backup) => write!(
                    f,
                    "升级未提交：{error}；升级前备份：{backup}",
                    error = self.error,
                    backup = backup.display()
                ),
                None => write!(f, "升级未提交：{error}", error = self.error),
            }
        }
    }
}
impl std::error::Error for UpgradeFailure {}
impl From<Error> for UpgradeFailure {
    fn from(error: Error) -> Self {
        Self { error, backup: None, committed: false }
    }
}

/// What the page and the desktop shell say before asking to upgrade (#193). Pages recognise it by this text.
pub(crate) const NEEDED: &str =
    "这个图库需要升级到新格式才能打开。升级后旧版本的 Illustash 打不开它；升级前会自动备份，升级没有完成时图库保持原样。";

/// Whether a package that failed to open is one an explicit upgrade can open, checked read-only.
pub(crate) fn needed(path: &Path) -> Option<ValidatedUpgrade> {
    prepare(path).ok()
}

/// Upgrade a prepared package, saying what happened on the terminal as `--upgrade` does.
pub(crate) fn run(prepared: &ValidatedUpgrade) -> std::result::Result<UpgradeReport, UpgradeFailure> {
    let report = upgrade(prepared)?;
    eprintln!(
        "图库升级已提交：terms {} → {}；升级前备份：{}{}",
        report.before,
        report.after,
        report.backup.display(),
        if report.maintenance_pending { "；空间维护待重试" } else { "" }
    );
    Ok(report)
}

impl ValidatedUpgrade {
    pub(crate) fn name(&self) -> &str {
        self.library.name()
    }
}

/// No SQLite writes, backups, session registration, or source-index refresh.
pub(crate) fn prepare(path: &Path) -> Result<ValidatedUpgrade> {
    let library = Library::open_for_upgrade(path)?;
    let mut c = app_library::read_connection(&library.db())?;
    let tx = c.transaction()?;
    let strict = preflight_application(&tx).and_then(|v| {
        preflight_source(&library)?;
        Ok(v)
    });
    let legacy = strict.is_err();
    let before = if let Ok(v) = strict {
        v
    } else {
        crate::app_legacy_upgrade::validate(&tx)?;
        crate::schema::version(&tx)?
    };
    if !legacy || Library::open(path).is_err() || crate::app_legacy_upgrade::required(&library)? {
        tx.commit()?;
        Ok(ValidatedUpgrade { library, before, legacy })
    } else {
        Err(Error::Schema("当前图库无需升级".into()))
    }
}

/// One application SQLite commit. Failure before commit rolls it back; restoration is explicit.
pub(crate) fn upgrade(prepared: &ValidatedUpgrade) -> std::result::Result<UpgradeReport, UpgradeFailure> {
    if prepared.legacy {
        crate::app_legacy_upgrade::upgrade(&prepared.library)
    } else {
        upgrade_with(prepared, |_| Ok(()), |_| Ok(()))
    }
}
fn upgrade_with<B, A>(
    prepared: &ValidatedUpgrade,
    before_commit: B,
    after_commit: A,
) -> std::result::Result<UpgradeReport, UpgradeFailure>
where
    B: FnOnce(&Connection) -> Result<()>,
    A: FnOnce(&Library) -> Result<()>,
{
    // Repeat package validation: a prepared handle is not permission to upgrade a changed package.
    let current = prepared.library.reopen_for_upgrade()?;
    if current.instance_path() != prepared.library.instance_path() || current.config() != prepared.library.config() {
        return Err(Error::Conflict("升级预检后图库包已改变，请重新预检".into()).into());
    }
    let mut c = crate::sqlite::open_with_flags(current.db(), OpenFlags::SQLITE_OPEN_READ_WRITE).map_err(Error::from)?;
    crate::configure(&c)?;
    let mut backup = None;
    let mut committed = false;
    let result = (|| -> Result<UpgradeReport> {
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let before = preflight_application(&tx)?;
        if before != prepared.before {
            return Err(Error::Conflict("升级预检后词条版本已改变，请重新预检".into()));
        }
        preflight_source(&current)?;
        // An independent reader sees the committed state while this transaction excludes other writers.
        // SQLite backup preserves implicit rowids; VACUUM INTO would rewrite them in the recovery copy.
        let snapshot = app_library::read_connection(&current.db())?;
        let prepared_backup =
            crate::schema_rules::prepare_upgrade_backup(&snapshot, before, &crate::platform_backup::capability())?;
        backup = prepared_backup.path().map(std::path::Path::to_owned);
        drop(snapshot);
        let backup_path = backup.clone().ok_or_else(|| Error::Invalid("升级前备份没有生成".into()))?;
        crate::schema_rules::migrate_prepared_in(&tx, crate::SCHEMA_VERSION, &prepared_backup)?;
        before_commit(&tx)?;
        tx.commit()?;
        committed = true;
        // Same post-commit maintenance as the existing core CLI. Busy/disk failures retain its retry marker.
        crate::schema::compact_pending(&c);
        let maintenance_pending = meta(&c, "terms_compact_pending")?.is_some();
        drop(c);
        after_commit(&current)?;
        let library = current.reopen()?;
        Ok(UpgradeReport { library, before, after: crate::SCHEMA_VERSION, backup: backup_path, maintenance_pending })
    })();
    result.map_err(|error| UpgradeFailure { error, backup, committed })
}
fn meta(c: &Connection, key: &str) -> Result<Option<String>> {
    Ok(c.query_row("SELECT value FROM meta WHERE key=?", [key], |r| r.get(0)).optional()?)
}
fn preflight_source(library: &Library) -> Result<()> {
    let mut c = app_library::read_connection(&library.source())?;
    let tx = c.transaction()?;
    app_library::check_database(&tx)?;
    for sql in [
        "SELECT uuid,filename,src_path,width,height,added_at,src_kind,in_scope,trashed,source_id,preset FROM assets LIMIT 0",
        "SELECT uuid,dim,value,conf FROM labels LIMIT 0",
        "SELECT uuid,dim,value,note FROM decisions LIMIT 0",
        "SELECT asset,system,key,source_id,current FROM asset_links LIMIT 0",
        "SELECT source_id,path,size,mtime_ns,md5,asset FROM files LIMIT 0",
        "SELECT * FROM characters LIMIT 0", "SELECT * FROM character_aliases LIMIT 0", "SELECT * FROM runs LIMIT 0",
    ] {
        tx.prepare(sql).map_err(|e| Error::Schema(format!("source schema 尚不兼容，需要另行显式升级：{e}")))?;
    }
    tx.commit()?;
    Ok(())
}
fn preflight_application(c: &Connection) -> Result<i64> {
    app_library::validate_application(c, true)?;
    let app = meta(c, "schema_version")?;
    if app.as_deref() != Some("36") || !matches!(crate::schema::version(c)?, 5 | 6) {
        return Err(Error::Schema("需要应用升级".into()));
    }
    for key in ["terms_artists", "terms_labels", "terms_characters", "terms_names"] {
        if meta(c, key)?.as_deref() != Some("core") {
            return Err(Error::Schema("需要模块切换".into()));
        }
    }
    if meta(c, "identity_policy")?.as_deref() == Some("v2") {
        return Err(Error::Schema("身份策略 v2 需要应用层转换，本批词条升级不支持".into()));
    }
    crate::character_projection::policy(c)?;
    // Opening cannot silently compensate for unbuilt indexes or the Store.initialize write path.
    for key in ["indexed", "general_indexed", "rating_indexed", "names_version"] {
        if meta(c, key)?.is_none() {
            return Err(Error::Schema(format!("应用初始化或索引未完成：缺少 {key}，本批升级不补跑索引")));
        }
    }
    // All established schema-36 application read objects and core-5 storage must already exist.
    // Preparing views expands their dependencies without mutating projections or creating temp tables.
    const OBJECTS: &str = "assets entities candidates general_hits assertions deferred imported asset_tags asset_rating actions image_sigs relations links names asked audit_sample display_names name_aliases name_cache name_settings work_review reviews prefs family_evidence single_assets lead_assets duplicates curation sync_runs sync_added similar_groups clusters cluster_info prototypes weak_claims proto_claims learn_state auto_rules excluded family_confirmed membership_rules memberships tag_memberships variant_memberships ambiguous_claims identity_relations identity_evidence identity_decisions identity_decision_evidence identity_baseline identity_canonical identity_scopes identity_suspended identity_uncertain identity_anchor_decisions identity_anchor_history identity_pairs identity_nodes backend_effects term_assets terms term_names term_links term_relations term_packages term_evidence term_projection term_batches term_verdicts term_evidence_sets term_evidence_set_assets term_evidence_set_rows term_active_evidence term_identity_baseline term_tree term_legacy_ids term_key_names term_key_aliases term_name_display term_name_choice term_effective_verdicts term_relation_states term_relation_latest term_current_evidence";
    for object in OBJECTS.split_whitespace() {
        c.prepare(&format!("SELECT * FROM {object} LIMIT 0"))
            .map_err(|e| Error::Schema(format!("应用/core 结构未完成：{object}：{e}")))?;
    }
    for sql in [
        "SELECT uuid,filename,src_path,width,height,added_at,src_kind FROM assets LIMIT 0",
        "SELECT id,name,work,external_key,variant,parent_id,tag,merged_into,kind,artist,label_only,mascot,mascot_origin,work_id,original FROM entities LIMIT 0",
        "SELECT uuid,entity,source,score FROM candidates LIMIT 0",
        "SELECT uuid,entity,sources,score FROM family_evidence LIMIT 0",
        "SELECT id,label,before_json,ts,undone FROM actions LIMIT 0",
        "SELECT entity_id,lang,value,source,role,preferred,position FROM names LIMIT 0",
        "SELECT entity,skin_only,name,name_source,zh,ja,en,tag FROM term_name_display LIMIT 0",
        "SELECT kind,key,name,name_source,zh,ja,en FROM term_name_choice LIMIT 0",
        "SELECT key,value FROM prefs LIMIT 0",
        "SELECT uuid,entity,status,origin FROM memberships LIMIT 0",
    ] { c.prepare(sql)?; }
    for table in crate::schema::PRIVATE
        .iter()
        .chain(crate::legacy_characters::GUARDED.iter())
        .chain(crate::legacy_names::GUARDED.iter())
    {
        for event in ["insert", "update", "delete"] {
            let trigger = format!("{table}_writer_{event}");
            let exists: bool = c.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='trigger' AND name=? AND tbl_name=?)",
                [trigger.as_str(), *table],
                |r| r.get(0),
            )?;
            if !exists {
                return Err(Error::Schema(format!("核心写保护未完成：缺少 {trigger}")));
            }
        }
    }
    crate::schema::version(c)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        sync::atomic::{AtomicU64, Ordering},
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Fixture(PathBuf);
    impl Fixture {
        // Genuine migrate_to old core DDL, not a schema-7 database with a downgraded version marker.
        // Full historical producer/data equivalence is separately covered by application integration tests.
        fn new(version: i64) -> Self {
            let p = std::env::temp_dir().join(format!(
                "illustash-upgrade-{}-{}.pilib",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&p).unwrap();
            fs::write(p.join("library.json"), r#"{"format":1,"id":"synthetic","name":"升级合成","sources":[]}"#)
                .unwrap();
            let mut c = crate::open(&p.join("library.sqlite")).unwrap();
            c.execute_batch(include_str!("app_create_schema.sql")).unwrap();
            crate::schema::migrate_to(&mut c, 5).unwrap();
            c.execute_batch("DROP TABLE asset_tags; DROP TABLE general_hits; DROP TABLE asset_rating;").unwrap();
            c.execute_batch(crate::legacy_labels::VIEWS).unwrap();
            c.execute_batch("INSERT INTO meta VALUES ('terms_artists','core'),('terms_labels','core'),('terms_characters','core'),('terms_names','core'),('indexed','synthetic'),('general_indexed','synthetic'),('rating_indexed','synthetic'),('names_version','0');").unwrap();
            for table in crate::legacy_characters::GUARDED.iter().chain(crate::legacy_names::GUARDED.iter()) {
                crate::schema::guard(&c, table).unwrap();
            }
            if version == 6 {
                crate::schema::migrate_to(&mut c, 6).unwrap();
                crate::schema::compact_pending(&c);
                // Partial fixture construction now uses the public backup-safe Core API.
                // Remove only this synthetic fixture's setup backup before measuring the real upgrade.
                fs::remove_dir_all(p.join("library-backups")).unwrap();
            }
            c.execute_batch("CREATE TABLE implicit_rows(value TEXT); INSERT INTO implicit_rows(rowid,value) VALUES(27,'retain rowid');").unwrap();
            drop(c);
            let s = crate::sqlite::open(p.join("source.sqlite")).unwrap();
            s.execute_batch("CREATE TABLE assets(uuid TEXT,filename TEXT,src_path TEXT,width INTEGER,height INTEGER,added_at TEXT,src_kind TEXT,in_scope INTEGER,trashed INTEGER,source_id TEXT,preset TEXT); CREATE TABLE labels(uuid TEXT,dim TEXT,value TEXT,conf REAL); CREATE TABLE decisions(uuid TEXT,dim TEXT,value TEXT,note TEXT); CREATE TABLE characters(char_id TEXT); CREATE TABLE character_aliases(char_id TEXT,alias TEXT); CREATE TABLE runs(id INTEGER);").unwrap();
            s.execute_batch(include_str!("app_source_schema.sql")).unwrap();
            Self(p)
        }
        fn connection(&self) -> Connection {
            crate::open(&self.0.join("library.sqlite")).unwrap()
        }
        fn backups(&self) -> usize {
            fs::read_dir(self.0.join("library-backups")).map(|it| it.count()).unwrap_or(0)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn old_layouts_upgrade_once_with_one_rowid_faithful_backup_and_unchanged_source() {
        for version in [5, 6] {
            let f = Fixture::new(version);
            assert!(Library::open(&f.0).is_err());
            let source = fs::read(f.0.join("source.sqlite")).unwrap();
            let db_before = fs::read(f.0.join("library.sqlite")).unwrap();
            let prepared = prepare(&f.0).unwrap();
            assert_eq!(db_before, fs::read(f.0.join("library.sqlite")).unwrap());
            assert_eq!(f.backups(), 0);
            let report = upgrade(&prepared).unwrap();
            assert_eq!((report.before, report.after, report.maintenance_pending), (version, 7, false));
            assert_eq!(report.library.display_path(), f.0);
            assert_eq!(f.backups(), 1);
            let backup = app_library::read_connection(&report.backup).unwrap();
            assert_eq!(crate::schema::version(&backup).unwrap(), version);
            assert_eq!(backup.query_row("SELECT rowid FROM implicit_rows", [], |r| r.get::<_, i64>(0)).unwrap(), 27);
            assert_eq!(source, fs::read(f.0.join("source.sqlite")).unwrap());
            assert!(prepare(&f.0).is_err());
            assert_eq!(f.backups(), 1);
        }
    }
    #[test]
    fn failure_before_commit_rolls_back_all_core_changes_and_retains_backup() {
        let f = Fixture::new(5);
        let p = prepare(&f.0).unwrap();
        let failure = upgrade_with(
            &p,
            |tx| {
                tx.execute("INSERT INTO prefs VALUES ('injected','must roll back')", [])?;
                Err(Error::Invalid("synthetic failure".into()))
            },
            |_| Ok(()),
        )
        .unwrap_err();
        assert!(!failure.committed);
        assert!(failure.backup.as_ref().unwrap().is_file());
        let c = f.connection();
        assert_eq!(crate::schema::version(&c).unwrap(), 5);
        assert!(!c
            .query_row("SELECT EXISTS(SELECT 1 FROM prefs WHERE key='injected')", [], |r| r.get::<_, bool>(0))
            .unwrap());
        let ddl: String =
            c.query_row("SELECT sql FROM sqlite_master WHERE name='candidates'", [], |r| r.get(0)).unwrap();
        assert!(!ddl.contains("WITHOUT ROWID"));
        assert!(failure.to_string().contains("升级未提交"));
    }
    #[test]
    fn post_commit_failure_never_claims_rollback() {
        let f = Fixture::new(6);
        let p = prepare(&f.0).unwrap();
        let failure =
            upgrade_with(&p, |_| Ok(()), |_| Err(Error::Invalid("synthetic startup failure".into()))).unwrap_err();
        assert!(failure.committed);
        assert!(failure.to_string().contains("升级已提交"));
        assert!(failure.backup.as_ref().unwrap().is_file());
        assert_eq!(crate::schema::version(&f.connection()).unwrap(), 7);
        assert!(Library::open(&f.0).is_ok());
    }
    #[test]
    fn invalid_policy_ownership_and_old_core_application_refuse_without_backup() {
        for fault in ["DROP TRIGGER candidates_writer_insert", "UPDATE meta SET value='35' WHERE key='schema_version'"]
        {
            let f = Fixture::new(5);
            f.connection().execute_batch(fault).unwrap();
            let before = fs::read(f.0.join("library.sqlite")).unwrap();
            assert!(prepare(&f.0).is_err(), "{fault}");
            assert_eq!(before, fs::read(f.0.join("library.sqlite")).unwrap());
            assert_eq!(f.backups(), 0);
        }
        let f = Fixture::new(5);
        crate::sqlite::open(f.0.join("source.sqlite")).unwrap().execute_batch("DROP TABLE files").unwrap();
        assert!(prepare(&f.0).is_ok()); // Explicit source upgrade now creates files.
        assert_eq!(f.backups(), 0);
        let f = Fixture::new(5);
        f.connection().execute("DELETE FROM meta WHERE key='indexed'", []).unwrap();
        assert!(prepare(&f.0).is_ok());
        assert_eq!(f.backups(), 0);
    }
    #[test]
    fn changed_package_after_prepare_refuses_before_any_backup() {
        let f = Fixture::new(5);
        let p = prepare(&f.0).unwrap();
        fs::write(f.0.join("library.json"), r#"{"format":1,"id":"different","name":"changed"}"#).unwrap();
        let failure = upgrade(&p).unwrap_err();
        assert!(!failure.committed);
        assert!(failure.backup.is_none());
        assert_eq!(f.backups(), 0);
        assert_eq!(crate::schema::version(&f.connection()).unwrap(), 5);
    }
}
