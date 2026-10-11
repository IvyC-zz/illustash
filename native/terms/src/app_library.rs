//! Existing application packages. Opening is validation only; creation, upgrades and cutover are explicit workflows.
use crate::{http_json::Json, Error, Result};
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

pub const FORMAT: i64 = 1;
pub const APPLICATION_SCHEMA: &str = "36";
const CORE_MARKERS: [&str; 4] = ["terms_artists", "terms_labels", "terms_characters", "terms_names"];

/// Opening progress: the database being checked and the overall fraction of both integrity checks.
/// Called on the opening thread, often (every hundred SQLite steps); callers throttle their output.
pub type Progress = Arc<dyn Fn(&str, f64) + Send + Sync + std::panic::RefUnwindSafe>;

/// Paths are immutable after validation. Session identity must additionally include a fresh session generation,
/// since copies of a package may share its config ID. No default package or Python runtime is consulted.
///
/// Every file the package owns is reached through the canonical path fixed at opening: retargeting the alias
/// the user opened afterwards must not redirect reads or writes to another package. The alias is kept only to
/// show and remember what the user chose.
#[derive(Clone, Debug)]
pub struct Library {
    display_path: PathBuf,
    instance_path: PathBuf,
    config: Value,
    ordered_config: Json,
}

impl Library {
    pub fn open(path: &Path) -> Result<Self> {
        Self::open_validated(path, false, None)
    }

    /// Same validation; a multi-GiB package's integrity checks take tens of seconds, so report them.
    pub fn open_reporting(path: &Path, progress: Progress) -> Result<Self> {
        Self::open_validated(path, false, Some(progress))
    }

    /// Explicit upgrade preflight only. Ordinary opening never accepts an old core.
    pub(crate) fn open_for_upgrade(path: &Path) -> Result<Self> {
        Self::open_validated(path, true, None)
    }

