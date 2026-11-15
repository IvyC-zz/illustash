//! 图库包两个数据库的完整性检查（#212，用户 2026-10-18 决定）。
//!
//! 完整检查读两个文件的每一页：约 5 万张图的图库接近一分钟。只有上次会话没有正常结束（崩溃、强制退出、断电）
//! 或检查发现过损坏时，打开图库才运行它。其他情况下，图库就绪后在后台检查 source.sqlite，
//! library.sqlite 每七天检查一次；设置里可以同时启动两项。升级、导入和备份保留各自的检查。
//! 发现损坏时会话停止写入（POST 请求回答该怎么办，任务和持久恢复停止），下次打开时先再检查一遍。
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

/// 图库包的检查记录，只由持有该图库包的会话写入。
pub(crate) const RECORD: &str = ".illustash-checks.json";
const LIBRARY_EVERY: i64 = 7 * 24 * 3600;
/// 让图库页面先加载完，后台检查再来和它抢磁盘。
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

/// 一次检查在一个数据库里发现的问题。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Damage {
    pub(crate) database: Database,
    /// SQLite 自己对第一个问题的描述，供日志和支持使用。
    pub(crate) detail: String,
    /// Unix seconds.
    pub(crate) found: i64,
}
impl Damage {
    pub(crate) fn new(database: Database, detail: String) -> Self {
        Self { database, detail, found: now() }
    }
    /// 用户能怎么处理。整理记录无法重建，识别结果可以。
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
/// library.sqlite 只能换回备份（#492）。图库文件夹里有两处备份：`backups/` 是设置里「立即备份」留下的
/// `library-日期-时间-….sqlite`（同一文件夹里抢救 source.sqlite 时留存的 `source-damaged-…` 不是整理记录）；
/// `library-backups/` 是升级图库格式前自动留下的 `terms-v旧版-before-v新版-….sqlite`，换回后要再升级一次。
/// 换回的图库打开后也会照常生成人工结论快照（内容是换回后的状态），所以补回时要选换回之前的那份（#489、#503）。
pub(crate) const LIBRARY_DAMAGED: &str = "这个图库的整理记录数据库（library.sqlite）已损坏，为保护数据已停止写入。请退出 Illustash，把图库文件夹里的 library.sqlite 连同旁边的 library.sqlite-wal、library.sqlite-shm 一起移到别处保存，再从图库文件夹的备份里选一份复制回来、改名为 library.sqlite，然后重新打开。备份有两处：backups 文件夹里是在设置里「立即备份」留下的，只用名字以 library- 开头的文件（以 source-damaged 开头的是抢救识别结果时留存的损坏文件，不能用）；library-backups 文件夹里是升级图库格式前自动留下的，内容停在那次升级之前，换回后打开时会请你再升级一次。两处都有时，选修改时间最近的一份；如果 Illustash 提示这份备份需要先用 pre-v8 版本升级，就换一份。所选备份之后的整理会丢失。打开后，可以在「设置 › 数据」的「人工结论快照」里，对换回之前最近的一份自动快照点「恢复…」，补回备份之后的人工结论：只补这个图库还没决定的项，和已有结论冲突的保留已有的。";
/// source.sqlite 可以在应用内抢救（#228）：启动页、图库页的提示和图库切换器配合「抢救重建」按钮显示的文字。
pub(crate) const SOURCE_DAMAGED: &str = "这个图库的识别结果数据库（source.sqlite）已损坏，为保护数据已停止写入；人工整理记录在 library.sqlite 里，没有受影响。可以点「抢救重建」：还读得出的识别结果和每张图的 ID 都会保留，读不出的部分重新扫描来源文件夹、重新识别；损坏的文件移到图库的 backups 文件夹里留存。抢救前请确认来源文件夹都能访问。";
pub(crate) const NOTHING_TO_RESCUE: &str = "这个图库的识别结果数据库没有发现损坏，不需要抢救";

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Record {
    /// 会话打开图库包期间设置。下一个会话打开时仍然是设置状态，说明上一个会话没有正常结束。
    #[serde(default)]
    open: bool,
    /// 上次完整检查认定各数据库完好的 Unix 秒。
    #[serde(default)]
    checked: BTreeMap<Database, i64>,
    #[serde(default)]
    damaged: Option<Damage>,
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// 磁盘上留下的记录，只读。读不出的记录说明不了上次会话的情况，所以算作没有正常结束。
fn read(dir: &Path) -> Record {
    match fs::read(dir.join(RECORD)) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or(Record { open: true, ..Record::default() }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Record::default(),
        Err(_) => Record { open: true, ..Record::default() },
    }
}

/// 检查记在 `dir` 处图库包里的损坏，供服务打开失败后的桌面外壳使用。
pub(crate) fn recorded_damage(dir: &Path) -> Option<Damage> {
    read(dir).damaged
}

/// 持有图库包的会话所见的记录（由 `app_service::Owner` 持有）。每次改动都立即写入，经临时文件再改名，
/// 所以崩溃后留下的要么是旧记录，要么是新记录。
pub(crate) struct Health {
    dir: PathBuf,
    record: Mutex<Record>,
    /// 上次会话没有正常结束，并且之后还没有检查认定两个数据库都完好。
    inherited: AtomicBool,
    /// 这个会话把图库包标为打开，所以它正常结束时会再标为关闭。
    marked: AtomicBool,
    /// 这个会话打开图库包的 Unix 秒：在那之后检查过的数据库不用再检查。
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
    /// 打开时先检查两个数据库：在异常结束之后，或记录里有损坏时。
    pub(crate) fn needs_check(&self) -> bool {
        self.inherited.load(Ordering::SeqCst) || self.damage().is_some()
    }
    pub(crate) fn damage(&self) -> Option<Damage> {
        self.record.lock().expect("integrity record lock poisoned").damaged.clone()
    }
    pub(crate) fn checked(&self) -> BTreeMap<Database, i64> {
        self.record.lock().expect("integrity record lock poisoned").checked.clone()
    }
    /// 会话打开了图库包，此前检查了 `checked`（两个都查或都不查）并认定它们完好。
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
    /// 一次完整检查认定 `database` 完好。
    pub(crate) fn passed(&self, database: Database) -> Result<()> {
        self.update(|record| {
            record.checked.insert(database, now());
        })
    }
    /// source.sqlite 已换成抢救出的副本（#228），抢救时已完整检查过它。
    pub(crate) fn rescued(&self) -> Result<()> {
        self.update(|record| {
            record.checked.insert(Database::Source, now());
            if record.damaged.as_ref().is_some_and(|d| d.database == Database::Source) {
                record.damaged = None;
            }
        })
    }
    /// 记录损坏之后数据库被换掉了（#492，例如换回了升级前的旧格式备份），完整检查认定两个都完好：清除损坏，
    /// 这样外壳不再显示损坏提示，而是按图库本身的状态（例如需要升级）处理。
    pub(crate) fn sound_again(&self) -> Result<()> {
        let time = now();
        self.update(|record| {
            for database in Database::BOTH {
                record.checked.insert(database, time);
            }
            record.damaged = None;
        })
    }
    pub(crate) fn damaged(&self, damage: Damage) -> Result<()> {
        eprintln!("图库数据库损坏（{}）：{}", damage.database.file(), damage.detail);
        self.update(|record| record.damaged = Some(damage))
    }
    /// 打开后后台要检查什么：source.sqlite，除非打开时刚检查过；library.sqlite 每周一次。
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
    /// 会话正常结束。记录里有损坏时，下次打开仍会先检查。
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
        // 不用 store::new_key：测试会重放一串固定的键，这个文件不能用掉其中一个。
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

/// 打开前的两项完整检查，先查 library.sqlite，各按文件大小在 `progress` 里占权重。
/// 遇到第一个损坏的数据库就结束检查。
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
    /// 正在检查的数据库和完成的比例。
    running: Option<(Database, f64)>,
    /// 上次后台检查没能完成的原因（不是损坏：文件不见了、I/O 错误）。
    error: Option<String>,
}

/// 会话的后台检查：最多一个线程，随会话停止。
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
    /// 在 `delay` 之后按顺序检查 `databases`；已有检查在运行或没有要查的时返回 false。
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
                // 期间开始写入的写入方优先使用文件：检查停下，等安静后从头开始。
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
    /// `GET /api/integrity`：正在进行的检查，各数据库上次被认定完好的时间（Unix 秒，从没有则为 null），
    /// 记录里的损坏和处理办法，以及上次后台检查没有完成的原因。
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
/// 没有写入方的日志时立即返回，否则等到连续两秒都没看到日志再返回；检查期间被停止时返回 false。
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
