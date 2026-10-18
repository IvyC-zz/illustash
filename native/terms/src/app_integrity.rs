//! Integrity checks of a package's two databases (#212, user decision 2026-10-18).
//!
//! A full check reads every page of both files: close to a minute for a library of some 50,000 pictures. Opening
//! runs it only when the previous session did not end normally (a crash, a forced quit, a power cut) or a check
//! has found damage. Otherwise source.sqlite is checked in the background once the library is ready, and
//! library.sqlite once every seven days; settings can start both at once. Upgrades, imports and backups keep their
//! own checks. Damage stops the session writing (POST requests answer with what to do, tasks and durable recovery
//! stop), and the next opening checks again before anything else.
use crate::app_library::{inspect_file, Library, Report, Verdict, Watch};
use crate::http_json::Json;
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

/// The package's check record, written only by the session that owns the package.
pub(crate) const RECORD: &str = ".illustash-checks.json";
const LIBRARY_EVERY: i64 = 7 * 24 * 3600;
/// Lets the library page load before the background check competes with it for the disk.
pub(crate) const BACKGROUND_DELAY: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) enum Database {
    #[serde(rename = "library.sqlite")]
    Library,
    #[serde(rename = "source.sqlite")]
    Source,
}
impl Database {
    pub(crate) const BOTH: [Database; 2] = [Database::Library, Database::Source];
    pub(crate) fn file(self) -> &'static str {
        match self {
            Database::Library => "library.sqlite",
            Database::Source => "source.sqlite",
        }
    }
    fn path(self, library: &Library) -> PathBuf {
        match self {
            Database::Library => library.db(),
            Database::Source => library.source(),
        }
    }
}