    fn open_validated(path: &Path, upgrade: bool, progress: Option<Progress>) -> Result<Self> {
        if path.extension().and_then(|s| s.to_str()) != Some("pilib") {
            return Err(Error::Invalid("图库包的扩展名必须是 .pilib".into()));
        }
        // Display paths preserve the explicit alias, matching Python Path.absolute rather than resolve.
        let path = if path.is_absolute() { path.to_owned() } else { std::env::current_dir()?.join(path) };
        let instance_path = crate::app_paths::canonical(&path)?;
        if !path.is_dir() {
            return Err(Error::Invalid("图库包必须是已有目录".into()));
        }
        let text = fs::read_to_string(path.join("library.json"))?;
        let config: Value =
            serde_json::from_str(&text).map_err(|e| Error::Invalid(format!("无效 library.json：{e}")))?;
        if !config.is_object() || config.get("format").and_then(Value::as_i64) != Some(FORMAT) {
            return Err(Error::Schema("图库包格式需要显式升级，当前程序只打开 format 1".into()));
        }
        for key in ["id", "name"] {
            if !config.get(key).is_some_and(Value::is_string) {
                return Err(Error::Invalid(format!("library.json 缺少字符串 {key}")));
            }
        }
        if config.get("sources").is_some_and(|v| {
            !v.is_array() || v.as_array().expect("validated internal JSON array").iter().any(|s| !s.is_object())
        }) {
            return Err(Error::Invalid("library.json 的 sources 必须是对象数组".into()));
        }
        let ordered_config =
            serde_json::from_str(&text).map_err(|e| Error::Invalid(format!("无效 library.json：{e}")))?;
        let library = Self { display_path: path, instance_path, config, ordered_config };
        // Weight each check by its file size, so the overall fraction tracks the bytes actually read.
        let weights = match &progress {
            Some(_) => {
                let (db, source) = (database_bytes(&library.db())?, database_bytes(&library.source())?);
                let total = (db + source).max(1) as f64;
                [(0.0, db as f64 / total), (db as f64 / total, source as f64 / total)]
            }
            None => [(0.0, 0.0); 2],
        };
        let report = |index: usize, label: &'static str| {
            progress.clone().map(|p| {
                let (base, span) = weights[index];
                Arc::new(move |done: f64| p(label, base + span * done))
                    as Arc<dyn Fn(f64) + Send + Sync + std::panic::RefUnwindSafe>
            })
        };
        let mut db = read_connection(&library.db())?;
        // One read snapshot: a concurrent migration cannot supply a mixture of version/ownership flags.
        let tx = db.transaction()?;
        check_database_reporting(&tx, report(0, "library.sqlite"))?;
        validate_checked(&tx, upgrade)?;
        tx.commit()?;
        let source = read_connection(&library.source())?;
        check_database_reporting(&source, report(1, "source.sqlite"))?;
        // Source layouts have no independent version number. Require only the established base schema;
        // new source links/presets are not needed by the migrated reads and must never be added on open.
        source.prepare(
            "SELECT uuid,filename,src_path,width,height,added_at,src_kind,in_scope,trashed FROM assets LIMIT 0",
        )?;
        source.prepare("SELECT uuid,dim,value,conf FROM labels LIMIT 0")?;
        // provenance.sqlite is optional: the existing HTTP readers supply empty evidence when absent.
        Ok(library)
    }

    /// Validate the same package again through its canonical path, keeping the alias the user opened for display.
    pub(crate) fn reopen(&self) -> Result<Self> {
        self.keep_display(Self::open_validated(&self.instance_path, false, None)?)
    }

    /// `reopen` for the explicit upgrade workflow, which accepts an old core.
    pub(crate) fn reopen_for_upgrade(&self) -> Result<Self> {
        self.keep_display(Self::open_validated(&self.instance_path, true, None)?)
    }

    fn keep_display(&self, mut library: Self) -> Result<Self> {
        library.display_path = self.display_path.clone();
        Ok(library)
    }

    /// A settings write updates the captured session's config without reopening or upgrading databases.
    pub(crate) fn reload_config(&mut self) -> Result<()> {
        let text = fs::read_to_string(self.instance_path.join("library.json"))?;
        self.config = serde_json::from_str(&text).map_err(|e| Error::Invalid(e.to_string()))?;
        self.ordered_config = serde_json::from_str(&text).map_err(|e| Error::Invalid(e.to_string()))?;
        Ok(())
    }

    /// The path as the user opened it, alias included: for display and the recent-libraries registry only.
    pub fn display_path(&self) -> &Path {
        &self.display_path
    }
    /// Canonical package identity and the root of every package file; combine it with a fresh session
    /// generation for identity, never just config.id.
    pub fn instance_path(&self) -> &Path {
        &self.instance_path
    }
    pub fn config(&self) -> &Value {
        &self.config
    }
    pub fn name(&self) -> &str {
        self.config["name"].as_str().expect("validated name")
    }
    pub fn id(&self) -> &str {
        self.config["id"].as_str().expect("validated id")
    }
    pub fn source(&self) -> PathBuf {
        self.instance_path.join("source.sqlite")
    }
    pub fn db(&self) -> PathBuf {
        self.instance_path.join("library.sqlite")
    }
    pub fn features(&self) -> PathBuf {
        self.instance_path.join("features")
    }
    pub fn thumbnails(&self) -> PathBuf {
        self.instance_path.join("thumbnails")
    }
    pub fn backups(&self) -> PathBuf {
        self.instance_path.join("backups")
    }
    pub fn frozen(&self) -> bool {
        self.config.get("test").is_some_and(truthy) || self.config.get("frozen").is_some_and(truthy)
    }

    /// Same field order and nested config order as web.library.Library.info; sizes include live WAL bytes.
    pub(crate) fn info(&self) -> Result<Json> {
        Ok(Json::object([
            ("path", self.display_path.to_str().ok_or_else(|| Error::Invalid("图库路径不是 UTF-8".into()))?.into()),
            ("name", self.ordered_config.get("name").expect("app_library internal invariant").clone()),
            ("id", self.ordered_config.get("id").expect("app_library internal invariant").clone()),
            ("format", self.ordered_config.get("format").expect("app_library internal invariant").clone()),
            ("sources", self.ordered_config.get("sources").cloned().unwrap_or(Json::Array(Vec::new()))),
            ("test", self.ordered_config.get("test").cloned().unwrap_or_else(|| Value::Null.into())),
            ("source_bytes", Value::from(database_bytes(&self.source())?).into()),
            ("library_bytes", Value::from(database_bytes(&self.db())?).into()),
            ("origins", crate::origin::descriptions().into()),
        ]))
    }

    /// Public byte contract without exposing the HTTP-only ordered JSON representation.
    pub fn info_json(&self) -> Result<String> {
        Ok(self.info()?.dump())
    }
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(v) => *v,
        Value::Number(v) => v.as_f64() != Some(0.0),
        Value::String(v) => !v.is_empty(),
        Value::Array(v) => !v.is_empty(),
        Value::Object(v) => !v.is_empty(),
    }
}
pub(crate) fn read_connection(path: &Path) -> Result<Connection> {
    if !path.is_file() {
        return Err(Error::Invalid(format!("图库缺少已有数据库：{}", path.display())));
    }
    // No CREATE flag, no migrations or journal mode changes. URI interpretation is deliberately disabled
    // for package paths, so names containing '?' or '#' cannot silently refer to a different file.
    let conn = crate::sqlite::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    conn.busy_timeout(Duration::from_secs(10))?;
    Ok(conn)
}
pub(crate) fn check_database(conn: &Connection) -> Result<()> {
    check_database_reporting(conn, None)
}

