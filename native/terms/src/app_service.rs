//! In-process application lifecycle. Transport framing and security stay in app_http.
use crate::http_json::Json;
use crate::{app_library::Library, Error, Result};
use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        Arc, Mutex, RwLock,
    },
};
/// The host that opened a physical package owns it until it switches away, shuts down or exits: an OS
/// file lock, which the system drops with the process, so a crash never leaves the package locked. A
/// second host (or the same package through another alias) is refused instead of treating the active
/// owner's running work as left over from a crash.
pub(crate) struct Owner {
    _lock: std::fs::File,
}
impl Owner {
    pub(crate) fn claim(library: &Library) -> Result<Arc<Self>> {
        crate::app_file_lock::ensure_supported()?;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(library.instance_path().join(".illustash-owner.lock"))?;
        match crate::app_file_lock::acquire_unobstructed(&file) {
            Ok(()) => Ok(Arc::new(Self { _lock: file })),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock || e.raw_os_error() == Some(33) => {
                Err(Error::Conflict("这个图库已在另一个 Illustash 中打开；请先关闭那边，或回到那边继续使用".into()))
            }
            Err(e) => Err(e.into()),
        }
    }
}
pub(crate) struct Session {
    pub(crate) library: Library,
    /// Released when the session closes (switch or shutdown), not only when the service is dropped.
    pub(crate) owner: Option<Arc<Owner>>,
    pub(crate) token: String,
    pub(crate) generation: u64,
    pub(crate) syncer: crate::app_sync::Syncer,
    pub(crate) names: crate::app_names::Updater,
    pub(crate) provenance: crate::app_provenance::Runner,
    pub(crate) index: crate::app_index::Updater,
    pub(crate) auto_sync: bool,
    pub(crate) research: crate::app_research::Research,
    pub(crate) imports: Imports,
}
/// Recognition-result imports run outside the session lock (#26). They still take turns, as they did under the
/// lock, and closing the session (switching libraries or quitting) cancels the running and the waiting ones.
#[derive(Default)]
pub(crate) struct Imports {
    active: Arc<AtomicUsize>,
    turn: Arc<Mutex<()>>,
    cancel: Arc<AtomicBool>,
}
impl Imports {
    /// Counted as active from here, so index and sync starts are refused while it waits or runs.
    pub(crate) fn admit(&self) -> ImportTicket {
        self.active.fetch_add(1, Ordering::SeqCst);
        ImportTicket { active: self.active.clone(), turn: self.turn.clone(), cancel: self.cancel.clone() }
    }
    pub(crate) fn running(&self) -> bool {
        self.active.load(Ordering::SeqCst) > 0
    }
}
pub(crate) struct ImportTicket {
    active: Arc<AtomicUsize>,
    turn: Arc<Mutex<()>>,
    pub(crate) cancel: Arc<AtomicBool>,
}
impl ImportTicket {
    /// Waits for the previous import of this session, then runs with the session's cancel flag.
    pub(crate) fn run<T>(&self, import: impl FnOnce(&AtomicBool) -> T) -> T {
        let _turn = self.turn.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        import(&self.cancel)
    }
}
impl Drop for ImportTicket {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::SeqCst);
    }
}
impl Session {
    pub(crate) fn token() -> Result<String> {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).map_err(|e| Error::Invalid(e.to_string()))?;
        Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
    }
    #[allow(clippy::too_many_arguments)] // Session options mirror independent startup task flags.
    fn new(
        library: Library,
        owner: Arc<Owner>,
        generation: u64,
        names_enabled: bool,
        names_network: bool,
        provenance_enabled: bool,
        provenance_network: bool,
        root: &Path,
        index_network: bool,
        index_automatic: bool,
    ) -> Result<Self> {
        let token = Self::token()?;
        let names = crate::app_names::Updater::new(&library, names_enabled, names_network, generation, &token)?;
        let provenance =
            crate::app_provenance::Runner::new(&library, generation, &token, provenance_enabled, provenance_network)?;
        let index = crate::app_index::Updater::new(&library, root, generation, &token, index_network, index_automatic)?;
        let similar = crate::app_settings::startup(&library)?.get("similar").is_some_and(truthy);
        let research = crate::app_research::Research::new(generation, &token, similar);
        Ok(Self {
            library,
            owner: Some(owner),
            generation,
            token,
            syncer: crate::app_sync::Syncer::default(),
            names,
            provenance,
            index,
            auto_sync: true,
            research,
            imports: Imports::default(),
        })
    }
}
pub struct Application {
    pub(crate) session: RwLock<Session>,
    pub(crate) generation: AtomicU64,
    closed: Arc<AtomicBool>,
    pub(crate) tags: PathBuf,
    pub(crate) root: PathBuf,
    pub(crate) reference: PathBuf,
    pub(crate) sync_disabled: bool,
    pub(crate) images: crate::app_files::Images,
    pub(crate) exports: crate::app_archives::Exports,
    pub(crate) names_enabled: bool,
    pub(crate) names_network: bool,
    pub(crate) provenance_enabled: bool,
    pub(crate) provenance_network: bool,
    pub(crate) index_network: bool,
    pub(crate) index_automatic: bool,
}
pub(crate) fn truthy(value: &Json) -> bool {
    match value {
        Json::Scalar(serde_json::Value::Null) => false,
        Json::Scalar(serde_json::Value::Bool(value)) => *value,
        Json::Scalar(serde_json::Value::String(value)) => !value.is_empty(),
        Json::Scalar(serde_json::Value::Number(value)) => value.as_f64() != Some(0.0),
        Json::Integer(value) => value.trim_start_matches('-').chars().any(|c| c != '0'),
        Json::Float(value) => *value != 0.0,
        Json::Array(values) => !values.is_empty(),
        Json::Object(values) => !values.is_empty(),
        _ => true,
    }
}
/// Explicit application configuration; no socket, desktop or client is required.
#[derive(Clone, Debug)]
pub struct Options {
    pub root: PathBuf,
    pub names_enabled: bool,
    pub names_network: bool,
    pub provenance_enabled: bool,
    pub provenance_network: bool,
    pub index_network: bool,
    pub index_automatic: bool,
    pub auto_sync: bool,
    pub sync_disabled: bool,
    pub per_run: usize,
}
impl Options {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            names_enabled: true,
            names_network: false,
            provenance_enabled: true,
            provenance_network: false,
            index_network: false,
            index_automatic: true,
            auto_sync: true,
            sync_disabled: false,
            per_run: 0,
        }
    }
}
impl Application {
    /// Opens and registers an explicit package. Does not bind a port or start background work.
    pub fn open(path: &Path, options: Options) -> Result<Self> {
        Self::from_library(Library::open(path)?, Session::token()?, options, |error| error)
    }
    pub(crate) fn from_library(
        library: Library,
        token: String,
        options: Options,
        registration_error: impl FnOnce(Error) -> Error,
    ) -> Result<Self> {
        // Owned before any recovery below reads or rewrites the package's task state.
        let owner = Owner::claim(&library)?;
        let Options {
            root,
            names_enabled,
            names_network,
            provenance_enabled,
            provenance_network,
            index_network,
            index_automatic,
            auto_sync,
            sync_disabled,
            per_run,
        } = options;
        let reference = crate::app_resources::reference_path(&root)?;
        let names = crate::app_names::Updater::new(&library, names_enabled, names_network, 1, &token)?;
        let provenance =
            crate::app_provenance::Runner::new(&library, 1, &token, provenance_enabled, provenance_network)?;
        let index = crate::app_index::Updater::new(&library, &root, 1, &token, index_network, index_automatic)?;
        let similar_enabled = crate::app_settings::startup(&library)?.get("similar").is_some_and(truthy);
        let mut research = crate::app_research::Research::new(1, &token, similar_enabled);
        research.anchors = reference.parent().expect("internal file path must have a parent").to_owned();
        let mut session = Session {
            library,
            owner: Some(owner),
            generation: 1,
            token,
            syncer: crate::app_sync::Syncer::default(),
            names,
            provenance,
            index,
            auto_sync,
            research,
            imports: Imports::default(),
        };
        session.syncer.resource_root = root.clone();
        session.syncer.per_run = per_run;
        session.syncer.configure_startup(&session.library, !sync_disabled)?;
        crate::app_libraries::remember(&session.library).map_err(registration_error)?;
        Ok(Self {
            session: RwLock::new(session),
            generation: AtomicU64::new(1),
            closed: Arc::default(),
            tags: root.join("tags/tags_v3.csv"),
            root,
            reference,
            sync_disabled,
            images: crate::app_files::Images::default(),
            exports: crate::app_archives::Exports::default(),
            names_enabled,
            names_network,
            provenance_enabled,
            provenance_network,
            index_network,
            index_automatic,
        })
    }
}

