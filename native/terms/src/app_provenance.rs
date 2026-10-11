//! Native provenance coordination; optional computation workers never commit formal application state.
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
use crate::{
    app_library::Library,
    app_tasks::{Task, TaskContext, TaskIdentity},
    http_json::Json,
    Error, Result,
};
use crate::{
    task_state::{State, Status},
    worker_protocol as protocol,
};
use rusqlite::params;
use serde_json::{json, Value};
use std::{
    io::Read,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
#[path = "app_provenance_cache.rs"]
pub(crate) mod cache;
#[path = "app_provenance_net.rs"]
mod net;
#[path = "app_provenance_sources.rs"]
pub(crate) mod sources;
const KINDS: [(&str, &str, usize); 4] = [
    ("full", "local_match,pixel_hash,iqdb,x_following,pixiv_following,groups", 1024),
    ("new", "local_match,pixel_hash,iqdb,groups", 50),
    ("pick", "pixel_hash,iqdb,groups", 20),
    ("artist", "verify,groups", 200),
];
fn invalid<T>(s: &str) -> Result<T> {
    Err(Error::Invalid(s.into()))
}
fn now() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}
fn read_json(path: &Path) -> Result<Value> {
    let mut b = Vec::new();
    let options = crate::platform_filesystem::read_nofollow_options();
    options.open(path)?.take(1048577).read_to_end(&mut b)?;
    if b.len() > 1048576 {
        return invalid(&format!("{}: worker 响应过大", path.display()));
    }
    crate::strict_json::parse(
        std::str::from_utf8(&b).map_err(|e| Error::Invalid(format!("{}: worker 响应编码无效: {e}", path.display())))?,
    )
    .map_err(|e| Error::Invalid(format!("{}: {e}", path.display())))
}
fn write_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let tmp = path.with_extension(format!("{}.tmp", crate::store::new_key()?));
    let result = (|| {
        let mut f = std::fs::OpenOptions::new().create_new(true).write(true).open(&tmp)?;
        f.write_all(bytes)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(tmp);
    }
    result
}
fn write_json(path: &Path, value: &Value) -> Result<()> {
    write_bytes(path, crate::strict_json::canonical(value).as_bytes())
}