/// quick_check reads every page, then (for tables) every row again to check constraints; SQLite 3.43+ calls
/// the progress handler during both passes. The pager's cache-miss count tracks the pages read so far. With
/// the default 2 MiB page cache nearly every read misses; on real libraries the total came to 1.4–1.6× the
/// page count (2026-10-02), so `READS` is the expected ratio. A low estimate stalls the bar near its end,
/// a high one ends in a short jump, which is the lesser problem.
fn check_database_reporting(
    conn: &Connection,
    progress: Option<Arc<dyn Fn(f64) + Send + Sync + std::panic::RefUnwindSafe>>,
) -> Result<()> {
    const READS: f64 = 1.5;
    if let Some(progress) = &progress {
        let pages: i64 = conn.query_row("PRAGMA page_count", [], |row| row.get(0))?;
        let expected = pages.max(1) as f64 * READS;
        let handle = Handle(unsafe { conn.handle() });
        let start = cache_misses(handle);
        let report = progress.clone();
        progress(0.0);
        conn.progress_handler(
            100,
            Some(move || {
                report(((cache_misses(handle) - start).max(0) as f64 / expected).min(0.99));
                false
            }),
        );
    }
    let result: rusqlite::Result<String> = conn.query_row("PRAGMA quick_check(1)", [], |row| row.get(0));
    if let Some(progress) = &progress {
        conn.progress_handler(0, None::<fn() -> bool>);
        if result.is_ok() {
            progress(1.0);
        }
    }
    let result = result?;
    if result != "ok" {
        return Err(Error::Schema(format!("图库数据库损坏：{result}")));
    }
    Ok(())
}

/// The connection's own handle, read only from its progress callback (same thread, inside its step).
#[derive(Clone, Copy)]
struct Handle(*mut rusqlite::ffi::sqlite3);
unsafe impl Send for Handle {}
impl std::panic::RefUnwindSafe for Handle {}