impl Application {
    /// Starts the established tasks and durable-event recovery; no transport is involved.
    pub fn start(&self) -> Result<()> {
        self.check_open()?;
        self.start_reporting(|| {})
    }
    pub(crate) fn start_reporting(&self, before_recovery: impl FnOnce()) -> Result<()> {
        {
            let mut session = self.session.write().expect("app_http session lock poisoned");
            self.check_open()?;
            let library = session.library.clone();
            if self.names_enabled {
                let _ = session.names.start(&library, None, false);
            }
        }
        {
            let session = self.session.read().expect("app_http session lock poisoned");
            self.check_open()?;
            let lib = session.library.clone();
            if crate::app_research::configured() {
                crate::app_research::dispatch(&lib, "imported")?;
            }
        }
        before_recovery();
        self.recover();
        {
            let mut session = self.session.write().expect("app_http session lock poisoned");
            self.check_open()?;
            if session.auto_sync {
                let lib = session.library.clone();
                let token = session.token.clone();
                let generation = session.generation;
                if !session.index.writing() {
                    session.syncer.automatic(&lib, generation, &token)?;
                }
            }
        }
        Ok(())
    }
    /// Replays durable work only while holding its original library instance.
    pub fn recover(&self) {
        if self.closed.load(Ordering::SeqCst) {
            return;
        }
        let mut session = self.session.write().expect("app_http session lock poisoned");
        if self.closed.load(Ordering::SeqCst) {
            return;
        }
        let library = session.library.clone();
        session.provenance.recover();
        let busy = session.syncer.running();
        session.index.recover(&library, busy);
        session.names.recover(&library);
        if let Err(error) = session.research.recover(&library, busy) {
            eprintln!("研究事件恢复失败：{error}");
        }
        if let Err(error) = crate::app_effects::drain(&session.library.db()) {
            eprintln!("native verdict事件恢复失败：{error}");
        }
    }
    /// Stops and joins the current session's tasks and exports.
    pub fn shutdown(&self) {
        let mut session = self.session.write().expect("application session lock poisoned");
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        session.close();
        drop(session);
        self.exports.shutdown();
    }
    /// Capture when accepting work, before body IO, scheduling or lock waits.
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }
    /// Execute against the accepted physical instance, rechecking after the lock wait.
    /// Package files resolve through the canonical path fixed at opening, so display aliases cannot retarget them.
    pub fn with_library<T>(&self, received: u64, operation: impl FnOnce(&Library) -> Result<T>) -> Result<T> {
        let session = self.session.read().expect("application session lock poisoned");
        self.check_open()?;
        if !self.accepts(&session, received) {
            return Err(Error::Conflict("会话已过期，请刷新页面".into()));
        }
        operation(&session.library)
    }
}
impl Session {
    fn close(&mut self) {
        self.imports.cancel.store(true, Ordering::SeqCst);
        self.provenance.close();
        self.index.stop();
        self.research.stop();
        self.names.stop();
        self.syncer.stop();
        self.owner = None;
    }
}