/// What a check found in one database.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Damage {
    pub(crate) database: Database,
    /// SQLite's own description of the first problem, for the log and for support.
    pub(crate) detail: String,
    /// Unix seconds.
    pub(crate) found: i64,
}
impl Damage {
    pub(crate) fn new(database: Database, detail: String) -> Self {
        Self { database, detail, found: now() }
    }
    /// What the person can do about it. The curation records cannot be rebuilt, the recognition results can.
    pub(crate) fn guidance(&self) -> &'static str {
        match self.database {
            Database::Library => LIBRARY_DAMAGED,
            Database::Source => SOURCE_DAMAGED,
        }
    }
    pub(crate) fn error(&self) -> Error {
        Error::Conflict(self.guidance().into())
    }
}
pub(crate) const LIBRARY_DAMAGED: &str = "这个图库的整理记录数据库（library.sqlite）已损坏，为保护数据已停止写入。请退出 Illustash，把图库文件夹里的 library.sqlite 连同旁边的 library.sqlite-wal、library.sqlite-shm 一起移到别处保存，再从 backups 文件夹里选最近的一份备份复制回来、改名为 library.sqlite，然后重新打开。最近一次备份之后的整理会丢失。";
pub(crate) const SOURCE_DAMAGED: &str = "这个图库的识别结果数据库（source.sqlite）已损坏，为保护数据已停止写入；人工整理记录在 library.sqlite 里，没有受影响。目前还不能在 app 里重建 source.sqlite：请先在设置里立即备份一次整理记录，然后退出 Illustash，保持图库文件夹原样，不要删除或替换其中的文件。";

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Record {
    /// Set while a session has the package open. Still set when the next session opens it: that one did not end
    /// normally.
    #[serde(default)]
    open: bool,
    /// Unix seconds of the last full check that found each database sound.
    #[serde(default)]
    checked: BTreeMap<Database, i64>,
    #[serde(default)]
    damaged: Option<Damage>,
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// The record as left on disk, read-only. A record that cannot be read says nothing about the last session, so it
/// counts as one that did not end normally.
fn read(dir: &Path) -> Record {
    match fs::read(dir.join(RECORD)) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or(Record { open: true, ..Record::default() }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Record::default(),
        Err(_) => Record { open: true, ..Record::default() },
    }
}

/// The damage a check recorded in the package at `dir`, for the desktop shell after the service failed to open it.
pub(crate) fn recorded_damage(dir: &Path) -> Option<Damage> {
    read(dir).damaged
}

/// The owning session's view of the record (`app_service::Owner` holds it). Every change is written at once, through
/// a temporary file and a rename, so a crash leaves either the old record or the new one.
pub(crate) struct Health {
    dir: PathBuf,
    record: Mutex<Record>,
    /// The previous session did not end normally, and no check since has found both databases sound.
    inherited: AtomicBool,
    /// This session marked the package open, so its normal end marks it closed again.
    marked: AtomicBool,
    /// Unix seconds when this session opened the package: a database checked since then is not due again.
    opened: Mutex<i64>,
}
impl Health {
    pub(crate) fn load(dir: &Path) -> Self {
        let record = read(dir);
        Self {
            dir: dir.to_owned(),
            inherited: AtomicBool::new(record.open),
            record: Mutex::new(record),
            marked: AtomicBool::new(false),
            opened: Mutex::new(i64::MAX),
        }
    }
    /// Opening checks both databases first: after an abnormal end, or while damage is on record.
    pub(crate) fn needs_check(&self) -> bool {
        self.inherited.load(Ordering::SeqCst) || self.damage().is_some()
    }
    pub(crate) fn damage(&self) -> Option<Damage> {
        self.record.lock().expect("integrity record lock poisoned").damaged.clone()
    }
    pub(crate) fn checked(&self) -> BTreeMap<Database, i64> {
        self.record.lock().expect("integrity record lock poisoned").checked.clone()
    }
    /// The session opened the package, after checking `checked` (both or none) and finding them sound.
    pub(crate) fn opened(&self, checked: &[Database]) -> Result<()> {
        let time = now();
        self.update(|record| {
            record.open = true;
            for database in checked {
                record.checked.insert(*database, time);
            }
            if record.damaged.as_ref().is_some_and(|d| checked.contains(&d.database)) {
                record.damaged = None;
            }
        })?;
        if checked.len() == Database::BOTH.len() {
            self.inherited.store(false, Ordering::SeqCst);
        }
        self.marked.store(true, Ordering::SeqCst);
        *self.opened.lock().expect("integrity record lock poisoned") = time;
        Ok(())
    }
    /// A full check found `database` sound.
    pub(crate) fn passed(&self, database: Database) -> Result<()> {
        self.update(|record| {
            record.checked.insert(database, now());
        })
    }
    pub(crate) fn damaged(&self, damage: Damage) -> Result<()> {
        eprintln!("图库数据库损坏（{}）：{}", damage.database.file(), damage.detail);
        self.update(|record| record.damaged = Some(damage))
    }
    /// What the background checks after opening: source.sqlite unless opening just checked it, library.sqlite once
    /// a week.
    pub(crate) fn due(&self) -> Vec<Database> {
        let opened = *self.opened.lock().expect("integrity record lock poisoned");
        let checked = self.checked();
        let mut due = Vec::new();
        if checked.get(&Database::Library).is_none_or(|time| now() - time >= LIBRARY_EVERY) {
            due.push(Database::Library);
        }
        if checked.get(&Database::Source).is_none_or(|time| *time < opened) {
            due.push(Database::Source);
        }
        due
    }
    /// The session ended normally. Damage on record keeps the next opening checking first.
    pub(crate) fn closed(&self) {
        if !self.marked.swap(false, Ordering::SeqCst) {
            return;
        }
        if let Err(error) = self.update(|record| record.open = false) {
            eprintln!("没能记下图库已正常关闭，下次打开时会先检查数据：{error}");
        }
    }
    fn update(&self, change: impl FnOnce(&mut Record)) -> Result<()> {
        let mut record = self.record.lock().expect("integrity record lock poisoned");
        let mut next = record.clone();
        change(&mut next);
        let target = self.dir.join(RECORD);
        // Not store::new_key: tests replay a fixed sequence of keys, and this file must not consume one.
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let temp =
            self.dir.join(format!("{RECORD}.{}-{}.tmp", std::process::id(), NEXT.fetch_add(1, Ordering::SeqCst)));
        fs::write(&temp, serde_json::to_vec(&next).map_err(|e| Error::Invalid(e.to_string()))?)?;
        if let Err(e) = fs::rename(&temp, &target) {
            let _ = fs::remove_file(&temp);
            return Err(e.into());
        }
        *record = next;
        Ok(())
    }
}

/// Both full checks before opening, library.sqlite first, each weighted by its file size for `progress`.
/// The first damaged database ends the check.
pub(crate) fn check_both(library: &Library, progress: Option<crate::app_library::Progress>) -> Result<Option<Damage>> {
    for (label, verdict) in library.inspect(progress)? {
        if let Verdict::Damaged(detail) = verdict {
            let database = if label == Database::Library.file() { Database::Library } else { Database::Source };
            return Ok(Some(Damage::new(database, detail)));
        }
    }
    Ok(None)
}

#[derive(Clone, Default)]
struct Status {
    /// The database being checked and the fraction done.
    running: Option<(Database, f64)>,
    /// Why the last background check could not finish (not damage: a missing file, an I/O error).
    error: Option<String>,
}

/// The session's background check: at most one thread, stopped with the session.
#[derive(Default)]
pub(crate) struct Checker {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    status: Arc<Mutex<Status>>,
}
impl Checker {
    pub(crate) fn running(&self) -> bool {
        self.thread.as_ref().is_some_and(|thread| !thread.is_finished())
    }
    /// Check `databases` in order after `delay`; false when a check is already running or nothing is asked.
    pub(crate) fn start(
        &mut self,
        library: &Library,
        health: Arc<Health>,
        databases: Vec<Database>,
        delay: Duration,
    ) -> bool {
        if self.running() || databases.is_empty() || health.damage().is_some() {
            return false;
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let stop = Arc::new(AtomicBool::new(false));
        self.stop = stop.clone();
        let status = self.status.clone();
        *status.lock().expect("integrity status lock poisoned") =
            Status { running: Some((databases[0], 0.0)), error: None };
        let paths: Vec<(Database, PathBuf)> = databases.iter().map(|d| (*d, d.path(library))).collect();
        self.thread = Some(std::thread::spawn(move || {
            let until = Instant::now() + delay;
            while Instant::now() < until {
                if stop.load(Ordering::SeqCst) {
                    status.lock().expect("integrity status lock poisoned").running = None;
                    return;
                }
                std::thread::sleep(Duration::from_millis(50).min(delay));
            }
            for (database, path) in paths {
                let shown = status.clone();
                let report: Report = Arc::new(move |done| {
                    shown.lock().expect("integrity status lock poisoned").running = Some((database, done));
                });
                let mut journal = path.clone().into_os_string();
                journal.push("-journal");
                let watch = Watch { stop: Some(stop.clone()), writer: Some(PathBuf::from(journal)) };
                // A writer that starts meanwhile gets the file: the check stops, waits for quiet and starts over.
                let result = loop {
                    if !quiet(watch.writer.as_deref().expect("watched journal"), &stop) {
                        break Ok(Verdict::Yielded);
                    }
                    report(0.0);
                    match inspect_file(&path, Some(report.clone()), watch.clone()) {
                        Ok(Verdict::Yielded) if !stop.load(Ordering::SeqCst) => continue,
                        result => break result,
                    }
                };
                if stop.load(Ordering::SeqCst) {
                    break;
                }
                let recorded = match result {
                    Ok(Verdict::Sound) => health.passed(database),
                    Ok(Verdict::Damaged(detail)) => {
                        let recorded = health.damaged(Damage::new(database, detail));
                        if let Err(error) = &recorded {
                            eprintln!("没能记下图库数据库损坏：{error}");
                        }
                        break;
                    }
                    Ok(Verdict::Yielded) => Ok(()),
                    Err(error) => Err(error),
                };
                if let Err(error) = recorded {
                    eprintln!("图库数据检查没有完成（{}）：{error}", database.file());
                    status.lock().expect("integrity status lock poisoned").error = Some(error.to_string());
                }
            }
            status.lock().expect("integrity status lock poisoned").running = None;
        }));
        true
    }
    pub(crate) fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
    /// `GET /api/integrity`: the check in progress, when each database was last found sound (Unix seconds, null if
    /// never), the damage on record with what to do, and why the last background check did not finish.
    pub(crate) fn info(&self, health: &Health) -> Json {
        let status = self.status.lock().expect("integrity status lock poisoned").clone();
        let running = match status.running.filter(|_| self.running()) {
            Some((database, done)) => {
                Json::object([("database", database.file().into()), ("done", Json::Float(done.clamp(0.0, 1.0)))])
            }
            None => serde_json::Value::Null.into(),
        };
        let checked = health.checked();
        let damaged = match health.damage() {
            Some(damage) => {
                Json::object([("database", damage.database.file().into()), ("message", damage.guidance().into())])
            }
            None => serde_json::Value::Null.into(),
        };
        Json::object([
            ("running", running),
            (
                "checked",
                Json::object(Database::BOTH.map(|d| {
                    (
                        d.file(),
                        checked.get(&d).map_or(serde_json::Value::Null, |time| serde_json::Value::from(*time)).into(),
                    )
                })),
            ),
            ("damaged", damaged),
            ("error", status.error.map_or(serde_json::Value::Null, serde_json::Value::from).into()),
        ])
    }
}
/// Returns at once when no writer's journal is there, else once none has been seen for two seconds; false when the
/// check is stopped meanwhile.
fn quiet(journal: &Path, stop: &AtomicBool) -> bool {
    const QUIET: Duration = Duration::from_secs(2);
    let mut since = None;
    let mut seen = false;
    loop {
        if stop.load(Ordering::SeqCst) {
            return false;
        }
        if journal.exists() {
            (seen, since) = (true, None);
        } else if !seen || since.get_or_insert_with(Instant::now).elapsed() >= QUIET {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

impl Drop for Checker {
    fn drop(&mut self) {
        self.stop();
    }
}