fn cache_misses(handle: Handle) -> i64 {
    let (mut current, mut high) = (0, 0);
    unsafe {
        rusqlite::ffi::sqlite3_db_status(
            handle.0,
            rusqlite::ffi::SQLITE_DBSTATUS_CACHE_MISS,
            &mut current,
            &mut high,
            0,
        );
    }
    current as i64
}
pub(crate) fn require_meta(conn: &Connection, key: &str, expected: &str) -> Result<()> {
    let mut query = conn.prepare("SELECT value FROM meta WHERE key=?1")?;
    let mut rows = query.query([key])?;
    let value = rows.next()?.map(|r| r.get::<_, String>(0)).transpose()?;
    if value.as_deref() != Some(expected) || rows.next()?.is_some() {
        return Err(Error::Schema(format!("图库需要显式初始化、升级或切换：{key} 应为 {expected}")));
    }
    Ok(())
}
/// Same read-only version/ownership checks for opening and the locked upgrade snapshot.
pub(crate) fn validate_application(conn: &Connection, upgrade: bool) -> Result<()> {
    check_database(conn)?;
    validate_checked(conn, upgrade)
}
fn validate_checked(conn: &Connection, upgrade: bool) -> Result<()> {
    if upgrade {
        return crate::app_legacy_upgrade::validate(conn);
    }
    require_meta(conn, "schema_version", APPLICATION_SCHEMA)?;
    crate::schema::require_current(conn)?;
    for marker in CORE_MARKERS {
        require_meta(conn, marker, "core")?;
    }
    Ok(())
}
fn database_bytes(path: &Path) -> Result<u64> {
    let mut wal = path.as_os_str().to_owned();
    wal.push("-wal");
    let mut total = 0u64;
    for file in [path, Path::new(&wal)] {
        match fs::metadata(file) {
            Ok(metadata) => {
                total = total.checked_add(metadata.len()).ok_or_else(|| Error::Invalid("数据库尺寸溢出".into()))?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Fixture {
        root: PathBuf,
        package: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "illustash-app-library-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let package = root.join("synthetic.pilib");
            fs::create_dir_all(&package).unwrap();
            fs::write(package.join("library.json"), r#"{"format":1,"id":"synthetic","name":"合成图库","sources":[{"kind":"folder","id":"s1","path":"/synthetic-only"}],"test":{"name":"合成","id":"persona"},"extra":"retain"}"#).unwrap();
            let db = crate::sqlite::open(package.join("library.sqlite")).unwrap();
            db.execute_batch("CREATE TABLE meta(key TEXT PRIMARY KEY,value TEXT); INSERT INTO meta VALUES ('schema_version','36'),('terms_schema','7'),('terms_artists','core'),('terms_labels','core'),('terms_characters','core'),('terms_names','core');").unwrap();
            let src = crate::sqlite::open(package.join("source.sqlite")).unwrap();
            src.execute_batch("CREATE TABLE assets(uuid TEXT,filename TEXT,src_path TEXT,width INTEGER,height INTEGER,added_at TEXT,src_kind TEXT,in_scope INTEGER,trashed INTEGER); CREATE TABLE labels(uuid TEXT,dim TEXT,value TEXT,conf REAL);").unwrap();
            Self { root, package }
        }
        fn bytes(&self) -> Vec<(String, Vec<u8>)> {
            let mut result = fs::read_dir(&self.package)
                .unwrap()
                .map(|entry| {
                    let entry = entry.unwrap();
                    (entry.file_name().to_str().unwrap().to_string(), fs::read(entry.path()).unwrap())
                })
                .collect::<Vec<_>>();
            result.sort_by(|a, b| a.0.cmp(&b.0));
            result
        }
        fn refuses_unchanged(&self) {
            let before = self.bytes();
            assert!(Library::open(&self.package).is_err());
            assert_eq!(before, self.bytes());
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn opens_existing_without_side_effects_and_keeps_info_order() {
        let f = Fixture::new();
        let before = f.bytes();
        let lib = Library::open(&f.package).unwrap();
        assert_eq!(lib.display_path(), f.package);
        assert_eq!(lib.instance_path(), crate::app_paths::canonical(&f.package).unwrap());
        assert_eq!(lib.id(), "synthetic");
        assert_eq!(lib.name(), "合成图库");
        assert!(lib.frozen());
        assert_eq!(lib.config()["extra"], "retain");
        assert!(!lib.features().exists());
        assert!(!lib.backups().exists());
        assert!(!lib.thumbnails().exists());
        let info = lib.info_json().unwrap();
        let source_bytes = fs::metadata(lib.source()).unwrap().len();
        let library_bytes = fs::metadata(lib.db()).unwrap().len();
        assert_eq!(
            info,
            format!(
                r#"{{"path": {}, "name": "合成图库", "id": "synthetic", "format": 1, "sources": [{{"kind": "folder", "id": "s1", "path": "/synthetic-only"}}], "test": {{"name": "合成", "id": "persona"}}, "source_bytes": {source_bytes}, "library_bytes": {library_bytes}, "origins": [{{"id": "pixiv", "label": "Pixiv"}}, {{"id": "x", "label": "X"}}, {{"id": "other", "label": "其他"}}]}}"#,
                serde_json::to_string(lib.display_path().to_str().unwrap()).unwrap()
            )
        );
        assert_eq!(before, f.bytes());
    }
    #[test]
    fn rejects_each_version_and_core_marker_without_upgrading() {
        for (key, value) in [
            ("schema_version", "35"),
            ("schema_version", "37"),
            ("terms_schema", "6"),
            ("terms_schema", "8"),
            ("terms_schema", "bad"),
            ("terms_artists", "legacy"),
            ("terms_labels", "legacy"),
            ("terms_characters", "legacy"),
            ("terms_names", "legacy"),
        ] {
            let f = Fixture::new();
            let db = crate::sqlite::open(f.package.join("library.sqlite")).unwrap();
            db.execute("UPDATE meta SET value=? WHERE key=?", [value, key]).unwrap();
            drop(db);
            f.refuses_unchanged();
        }
        for key in
            ["schema_version", "terms_schema", "terms_artists", "terms_labels", "terms_characters", "terms_names"]
        {
            let f = Fixture::new();
            let db = crate::sqlite::open(f.package.join("library.sqlite")).unwrap();
            db.execute("DELETE FROM meta WHERE key=?", [key]).unwrap();
            drop(db);
            f.refuses_unchanged();
        }
    }
    #[test]
    fn rejects_bad_config_and_format_without_creating_anything() {
        for text in [
            "broken",
            "[]",
            "{}",
            r#"{"format":2,"id":"x","name":"x"}"#,
            r#"{"format":1,"id":null,"name":"x"}"#,
            r#"{"format":1,"id":"x"}"#,
            r#"{"format":1,"id":"x","name":"x","sources":null}"#,
            r#"{"format":1,"id":"x","name":"x","sources":[3]}"#,
        ] {
            let f = Fixture::new();
            fs::write(f.package.join("library.json"), text).unwrap();
            f.refuses_unchanged();
        }
    }
    #[test]
    fn rejects_missing_or_corrupt_required_files_without_recreating() {
        for file in ["library.json", "source.sqlite", "library.sqlite"] {
            let f = Fixture::new();
            fs::remove_file(f.package.join(file)).unwrap();
            f.refuses_unchanged();
        }
        for file in ["source.sqlite", "library.sqlite"] {
            let f = Fixture::new();
            fs::write(f.package.join(file), b"not SQLite").unwrap();
            f.refuses_unchanged();
        }
        let f = Fixture::new();
        let absent = f.root.join("absent.pilib");
        assert!(Library::open(&absent).is_err());
        assert!(!absent.exists());
    }
    #[test]
    fn rejects_source_schema_needing_upgrade() {
        for sql in
            ["DROP TABLE labels", "ALTER TABLE labels DROP COLUMN conf", "ALTER TABLE assets DROP COLUMN in_scope"]
        {
            let f = Fixture::new();
            let source = crate::sqlite::open(f.package.join("source.sqlite")).unwrap();
            source.execute_batch(sql).unwrap();
            drop(source);
            f.refuses_unchanged();
        }
    }
    #[test]
    fn optional_config_fields_and_provenance_are_not_required() {
        let f = Fixture::new();
        fs::write(f.package.join("library.json"), r#"{"name":"x","id":"x","format":1}"#).unwrap();
        let lib = Library::open(&f.package).unwrap();
        assert!(!lib.frozen());
        assert!(!lib.instance_path().join("provenance.sqlite").exists());
        let info = lib.info_json().unwrap();
        assert!(info.contains("\"sources\": [], \"test\": null"));
    }
    #[test]
    fn reports_check_progress_monotonically_by_file_size() {
        let f = Fixture::new();
        // Synthetic bulk only: about 8 MiB in source.sqlite with an index, 0.1 MiB in library.sqlite.
        let src = crate::sqlite::open(f.package.join("source.sqlite")).unwrap();
        src.execute_batch(
            "CREATE TABLE bulk(id INTEGER PRIMARY KEY, body TEXT); CREATE INDEX bulk_body ON bulk(body);
             WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<40000)
             INSERT INTO bulk SELECT i, printf('%0100d', i) FROM n;",
        )
        .unwrap();
        drop(src);
        let seen = Arc::new(std::sync::Mutex::new(Vec::<(String, f64)>::new()));
        let sink = seen.clone();
        let progress: Progress = Arc::new(move |label, done| sink.lock().unwrap().push((label.into(), done)));
        let before = f.bytes();
        Library::open_reporting(&f.package, progress).unwrap();
        assert_eq!(f.bytes(), before);
        let seen = seen.lock().unwrap();
        assert!(seen.windows(2).all(|w| w[0].1 <= w[1].1), "{seen:?}");
        assert_eq!(seen.first().unwrap(), &("library.sqlite".to_string(), 0.0));
        assert_eq!(seen.last().unwrap(), &("source.sqlite".to_string(), 1.0));
        let source: Vec<f64> = seen.iter().filter(|(l, _)| l == "source.sqlite").map(|(_, d)| *d).collect();
        // Intermediate reports during the scan itself, reaching most of the way before it completes.
        assert!(source.len() > 10, "{seen:?}");
        assert!(source[source.len() - 2] > 0.8, "{source:?}");
    }
    #[test]
    fn wal_size_is_counted_without_opening_or_changing_it() {
        let f = Fixture::new();
        let lib = Library::open(&f.package).unwrap();
        let base = database_bytes(&lib.db()).unwrap();
        fs::write(f.package.join("library.sqlite-wal"), b"synthetic-size-only").unwrap();
        assert_eq!(database_bytes(&lib.db()).unwrap(), base + 19);
        assert!(lib.info_json().unwrap().contains(&format!("\"library_bytes\": {}", base + 19)));
    }
    #[cfg(unix)]
    #[test]
    fn canonical_identity_distinguishes_copies_not_aliases() {
        use std::os::unix::fs::symlink;
        let f = Fixture::new();
        let alias = f.root.join("alias.pilib");
        symlink(&f.package, &alias).unwrap();
        let a = Library::open(&f.package).unwrap();
        let b = Library::open(&alias).unwrap();
        assert_eq!(a.instance_path(), b.instance_path());
        assert_ne!(a.display_path(), b.display_path());
        assert_eq!(b.display_path(), alias);
        let alias_text = serde_json::to_string(alias.to_str().unwrap()).unwrap();
        assert!(b.info_json().unwrap().starts_with(&format!("{{\"path\": {alias_text},")));
        assert_eq!(b.source(), a.instance_path().join("source.sqlite"));
        assert_eq!(b.db(), a.instance_path().join("library.sqlite"));
        fs::remove_file(&alias).unwrap();
        assert!(b.db().is_file());
        let other = Fixture::new();
        let c = Library::open(&other.package).unwrap();
        assert_eq!(a.id(), c.id());
        assert_ne!(a.instance_path(), c.instance_path());
    }
    #[cfg(unix)]
    #[test]
    fn retargeted_alias_keeps_every_package_file_on_the_opened_package() {
        use std::os::unix::fs::symlink;
        // Two packages with the same config ID; the alias first names A, then B.
        let (a, b) = (Fixture::new(), Fixture::new());
        fs::write(b.package.join("library.json"), r#"{"format":1,"id":"synthetic","name":"另一个合成图库"}"#).unwrap();
        let alias = a.root.join("alias.pilib");
        symlink(&a.package, &alias).unwrap();
        let mut opened = Library::open(&alias).unwrap();
        fs::remove_file(&alias).unwrap();
        symlink(&b.package, &alias).unwrap();
        let home = crate::app_paths::canonical(&a.package).unwrap();
        let files = [opened.db(), opened.source(), opened.features(), opened.thumbnails(), opened.backups()];
        assert_eq!(files.map(|p| p.parent().unwrap().to_owned()), [(); 5].map(|_| home.clone()));
        opened.reload_config().unwrap();
        assert_eq!(opened.name(), "合成图库");
        let again = opened.reopen().unwrap();
        assert_eq!(
            (again.instance_path(), again.display_path(), again.name()),
            (home.as_path(), alias.as_path(), "合成图库")
        );
        // Opening the alias afresh is a deliberate choice of B.
        assert_eq!(Library::open(&alias).unwrap().name(), "另一个合成图库");
    }
}