impl Application {
    /// Switch an explicitly named package, rejecting work accepted by any previous instance.
    pub fn switch(&self, received: u64, path: &Path) -> Result<PathBuf> {
        let mut session = self.session.write().expect("application session lock poisoned");
        self.switch_locked(&mut session, received, path)?;
        Ok(session.library.display_path().to_owned())
    }
    pub(crate) fn switch_locked(&self, session: &mut Session, received: u64, path: &Path) -> Result<Json> {
        self.check_open()?;
        if !self.accepts(session, received) {
            return Err(Error::Conflict("会话已过期，请刷新页面".into()));
        }
        if !path.join("library.json").is_file() {
            return Err(Error::Invalid("不是图库包".into()));
        }
        let library = Library::open(path)?;
        // Reopening the same physical package keeps its ownership; another one is claimed first.
        let owner = match &session.owner {
            Some(owner) if library.instance_path() == session.library.instance_path() => owner.clone(),
            _ => Owner::claim(&library)?,
        };
        let mut replacement = Session::new(
            library,
            owner,
            session.generation + 1,
            self.names_enabled,
            self.names_network,
            self.provenance_enabled,
            self.provenance_network,
            &self.root,
            self.index_network,
            self.index_automatic,
        )?;
        replacement.syncer.configure_startup(&replacement.library, !self.sync_disabled)?;
        crate::app_libraries::remember(&replacement.library)?;
        let result = Json::object([
            ("path", replacement.library.display_path().to_string_lossy().as_ref().into()),
            ("name", replacement.library.name().into()),
        ]);
        replacement.auto_sync = session.auto_sync;
        replacement.research.anchors = session.research.anchors.clone();
        replacement.syncer.resource_root = session.syncer.resource_root.clone();
        replacement.syncer.per_run = session.syncer.per_run;

        session.close();
        self.exports.stop();
        *session = replacement;
        let library = session.library.clone();
        if let Err(e) = crate::app_research::dispatch(&library, "imported") {
            eprintln!("研究启动调度失败：{e}");
        }
        if self.names_enabled {
            let _ = session.names.start(&library, None, false);
        }
        self.generation.store(session.generation, Ordering::SeqCst);
        if session.auto_sync {
            let lib = session.library.clone();
            let token = session.token.clone();
            let gen = session.generation;
            if let Err(e) = session.syncer.automatic(&lib, gen, &token) {
                eprintln!("同步自动启动失败：{e}");
            }
        }
        Ok(result)
    }
}