pub(crate) struct Runner {
    library: Library,
    generation: u64,
    session: String,
    task: Option<Task>,
    state: Arc<Mutex<State>>,
    delivery: Arc<Mutex<Json>>,
    history: Mutex<Option<cache::History>>,
    initialized: bool,
    pub enabled: bool,
    pub authorized: bool,
    worker: Option<PathBuf>,
    idle_timeout: Duration,
    pending: bool,
    observed_sync: i64,
    retry: Instant,
    lease: std::fs::File,
}
impl Runner {
    pub fn new(library: &Library, generation: u64, session: &str, enabled: bool, authorized: bool) -> Result<Self> {
        let library = library.clone();
        let path = library.instance_path().join("provenance.sqlite");
        let worker = std::env::var_os("ILLUSTASH_PROVENANCE_WORKER").map(PathBuf::from);
        let native = path.is_file()
            && crate::app_library::read_connection(&path)?.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='native_meta')",
                [],
                |r| r.get::<_, bool>(0),
            )?;
        // Reading an ordinary package or account archive must not extend its cache schema.
        let c = if native || (enabled && !library.frozen() && worker.is_some()) {
            Some(cache::connect(&path)?)
        } else {
            None
        };
        let initialized = c.is_some();
        let lease = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(library.instance_path().join(".provenance.lock"))?;
        let owned = lock(&lease);
        if let Some(c) = c.as_ref().filter(|_| owned) {
            c.execute("INSERT OR IGNORE INTO pending_runs(kind) SELECT kind FROM runs WHERE status IN ('running','stopping') AND kind='new'",[])?;
            c.execute("UPDATE runs SET status='stopped',reason=CASE WHEN reason='' THEN '中途退出' ELSE reason END,finished_at=datetime('now') WHERE status IN ('running','stopping')",[])?;
        }
        if owned {
            unlock(&lease);
        }
        let state = if path.is_file() { cache::latest(&path)? } else { Json::object([("status", "idle".into())]) };
        let state = State::parse(state, &path).unwrap_or_else(|e| {
            State::from(Json::object([("status", "failed".into()), ("reason", e.to_string().into())]))
        });
        let delivery = if owned && initialized {
            cache::deliver(&library, &path)
                .unwrap_or_else(|e| Json::object([("status", "failed".into()), ("error", e.to_string().into())]))
        } else {
            Json::Scalar(Value::Null)
        };
        let pending = if path.is_file() {
            crate::app_library::read_connection(&path)?.query_row(
                "SELECT EXISTS(SELECT 1 FROM pending_runs WHERE kind='new')",
                [],
                |r| r.get(0),
            )?
        } else {
            false
        };
        let app = crate::app_library::read_connection(&library.db())?;
        let has_sync = app.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='sync_runs')", [], |r| {
            r.get::<_, bool>(0)
        })?;
        let initial = if has_sync {
            app.query_row("SELECT coalesce(max(id),0) FROM sync_runs", [], |r| r.get::<_, i64>(0))?
        } else {
            0
        };
        let observed_sync = if let Some(c) = &c {
            c.execute("INSERT OR IGNORE INTO native_meta(key,value) VALUES ('sync_seen',?)", [initial.to_string()])?;
            c.query_row("SELECT value FROM native_meta WHERE key='sync_seen'", [], |r| r.get::<_, String>(0))?
                .parse()
                .map_err(|_| Error::Invalid("同步补跑游标无效".into()))?
        } else {
            initial
        };
        let idle_timeout = std::env::var("ILLUSTASH_PROVENANCE_IDLE_SECONDS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(600);
        Ok(Self {
            library,
            generation,
            session: session.into(),
            task: None,
            state: Arc::new(Mutex::new(state)),
            delivery: Arc::new(Mutex::new(delivery)),
            history: Mutex::new(if path.is_file() { Some(cache::History::new(&path)?) } else { None }),
            initialized,
            enabled,
            authorized,
            worker,
            idle_timeout: Duration::from_secs(idle_timeout),
            pending,
            observed_sync,
            retry: Instant::now(),
            lease,
        })
    }
    fn path(&self) -> PathBuf {
        self.library.instance_path().join("provenance.sqlite")
    }
    fn busy(&self) -> bool {
        self.task.as_ref().is_some_and(Task::running)
    }
    pub fn info(&self) -> Result<Json> {
        let mut history = self.history.lock().expect("provenance state mutex poisoned");
        if history.is_none() && self.path().is_file() {
            *history = Some(cache::History::new(&self.path())?);
        }
        let (runs, coverage) = if let Some(history) = &mut *history {
            history.read()?
        } else {
            (Json::Array(vec![]), Json::Scalar(Value::Null))
        };
        drop(history);
        let pref = crate::app_settings::startup(&self.library)?;
        Ok(Json::object([
            ("available", true.into()),
            ("state", self.state.lock().expect("provenance state mutex poisoned").snapshot()),
            ("runs", runs),
            ("coverage", coverage),
            ("auto", pref.get("provenance").cloned().unwrap_or(true.into())),
            ("budgets", Json::object(KINDS.iter().map(|(k, _, b)| (*k, (*b as i64).into())))),
            ("frozen", self.library.frozen().into()),
            ("auto_network", self.authorized.into()),
            ("delivery", self.delivery.lock().expect("provenance state mutex poisoned").clone()),
        ]))
    }
    pub fn post(&mut self, body: &Json) -> Result<Json> {
        if !self.enabled {
            return invalid("本次启动关闭了找回画师");
        }
        match body.get("action").map(Json::text).unwrap_or("") {
            "start" => {
                if !self.start(
                    "full",
                    matches!(body.get("allow_network"), Some(Json::Scalar(Value::Bool(true)))),
                    vec![],
                    vec![],
                )? {
                    return invalid("正在查找，稍等");
                }
            }
            "stop" => self.stop(),
            action @ ("lookup" | "artist") => {
                let (only, artists) = if action == "lookup" {
                    let Some(Json::Array(ids)) = body.get("ids") else {
                        return invalid("要查找的图片不对（一次最多 500 张）");
                    };
                    if ids.is_empty()
                        || ids.len() > 500
                        || ids.iter().any(|v| v.text().is_empty() || v.text().chars().count() > 64)
                    {
                        return invalid("要查找的图片不对（一次最多 500 张）");
                    }
                    (ids.iter().map(|v| v.text().into()).collect(), vec![])
                } else {
                    let Some(Json::Scalar(Value::String(artist))) = body.get("artist") else {
                        return invalid("画师不对");
                    };
                    if artist.is_empty() || artist.chars().count() > 64 {
                        return invalid("画师不对");
                    }
                    (vec![], vec![artist.clone()])
                };
                if self.busy() {
                    let kind = self
                        .state
                        .lock()
                        .expect("provenance state mutex poisoned")
                        .get("kind")
                        .map(Json::text)
                        .unwrap_or("")
                        .to_owned();
                    if ["pick", "artist"].contains(&kind.as_str()) {
                        return invalid("正在查找，稍等");
                    }
                    if kind == "new" {
                        self.queue()?;
                    }
                    self.stop();
                }
                if !self.start(if action == "lookup" { "pick" } else { "artist" }, true, only, artists)? {
                    return invalid("正在查找，稍等");
                }
            }
            _ => return invalid("未知操作"),
        }
        self.info()
    }
    fn initialize(&mut self) -> Result<()> {
        if !self.initialized {
            cache::connect(&self.path())?.execute(
                "INSERT OR IGNORE INTO native_meta(key,value) VALUES ('sync_seen',?)",
                [self.observed_sync.to_string()],
            )?;
            self.initialized = true;
        }
        Ok(())
    }
    fn queue(&mut self) -> Result<()> {
        self.initialize()?;
        cache::connect(&self.path())?.execute("INSERT OR IGNORE INTO pending_runs(kind) VALUES ('new')", [])?;
        self.pending = true;
        Ok(())
    }
    pub fn recover(&mut self) {
        if !self.enabled || self.library.frozen() {
            return;
        }
        let pref = crate::app_settings::startup(&self.library)
            .ok()
            .and_then(|j| j.get("provenance").cloned())
            .is_none_or(|j| crate::app_service::truthy(&j));
        let c = crate::app_library::read_connection(&self.library.db());
        if let Ok(c) = c {
            let row: Result<Option<i64>> = c
                .query_row(
                    "SELECT max(id) FROM sync_runs WHERE id>? AND status<>'running' AND imported>0",
                    [self.observed_sync],
                    |r| r.get(0),
                )
                .map_err(Into::into);
            if let Ok(Some(id)) = row {
                if self.initialize().is_err() {
                    return;
                }
                if let Ok(mut cache) = cache::connect(&self.path()) {
                    if let Ok(tx) = cache.transaction() {
                        let result = (|| -> Result<()> {
                            if pref {
                                tx.execute("INSERT OR IGNORE INTO pending_runs(kind) VALUES ('new')", [])?;
                            }
                            tx.execute("UPDATE native_meta SET value=? WHERE key='sync_seen'", [id.to_string()])?;
                            Ok(())
                        })();
                        if result.is_ok() && tx.commit().is_ok() {
                            self.observed_sync = id;
                            if pref {
                                self.pending = true;
                            }
                        }
                    }
                }
            }
        }
        if self.busy() {
            return;
        }
        if self.task.take().is_some() {
            unlock(&self.lease);
        } else if matches!(
            self.state.lock().expect("provenance state mutex poisoned").status(),
            Status::Running | Status::Stopping
        ) {
            // A same-path replacement can be constructed while the old session owns
            // the lease. Refresh its inherited state after that owner has closed.
            if let Ok(latest) = cache::latest(&self.path()) {
                let latest = State::parse(latest, &self.path()).unwrap_or_else(|e| {
                    State::from(Json::object([("status", "failed".into()), ("reason", e.to_string().into())]))
                });
                *self.state.lock().expect("provenance state mutex poisoned") = latest;
            }
        }
        if Instant::now() < self.retry {
            return;
        }
        self.retry = Instant::now() + Duration::from_secs(60);
        if lock(&self.lease) {
            if let Ok(c) = crate::app_library::read_connection(&self.path()) {
                self.pending = c
                    .query_row("SELECT EXISTS(SELECT 1 FROM pending_runs WHERE kind='new')", [], |r| r.get(0))
                    .unwrap_or(self.pending);
            }
            let replay = if self.initialized {
                cache::deliver(&self.library, &self.path())
            } else {
                Ok(Json::Scalar(Value::Null))
            };
            match replay {
                Ok(v) if !matches!(v, Json::Scalar(Value::Null)) => {
                    *self.delivery.lock().expect("provenance state mutex poisoned") = v
                }
                Err(e) => {
                    *self.delivery.lock().expect("provenance state mutex poisoned") =
                        Json::object([("status", "failed".into()), ("error", e.to_string().into())])
                }
                _ => {}
            }
            unlock(&self.lease);
        }
        if pref && self.pending && self.worker.is_some() {
            let _ = self.start("new", self.authorized, vec![], vec![]);
        }
    }
    pub fn start(&mut self, kind: &str, authorized: bool, only: Vec<String>, artists: Vec<String>) -> Result<bool> {
        let Some((_, steps, budget)) = KINDS.iter().find(|(k, _, _)| *k == kind) else {
            return invalid("未知的运行方式");
        };
        if self.library.frozen() {
            return invalid("冻结图库不能找回画师");
        }
        if !self.enabled {
            return Ok(false);
        }
        if self.busy() {
            if kind == "new" {
                self.queue()?;
            }
            return Ok(false);
        }
        if let Some(mut task) = self.task.take() {
            task.stop();
            unlock(&self.lease);
        }
        if !lock(&self.lease) {
            if kind == "new" {
                self.queue()?;
            }
            return Ok(false);
        }
        self.retry = Instant::now();
        let result = (|| {
            self.initialize()?;
            let identity = TaskIdentity::new(&self.library, self.generation, &self.session)?;
            let c = cache::connect(&self.path())?;
            c.execute("INSERT INTO runs(kind,steps) VALUES (?,?)", [kind, *steps])?;
            let id = c.last_insert_rowid();
            if kind == "new" {
                c.execute("DELETE FROM pending_runs WHERE kind='new'", [])?;
                self.pending = false;
            }
            *self.state.lock().expect("provenance state mutex poisoned") = Json::object([
                ("status", "running".into()),
                ("kind", kind.into()),
                ("started_at", now().into()),
                ("budget_mb", (*budget as i64).into()),
                ("step", "".into()),
                ("index", 0i64.into()),
                ("total", (steps.split(',').count() as i64).into()),
                ("text", "正在启动…".into()),
            ])
            .into();
            let (lib, state, delivery, worker, timeout) = (
                self.library.clone(),
                self.state.clone(),
                self.delivery.clone(),
                self.worker.clone(),
                self.idle_timeout,
            );
            let (kind, steps, budget) = (kind.to_owned(), steps.to_string(), *budget);
            self.task = Some(Task::start(identity, None, move |ctx| {
                let result = run(
                    &ctx,
                    &lib,
                    id,
                    &kind,
                    &steps,
                    budget,
                    authorized,
                    &only,
                    &artists,
                    worker.as_deref(),
                    timeout,
                    &state,
                    &delivery,
                );
                if let Err(e) = &result {
                    let timed_out = e.to_string().contains("无进展超时");
                    let status = if ctx.check().is_err() && !timed_out { "stopped" } else { "failed" };
                    let reason = if status == "stopped" { "已手动停止或超时".into() } else { e.to_string() };
                    if let Ok(c) = cache::connect(&lib.instance_path().join("provenance.sqlite")) {
                        let _ = c.execute(
                            "UPDATE runs SET status=?,reason=?,finished_at=datetime('now') WHERE id=?",
                            params![status, reason, id],
                        );
                    }
                    let mut s = state.lock().expect("provenance state mutex poisoned");
                    if timed_out {
                        s.set("timed_out", true.into());
                    }
                    s.remove("text");
                    s.set("status", status.into());
                    s.set("reason", reason.into());
                    s.set("finished_at", now().into());
                }
                result
            }));
            Ok(true)
        })();
        if result.is_err() {
            unlock(&self.lease);
        }
        result
    }
    pub fn close(&mut self) {
        if self.busy()
            && self
                .state
                .lock()
                .expect("provenance state mutex poisoned")
                .get("kind")
                .is_some_and(|v| v.text() == "new")
        {
            let _ = self.queue();
        }
        self.stop();
    }
    pub fn stop(&mut self) {
        if let Some(mut task) = self.task.take() {
            task.stop();
        }
        unlock(&self.lease);
    }
}
impl Drop for Runner {
    fn drop(&mut self) {
        self.close();
    }
}
fn lock(file: &std::fs::File) -> bool {
    crate::app_file_lock::acquire(file).is_ok()
}
fn unlock(file: &std::fs::File) {
    let _ = crate::app_file_lock::release(file);
}

#[allow(clippy::too_many_arguments)]
fn run(
    ctx: &TaskContext,
    lib: &Library,
    id: i64,
    kind: &str,
    steps: &str,
    budget: usize,
    authorized: bool,
    only: &[String],
    artists: &[String],
    worker: Option<&Path>,
    timeout: Duration,
    state: &Arc<Mutex<State>>,
    delivery: &Arc<Mutex<Json>>,
) -> Result<()> {
    let path = lib.instance_path().join("provenance.sqlite");
    let dir = lib.instance_path().join(".provenance").join(&ctx.identity().task_id);
    std::fs::create_dir_all(&dir)?;
    let staged = dir.join("provenance.sqlite");
    cache::backup(&path, &staged)?;
    let mut net = net::Net::new(authorized, budget)?;
    if steps.split(',').any(|s| ["verify", "pixiv_following"].contains(&s)) {
        net.prepare_pixiv(ctx, kind)?;
    }
    let before = cache::connect(&staged)?.query_row(
        "SELECT count(DISTINCT asset) FROM image_artists WHERE status='auto'",
        [],
        |r| r.get::<_, i64>(0),
    )?;
    let mut results = serde_json::Map::new();
    let mut reasons = Vec::new();
    let mut status = "done";
    for (index, step) in steps.split(',').enumerate() {
        ctx.check()?;
        {
            let mut s = state.lock().expect("provenance state mutex poisoned");
            s.set("step", step.into());
            s.set("index", (index as i64).into());
            s.set("text", format!("{}…", step_name(step)).into());
            s.remove("progress");
        }
        let result =
            if !authorized && ["pixel_hash", "iqdb", "x_following", "pixiv_following", "verify"].contains(&step) {
                json!({"stopped":"外部请求未授权"})
            } else {
                match step {
                    "x_following" => sources::x_following(ctx, &staged, &mut net)?,
                    "pixiv_following" => sources::pixiv_following(ctx, &staged, &mut net)?,
                    "pixel_hash" | "iqdb" => {
                        let v = compute(ctx, lib, &dir, step, only, artists, worker, timeout, &mut net, state)?;
                        let v = sources::validate_hashes(lib, &staged, step, &v, only)?;
                        sources::hashes(ctx, &staged, &mut net, step, &v)?
                    }
                    _ => compute(ctx, lib, &dir, step, only, artists, worker, timeout, &mut net, state)?,
                }
            };
        if result.get("failed").and_then(Value::as_u64).unwrap_or(0) > 0 {
            status = "failed";
            reasons.push(format!("{step}：研究步骤失败"));
        }
        if let Some(reason) = result["stopped"].as_str().filter(|s| !s.is_empty()) {
            if status != "failed" {
                status = "stopped";
            }
            if !reasons.iter().any(|s| s == reason) {
                reasons.push(reason.into());
            }
        }
        {
            let mut s = state.lock().expect("provenance state mutex poisoned");
            let mut out = s.get("results").cloned().unwrap_or_else(|| Json::Array(vec![]));
            if let Json::Array(rows) = &mut out {
                rows.push(Json::object([("step", step.into()), ("result", result_json(step, &result))]));
            }
            s.set("results", out);
        }
        results.insert(format!("{}.{}", index + 1, step), result);
    }
    validate_cache(lib, &staged, &path)?;
    let pkg = cache::package(lib, &staged, &dir.join("evidence"))?;
    let found = cache::connect(&staged)?.query_row(
        "SELECT count(DISTINCT asset) FROM image_artists WHERE status='auto'",
        [],
        |r| r.get::<_, i64>(0),
    )? - before;
    let report = sources::report(lib, &staged)?;
    ctx.receive(ctx.identity(),||{
        let mut c=cache::connect(&path)?;
        c.execute("ATTACH DATABASE ? AS stage",[crate::sqlite::path(&staged).to_string_lossy().as_ref()])?;
        c.execute_batch("PRAGMA defer_foreign_keys=ON")?;
        let tx=c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        for table in ["artists","artist_links","artist_names","image_sources","image_artists","seeds","lookups","marks","ref_sigs","ext_cache"] {
            tx.execute(&format!("DELETE FROM {table}"),[])?;
            tx.execute(&format!("INSERT INTO {table} SELECT * FROM stage.{table}"),[])?;
        }
        // Account archive imports may add leads during a research run: preserve new leads.
        tx.execute("INSERT INTO leads SELECT * FROM stage.leads WHERE 1 ON CONFLICT(source,system,key) DO UPDATE SET status=excluded.status,artist_id=excluded.artist_id,next_due_at=excluded.next_due_at",[])?;
        tx.execute("INSERT INTO native_delivery(task_id,package) VALUES (?,?)",params![ctx.identity().task_id,format!(".provenance/{}/evidence",ctx.identity().task_id)])?;
        tx.execute("UPDATE runs SET status=?,reason=?,results=?,found=?,traffic_mb=?,report=?,finished_at=datetime('now') WHERE id=?",params![status,reasons.join("；"),Value::Object(results).to_string(),found,(net.used as f64/1048576.0*100.0).round()/100.0,crate::character_json::dump(&report,false),id])?;
        tx.commit()?;
        *delivery.lock().expect("provenance state mutex poisoned")=match cache::deliver(lib,&path){Ok(v)=>v,Err(e)=>Json::object([("status","failed".into()),("error",e.to_string().into())])};
        let mut s=state.lock().expect("provenance state mutex poisoned");s.remove("text");s.set("status",status.into());s.set("reason",reasons.join("；").into());s.set("found",found.into());s.set("traffic_mb",Json::Float((net.used as f64/1048576.0*100.0).round()/100.0));s.set("finished_at",now().into());
        Ok(())
    })?;
    let _ = pkg;
    // The receipt and terminal state are committed. Cleanup failure must not relabel
    // accepted evidence as a failed run; recovery uses only the sealed evidence package.
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            if entry.file_name() == "evidence" {
                continue;
            }
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                let _ = std::fs::remove_dir_all(entry.path());
            } else {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    Ok(())
}
#[allow(clippy::too_many_arguments)]
fn compute(
    ctx: &TaskContext,
    lib: &Library,
    dir: &Path,
    step: &str,
    only: &[String],
    artists: &[String],
    worker: Option<&Path>,
    timeout: Duration,
    net: &mut net::Net,
    state: &Arc<Mutex<State>>,
) -> Result<Value> {
    let Some(worker) = worker else {
        return Ok(json!({"failed":1,"stopped":"未配置溯源研究 worker"}));
    };
    let step_dir = dir.join(step);
    std::fs::create_dir_all(&step_dir)?;
    let request = step_dir.join("request.json");
    let response = step_dir.join("response.json");
    write_json(
        &request,
        &protocol::value(&protocol::ProvenanceRequest {
            format: protocol::Format::Provenance,
            identity: ctx.identity(),
            owner_pid: std::process::id(),
            library: lib.instance_path(),
            cache: dir,
            step,
            only,
            artists,
            network: net.authorized,
            mailbox: &step_dir,
        })?,
    )?;
    let mut command = Command::new(worker);
    command.args([&request, &response]);
    // The network broker stays in Rust; computation has no credential or login interface.
    std::thread::scope(|scope| -> Result<()> {
        let child = scope.spawn(|| ctx.process(&mut command));
        struct CancelOnError<'a>(&'a TaskContext, bool);
        impl Drop for CancelOnError<'_> {
            fn drop(&mut self) {
                if self.1 {
                    self.0.cancel();
                }
            }
        }
        let mut guard = CancelOnError(ctx, true);
        let mut heartbeat = Instant::now();
        let mut last_progress = String::new();
        let mut failure = None;
        while !child.is_finished() {
            let progress =
                read_json(&step_dir.join("progress.json")).ok().filter(Value::is_object).map(|v| v.to_string());
            if let Some(progress) = progress {
                if progress != last_progress {
                    last_progress = progress.clone();
                    heartbeat = Instant::now();
                    if let Ok(v) = crate::http_json::parse_request(&progress) {
                        let mut s = state.lock().expect("provenance state mutex poisoned");
                        if let Some(text) = v.get("log") {
                            s.set("text", text.text().chars().take(200).collect::<String>().into());
                        }
                        if let Some(numbers) = v.get("progress") {
                            s.set("progress", numbers.clone());
                        }
                    }
                }
            }
            if heartbeat.elapsed() > timeout {
                ctx.cancel();
                failure = Some(Error::Invalid("研究 worker 无进展超时".into()));
                break;
            }
            let call = step_dir.join("net-request.json");
            if call.is_file() {
                let result = (|| {
                    let request: protocol::NetRequest = protocol::parse(read_json(&call)?, &call)?;
                    if &request.identity != ctx.identity() {
                        return invalid("外部查询任务已过期");
                    }
                    net.get(ctx, &request.url)
                })();
                let value = match result {
                    Ok((code, bytes)) => {
                        let body = step_dir.join("net-body.bin");
                        write_bytes(&body, &bytes)?;
                        json!({"status":code,"used":net.used})
                    }
                    Err(e) => json!({"error":e.to_string(),"used":net.used}),
                };
                std::fs::remove_file(call)?;
                write_json(&step_dir.join("net-response.json"), &value)?;
                heartbeat = Instant::now();
            }
            if let Err(e) = ctx.check() {
                failure = Some(e);
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        if failure.is_some() {
            ctx.cancel();
        }
        let result = child.join().map_err(|_| Error::Invalid("研究 worker 线程崩溃".into()))?;
        if let Some(e) = failure {
            return Err(e);
        }
        let status = result?;
        guard.1 = false;
        if !status.success() {
            return invalid("研究 worker 崩溃或退出失败");
        }
        Ok(())
    })?;
    ctx.check()?;
    if std::fs::symlink_metadata(dir.join("provenance.sqlite"))?.file_type().is_symlink() {
        return invalid("研究缓存不能是符号链接");
    }
    let result: protocol::ProvenanceResponse = protocol::parse(read_json(&response)?, &response)?;
    if result.format != protocol::Format::Provenance {
        return invalid("研究结果格式无效");
    }
    if &result.identity != ctx.identity() || result.step != step {
        return invalid("研究结果已过期");
    }
    let result = protocol::value(&result.result)?;
    if result.get("hashes").is_some_and(|v| v == "hashes.json") {
        let hash_path = step_dir.join("hashes.json");
        if std::fs::symlink_metadata(&hash_path)?.file_type().is_symlink() {
            return invalid("签名文件不能是符号链接");
        }
        let mut bytes = Vec::new();
        std::fs::File::open(hash_path)?.take(64 * 1048576 + 1).read_to_end(&mut bytes)?;
        if bytes.len() > 64 * 1048576 {
            return invalid("签名文件过大");
        }
        let hashes: Vec<protocol::HashEntry> = protocol::parse(
            serde_json::from_slice(&bytes)
                .map_err(|e| Error::Invalid(format!("{}: {e}", step_dir.join("hashes.json").display())))?,
            &step_dir.join("hashes.json"),
        )?;
        return protocol::value(&hashes);
    }
    Ok(result)
}