impl Drop for Application {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl Application {
    pub(crate) fn accepts(&self, session: &Session, received: u64) -> bool {
        !self.closed.load(Ordering::SeqCst) && session.generation == received
    }
    pub(crate) fn current(&self, received: u64) -> bool {
        !self.closed.load(Ordering::SeqCst) && self.generation() == received
    }
    /// Set once the application shuts down; long work outside the session lock polls it.
    pub(crate) fn closing(&self) -> Arc<AtomicBool> {
        self.closed.clone()
    }
    fn check_open(&self) -> Result<()> {
        if self.closed.load(Ordering::SeqCst) {
            Err(Error::Conflict("应用服务已关闭".into()))
        } else {
            Ok(())
        }
    }
}
/// Explicit creation uses the same factory and reference rule as HTTP and the welcome shell.
/// A published package survives a subsequent registry failure.
pub fn create(root: &Path, dest: &Path, name: &str, sources: Vec<crate::app_create::NewSource>) -> Result<PathBuf> {
    let created = build_sources(root, dest, name, sources)?;
    let path = created["path"].as_str().ok_or_else(|| Error::Schema("建库结果缺少路径".into()))?;
    let path = PathBuf::from(path);
    crate::app_libraries::remember_created(&path, name)?;
    Ok(path)
}
pub(crate) fn build_sources(
    root: &Path,
    dest: &Path,
    name: &str,
    sources: Vec<crate::app_create::NewSource>,
) -> Result<serde_json::Value> {
    let reference = crate::app_resources::reference_path(root)?;
    crate::app_create::create_sources(dest, name, sources, crate::app_create::optional_reference(&reference)?)
        .map_err(|error| crate::app_create::creation_error(error, &reference))
}

/// 只打开已有的库：路径不存在时 SQLite 会新建空文件，先拒绝。
pub(crate) fn existing(db: &Path) -> crate::Result<rusqlite::Connection> {
    if !db.is_file() {
        return Err(Error::Invalid(format!("找不到数据库：{}", db.display())));
    }
    crate::open(db)
}

/// CLI 与常驻进程使用同一事务边界，每条命令单独连接；空闲时不持有文件或写锁。
pub(crate) fn character_request(db: &Path, input: &serde_json::Value) -> crate::Result<serde_json::Value> {
    let mut c = existing(db)?;
    crate::schema::require_current(&c)?;
    let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    if matches!(input["op"].as_str(), Some("change" | "sweep" | "answer")) {
        tx.execute("INSERT OR REPLACE INTO meta(key,value) VALUES (?1,'1')", [crate::schema::WRITER_KEY])?;
    } else {
        crate::schema::begin_writing(&tx)?;
    }
    let result = crate::character_commands::command(&tx, input)?;
    if let Some(effect) = input.get("effect").and_then(serde_json::Value::as_str) {
        tx.execute("INSERT INTO backend_effects(kind) VALUES (?)", [effect])?;
    }
    crate::schema::commit_writing(tx)?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lock_wait_rechecks_accepted_generation() {
        let root = std::env::temp_dir().join(format!(
            "illustash-service-lock-{}-{}",
            std::process::id(),
            crate::store::new_key().unwrap()
        ));
        std::fs::create_dir_all(root.join("source")).unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--ignored", "--exact", "app_service::tests::lock_wait_fixture"])
            .env("ILLUSTASH_SERVICE_FIXTURE", &root)
            .env("ILLUSTASH_APP_SUPPORT", root.join("support"))
            .env("ILLUSTASH_SHARED", root.join("absent-reference"))
            .env("ILLUSTASH_NO_KEYCHAIN", "1")
            .status()
            .unwrap();
        std::fs::remove_dir_all(root).unwrap();
        assert!(status.success());
    }
    #[test]
    #[ignore = "isolated in-process fixture, invoked with synthetic support by parent test"]
    fn lock_wait_fixture() {
        let root = PathBuf::from(std::env::var_os("ILLUSTASH_SERVICE_FIXTURE").expect("isolated fixture only"));
        let create = |name: &str| {
            create(
                &root,
                &root.join(format!("{name}.pilib")),
                name,
                vec![crate::app_create::NewSource::Folder { path: root.join("source"), preset: "other".into() }],
            )
            .unwrap()
        };
        let (first, second) = (create("first"), create("second"));
        let mut options = Options::new(root);
        options.names_enabled = false;
        options.provenance_enabled = false;
        options.index_automatic = false;
        options.auto_sync = false;
        options.sync_disabled = true;
        let service = std::sync::Arc::new(Application::open(&first, options.clone()).unwrap());
        let received = service.generation();
        let mut session = service.session.write().unwrap();
        let waiter = service.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            tx.send(()).unwrap();
            waiter.with_library::<()>(received, |_| panic!("old waiting request applied to replacement"))
        });
        rx.recv().unwrap();
        service.switch_locked(&mut session, received, &second).unwrap();
        drop(session);
        assert!(matches!(thread.join().unwrap(), Err(Error::Conflict(_))));
        // These tasks occupy the real Session Syncer slot. Its existing stop path must cancel
        // and join them; the service cannot report switching/closing while their gate is held.
        for finish in ["switch", "shutdown", "drop"] {
            let target_service = if finish == "drop" {
                std::sync::Arc::new(Application::open(&first, options.clone()).unwrap())
            } else {
                service.clone()
            };
            let (ready_tx, ready_rx) = std::sync::mpsc::channel();
            let (cancel_tx, cancel_rx) = std::sync::mpsc::channel();
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            let generation = target_service.generation();
            {
                let mut session = target_service.session.write().unwrap();
                let identity =
                    crate::app_tasks::TaskIdentity::new(&session.library, generation, &session.token).unwrap();
                let task = crate::app_tasks::Task::start(identity, None, move |ctx| {
                    ready_tx.send(ctx.clone()).unwrap();
                    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
                    while ctx.check().is_ok() {
                        assert!(std::time::Instant::now() < deadline, "service did not cancel its task");
                        std::thread::yield_now();
                    }
                    cancel_tx.send(()).unwrap();
                    release_rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
                    assert!(ctx.receive::<()>(ctx.identity(), || panic!("cancelled owned task published")).is_err());
                    Ok(())
                });
                session.syncer.install_fixture_task(task);
            }
            let context = ready_rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
            let observe = (finish != "drop").then(|| target_service.clone());
            let finishing = target_service;
            let target = first.clone();
            let (done_tx, done_rx) = std::sync::mpsc::channel();
            let thread = std::thread::spawn(move || {
                if finish == "shutdown" {
                    finishing.shutdown();
                } else if finish == "drop" {
                    drop(finishing);
                } else {
                    finishing.switch(generation, &target).unwrap();
                }
                done_tx.send(()).unwrap();
            });
            cancel_rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
            assert!(matches!(done_rx.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty)));
            if let Some(observe) = observe {
                if finish == "shutdown" {
                    assert!(!observe.current(generation));
                } else {
                    assert_eq!(observe.generation(), generation);
                }
            }
            release_tx.send(()).unwrap();
            done_rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
            thread.join().unwrap();
            assert!(context.receive::<()>(context.identity(), || panic!("joined owned task published")).is_err());
        }
        assert!(service.with_library(service.generation(), |_| Ok(())).is_err());
    }
}