fn validate_cache(lib: &Library, staged: &Path, original: &Path) -> Result<()> {
    let app = crate::app_library::read_connection(&lib.db())?;
    let present = app
        .prepare("SELECT uuid FROM assets")?
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<std::collections::BTreeSet<_>>>()?;
    let c = crate::app_library::read_connection(staged)?;
    c.execute("ATTACH DATABASE ? AS original", [crate::sqlite::path(original).to_string_lossy().as_ref()])?;
    for table in ["image_artists", "image_sources", "marks"] {
        let sql = format!("SELECT DISTINCT asset FROM {table} WHERE asset NOT IN (SELECT asset FROM original.{table})");
        for row in c.prepare(&sql)?.query_map([], |r| r.get::<_, String>(0))? {
            if !present.contains(&row?) {
                return invalid("研究结果含当前包范围外的图片");
            }
        }
    }
    Ok(())
}

fn step_name(step: &str) -> &str {
    match step {
        // Runs recorded before OCR computation was removed (#177) still display.
        "ocr" => "识别图上的签名",
        "groups" => "分组推荐",
        "local_match" => "本地重比新图",
        "pixel_hash" => "像素哈希查 Danbooru",
        "iqdb" => "IQDB 查相似图",
        "x_following" => "对应 X 关注",
        "pixiv_following" => "读取 Pixiv 关注",
        "verify" => "本地比对画师作品",
        _ => step,
    }
}
fn result_json(step: &str, value: &Value) -> Json {
    let keys: &[&str] = match step {
        "pixel_hash" => &["looked", "hits", "misses", "unreadable", "failed", "stopped"],
        "iqdb" => &["asked", "hits", "artists", "failed", "stopped"],
        "x_following" => &["asked", "resolved", "unresolved", "stopped"],
        "pixiv_following" => &["candidates", "pages", "stopped"],
        "groups" => &["auto", "candidates", "stopped"],
        "ocr" => &["processed", "candidates", "failed", "stopped"],
        "local_match" => &["pictures", "matches", "stopped"],
        "verify" => &["artists", "matched_artists", "matches", "cold", "stopped"],
        _ => &[],
    };
    if keys.is_empty() {
        return value.clone().into();
    }
    let mut out = Json::object(keys.iter().filter_map(|key| value.get(*key).map(|v| (*key, v.clone().into()))));
    if let Some(fields) = value.as_object() {
        for (key, value) in fields {
            if !keys.contains(&key.as_str()) {
                out.set(key, value.clone().into());
            }
        }
    }
    out
}
