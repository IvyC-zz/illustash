//! Optional calculation workers operate on snapshots. The native owner validates and publishes.
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
use crate::{
    app_library::Library,
    app_tasks::{Task, TaskContext, TaskIdentity},
    http_json::Json,
    Error, Result,
};
use crate::{task_state::State, worker_protocol as protocol};
use rusqlite::{
    types::{Value as Sql, ValueRef},
    Connection, TransactionBehavior,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    time::Duration,
};

pub fn configured() -> bool {
    std::env::var_os("ILLUSTASH_RESEARCH_WORKER").is_some_and(|p| !p.is_empty())
}
fn bad() -> Error {
    Error::Invalid("研究结果格式不正确".into())
}
// Publication-time conflicts: the snapshot was valid but newer input arrived. Only these retry.
const INPUT_CHANGED: &str = "研究输入已改变，请重试";
const LEARNING_CHANGED: &str = "学习输入已改变";
/// Automatic retries after an input conflict; the delay doubles from the base each time.
const CONFLICT_RETRIES: u32 = 5;
fn input_changed(e: &Error) -> bool {
    matches!(e, Error::Conflict(m) if m == INPUT_CHANGED || m == LEARNING_CHANGED)
}
fn retry_base() -> Duration {
    Duration::from_millis(
        std::env::var("ILLUSTASH_RESEARCH_RETRY_MS").ok().and_then(|s| s.parse().ok()).unwrap_or(2000),
    )
}
const CACHES: &[(&str, usize)] = &[
    ("clusters", 2),
    ("cluster_info", 7),
    ("image_sigs", 4),
    ("relations", 6),
    ("similar_paired", 1),
    ("similar_retry", 2),
];
fn hash_db(c: &Connection, h: &mut Sha256, ctx: &TaskContext) -> Result<()> {
    fn feed(h: &mut Sha256, b: &[u8]) {
        h.update((b.len() as u64).to_le_bytes());
        h.update(b);
    }
    let tables = c.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' AND name NOT IN ('backend_effects','research_runs') ORDER BY name")?.query_map([],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
    for table in tables {
        ctx.check()?;
        feed(h, table.as_bytes());
        let quoted = table.replace('"', "\"\"");
        let n = c.prepare(&format!("SELECT * FROM \"{quoted}\""))?.column_count();
        let order = (1..=n).map(|i| i.to_string()).collect::<Vec<_>>().join(",");
        let mut q = c.prepare(&format!("SELECT * FROM \"{quoted}\" ORDER BY {order}"))?;
        let mut rows = q.query([])?;
        while let Some(r) = rows.next()? {
            ctx.check()?;
            feed(h, b"row");
            for i in 0..n {
                match r.get_ref(i)? {
                    ValueRef::Null => feed(h, b"null"),
                    ValueRef::Integer(v) => {
                        feed(h, b"int");
                        feed(h, &v.to_le_bytes());
                    }
                    ValueRef::Real(v) => {
                        feed(h, b"real");
                        feed(h, &v.to_bits().to_le_bytes());
                    }
                    ValueRef::Text(v) => {
                        feed(h, b"text");
                        feed(h, v);
                    }
                    ValueRef::Blob(v) => {
                        feed(h, b"blob");
                        feed(h, v);
                    }
                }
            }
        }
    }
    Ok(())
}
fn fingerprint(
    app: &Connection,
    src: &Connection,
    library: &Library,
    anchors: &Path,
    ctx: &TaskContext,
) -> Result<String> {
    let mut h = Sha256::new();
    hash_db(app, &mut h, ctx)?;
    hash_db(src, &mut h, ctx)?;
    // Base feature/anchor files are inputs too. Changed vectors invalidate an in-flight result.
    let mut files = vec![
        library.instance_path().join("features/full-manifest.csv"),
        library.instance_path().join("features/full-features.npy"),
        anchors.join("char-anchors.npy"),
        anchors.join("char-anchors.txt"),
    ];
    if let Ok(entries) = fs::read_dir(anchors.join("char-index")) {
        for entry in entries {
            let p = entry?.path();
            if p.extension().is_some_and(|s| s == "json") {
                files.push(p);
            }
        }
    }
    files.sort();
    for p in files {
        ctx.check()?;
        let name = p.to_string_lossy();
        h.update(name.as_bytes());
        if let Ok(mut f) = fs::File::open(p) {
            let mut bytes = [0; 65536];
            loop {
                ctx.check()?;
                let n = f.read(&mut bytes)?;
                if n == 0 {
                    break;
                }
                h.update(&bytes[..n]);
            }
        }
    }
    let mut q = app.prepare("SELECT src_path FROM assets ORDER BY uuid")?;
    for path in q.query_map([], |r| r.get::<_, Option<String>>(0))? {
        ctx.check()?;
        let path = path?.unwrap_or_default();
        if let Ok(m) = fs::metadata(&path) {
            h.update(m.len().to_le_bytes());
            if let Ok(t) =
                m.modified().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).map_err(std::io::Error::other))
            {
                h.update(t.as_nanos().to_le_bytes());
            }
        } else {
            h.update(b"missing");
        }
    }
    Ok(format!("{:x}", h.finalize()))
}
struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn backup(c: &Connection, path: &Path, ctx: &TaskContext) -> Result<()> {
    let mut out = crate::sqlite::open(path)?;
    let copy = rusqlite::backup::Backup::new(c, &mut out)?;
    loop {
        ctx.check()?;
        match copy.step(256)? {
            rusqlite::backup::StepResult::Done => break,
            rusqlite::backup::StepResult::More => {}
            _ => std::thread::sleep(Duration::from_millis(10)),
        }
    }
    Ok(())
}
fn queue_schema(c: &Connection) -> Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS research_runs(id TEXT PRIMARY KEY,kind TEXT NOT NULL,status TEXT NOT NULL,error TEXT NOT NULL DEFAULT '',created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);")?;
    Ok(())
}
/// Recognition acknowledges a durable scheduling intent; computation remains separately pending.
pub fn dispatch(library: &Library, kind: &str) -> Result<()> {
    if !configured() || library.frozen() {
        return Ok(());
    }
    let c = crate::sqlite::open(library.db())?;
    crate::configure(&c)?;
    queue_schema(&c)?;
    let events = c
        .prepare("SELECT id FROM backend_effects WHERE kind=? AND status='pending' ORDER BY id")?
        .query_map([kind], |r| r.get::<_, i64>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if events.is_empty() {
        c.execute("INSERT INTO research_runs(id,kind,status) VALUES ('startup',?,'pending') ON CONFLICT(id) DO UPDATE SET status='pending',error='' WHERE status='done'",[kind])?;
    } else {
        for id in events {
            c.execute(
                "INSERT OR IGNORE INTO research_runs(id,kind,status) VALUES (?,?,'pending')",
                rusqlite::params![format!("effect:{kind}:{id}"), kind],
            )?;
        }
    }
    Ok(())
}
pub struct Research {
    task: Option<Task>,
    state: Arc<Mutex<State>>,
    generation: u64,
    session: String,
    pub similar_enabled: bool,
    pub anchors: PathBuf,
    paused: bool,
    retry_at: Option<std::time::Instant>,
    /// Set when automatic conflict retries ran out; recovery then waits for an explicit start.
    exhausted: Arc<AtomicBool>,
}
impl Research {
    pub fn new(generation: u64, session: &str, similar_enabled: bool) -> Self {
        Self {
            generation,
            session: session.into(),
            similar_enabled,
            state: Arc::new(Mutex::new(json!({"status":if configured(){"idle"}else{"off"}}).into())),
            task: None,
            anchors: PathBuf::new(),
            paused: false,
            retry_at: None,
            exhausted: Arc::new(AtomicBool::new(false)),
        }
    }
    pub fn state(&self) -> Json {
        self.state.lock().expect("research state mutex poisoned").snapshot()
    }
    pub fn similar_state(&self) -> Json {
        if !self.similar_enabled {
            return Json::object([("status", "off".into())]);
        }
        let raw = self.state();
        let keys = if raw.get("status").map(Json::text) == Some("idle") {
            vec!["status", "compared", "found", "seconds", "at"]
        } else {
            vec!["status", "done", "total", "found", "message"]
        };
        Json::Object(keys.into_iter().filter_map(|k| raw.get(k).cloned().map(|v| (k.to_owned(), v))).collect())
    }
    pub fn learner_state(&self) -> Json {
        let raw = self.state();
        let mut value = Json::object([("status", raw.get("status").cloned().unwrap_or_else(|| "off".into()))]);
        if matches!(value.get("status").map(Json::text), Some("reading" | "pairing" | "comparing")) {
            value.set("status", "learning".into());
        }
        if value.get("status").map(Json::text) == Some("idle") {
            if let Some(at) = raw.get("learn_at") {
                value.set("at", at.clone());
            }
            for k in ["relearned", "changed", "clusters"] {
                if let Some(v) = raw.get(k) {
                    value.set(k, v.clone());
                }
            }
        } else if let Some(v) = raw.get("message") {
            value.set("message", v.clone());
        }
        value
    }
    pub fn stop(&mut self) {
        if let Some(mut t) = self.task.take() {
            t.stop();
        }
        *self.state.lock().expect("research state mutex poisoned") =
            json!({"status":if configured(){"idle"}else{"off"}}).into();
    }
    pub fn cancel(&mut self) {
        self.stop();
        self.paused = true;
    }
    pub fn start(
        &mut self,
        library: &Library,
        kind: &str,
        args: Value,
        reply: Option<mpsc::Sender<Result<Json>>>,
    ) -> Result<bool> {
        if library.frozen() && !["oc", "relate"].contains(&kind) {
            return Err(Error::Invalid("验收图库是只读的".into()));
        }
        let worker = std::env::var_os("ILLUSTASH_RESEARCH_WORKER")
            .filter(|p| !p.is_empty())
            .ok_or_else(|| Error::Invalid("研究 worker 未配置，功能不可用".into()))?;
        if self.task.as_ref().is_some_and(Task::running) {
            return Ok(false);
        }
        self.task.take();
        self.paused = false;
        self.retry_at = None;
        self.exhausted.store(false, Ordering::SeqCst);
        if ["similar", "regroup"].contains(&kind) {
            let c = crate::sqlite::open(library.db())?;
            crate::configure(&c)?;
            queue_schema(&c)?;
            c.execute(
                "INSERT INTO research_runs(id,kind,status) VALUES (lower(hex(randomblob(16))),?,'pending')",
                [kind],
            )?;
        }
        let library = library.clone();
        let identity = TaskIdentity::new(&library, self.generation, &self.session)?;
        let kind = kind.to_string();
        let state = self.state.clone();
        let anchors = self.anchors.clone();
        let exhausted = self.exhausted.clone();
        let timeout =
            std::env::var("ILLUSTASH_RESEARCH_TIMEOUT_MS").ok().and_then(|s| s.parse().ok()).unwrap_or(1800000);
        let running = json!({"status": match kind.as_str(){"similar"=>"pairing","relate"=>"comparing","oc"=>"grouping",_=>"learning"}});
        *state.lock().expect("research state mutex poisoned") = running.clone().into();
        self.task = Some(Task::start(identity, Some(Duration::from_millis(timeout)), move |ctx| {
            let mut attempt = 0;
            let result = loop {
                // Each attempt takes a fresh snapshot; a stale result was rejected and never published.
                match run(&library, &ctx, &worker, &kind, args.clone(), &state, &anchors) {
                    Err(e) if input_changed(&e) && !["oc", "relate"].contains(&kind.as_str()) => {
                        if attempt == CONFLICT_RETRIES {
                            exhausted.store(true, Ordering::SeqCst);
                            break Err(Error::Conflict(format!(
                                "研究输入持续变化，自动重试 {CONFLICT_RETRIES} 次后仍未完成，请稍后手动重试"
                            )));
                        }
                        attempt += 1;
                        *state.lock().expect("research state mutex poisoned") = json!({
                            "status": "waiting",
                            "message": format!("输入有变化，稍后自动重试（{attempt}/{CONFLICT_RETRIES}）"),
                        })
                        .into();
                        let until = std::time::Instant::now() + retry_base() * 2u32.pow(attempt - 1);
                        let waited = loop {
                            if let Err(e) = ctx.check() {
                                break Err(e);
                            }
                            if std::time::Instant::now() >= until {
                                break Ok(());
                            }
                            std::thread::sleep(Duration::from_millis(50));
                        };
                        if let Err(e) = waited {
                            break Err(e);
                        }
                        *state.lock().expect("research state mutex poisoned") = running.clone().into();
                    }
                    other => break other,
                }
            };
            match result {
                Ok(v) => {
                    let mut public = Json::object([("status", "idle".into())]);
                    if !["oc", "relate"].contains(&kind.as_str()) {
                        public.set("at", chrono::Local::now().format("%H:%M:%S").to_string().into());
                        public.extend(v.clone());
                        public.set("learn_at", chrono::Local::now().format("%H:%M:%S").to_string().into());
                        public.set("status", "idle".into());
                    }
                    *state.lock().expect("research state mutex poisoned") = public.into();
                    if let Some(tx) = reply {
                        let _ = tx.send(Ok(v));
                    }
                    Ok(())
                }
                Err(e) => {
                    if !["oc", "relate"].contains(&kind.as_str()) {
                        if let Ok(c) = crate::sqlite::open(library.db()) {
                            let _ = c.busy_timeout(Duration::from_millis(100));
                            let message = e.to_string().chars().take(500).collect::<String>();
                            let _=c.execute("UPDATE backend_effects SET error=? WHERE status='pending' AND kind IN ('verdict','imported','index_changed')",[&message]);
                            let _ = c.execute("UPDATE research_runs SET error=? WHERE status='pending'", [&message]);
                        }
                    }
                    *state.lock().expect("research state mutex poisoned") =
                        Json::object([("status", "error".into()), ("message", e.to_string().into())]).into();
                    if let Some(tx) = reply {
                        let _ = tx.send(Err(Error::Invalid(e.to_string())));
                    }
                    Err(e)
                }
            }
        }));
        Ok(true)
    }
    pub fn query(&mut self, library: &Library, kind: &str, args: Value) -> Result<mpsc::Receiver<Result<Json>>> {
        let (tx, rx) = mpsc::channel();
        if !self.start(library, kind, args, Some(tx))? {
            return Err(Error::Conflict("研究任务正在运行".into()));
        }
        Ok(rx)
    }
    /// Start pending learning/similarity work. Not while a sync runs: its batch imports leave
    /// `imported` events that one run after the sync coalesces, instead of learning competing
    /// with recognition for the CPU between batches.
    pub fn recover(&mut self, library: &Library, sync_busy: bool) -> Result<()> {
        if !configured()
            || library.frozen()
            || sync_busy
            || self.paused
            || self.task.as_ref().is_some_and(Task::running)
            || self.retry_at.is_some_and(|t| std::time::Instant::now() < t)
        {
            return Ok(());
        }
        if self.task.as_ref().is_some_and(|t| {
            matches!(t.outcome(), crate::app_tasks::TaskOutcome::Failed(_) | crate::app_tasks::TaskOutcome::Cancelled)
        }) {
            self.task.take();
            if self.exhausted.load(Ordering::SeqCst) {
                // Ongoing writes outlasted the retries: keep the error until an explicit start or reopen.
                self.paused = true;
                return Ok(());
            }
            self.retry_at = Some(std::time::Instant::now() + Duration::from_secs(60));
            return Ok(());
        }
        let c = crate::sqlite::open(library.db())?;
        crate::configure(&c)?;
        queue_schema(&c)?;
        // An interrupted run has no receipt: its original events/intent remain pending.
        c.execute("UPDATE research_runs SET status='pending' WHERE status='running'", [])?;
        let pending:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM backend_effects WHERE status='pending' AND kind IN ('verdict','imported','index_changed')) OR EXISTS(SELECT 1 FROM research_runs WHERE status='pending')",[],|r|r.get(0))?;
        if pending {
            self.start(library, "events", json!({"similar":self.similar_enabled}), None)?;
        }
        Ok(())
    }
}
fn run(
    library: &Library,
    ctx: &TaskContext,
    worker: &std::ffi::OsStr,
    kind: &str,
    args: Value,
    state: &Mutex<State>,
    anchors: &Path,
) -> Result<Json> {
    let scratch = Scratch(std::env::temp_dir().join(format!("illustash-research-{}", ctx.identity().task_id)));
    fs::create_dir(&scratch.0)?;
    let c = crate::app_library::read_connection(&library.db())?;
    let src = crate::app_library::read_connection(&library.source())?;
    // Snapshot/version from the same read transactions.
    c.execute_batch("BEGIN")?;
    src.execute_batch("BEGIN")?;
    let version = fingerprint(&c, &src, library, anchors, ctx)?;
    backup(&c, &scratch.0.join("library.sqlite"), ctx)?;
    backup(&src, &scratch.0.join("source.sqlite"), ctx)?;
    let events: Vec<i64> = c
        .prepare(
            "SELECT id FROM backend_effects WHERE status='pending' AND kind IN ('verdict','imported','index_changed') ORDER BY id",
        )?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let intents: Vec<String> =
        if c.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='research_runs')", [], |r| {
            r.get::<_, bool>(0)
        })? {
            c.prepare("SELECT id FROM research_runs WHERE status='pending' AND (?='events' OR kind=?)")?
                .query_map(rusqlite::params![kind, kind], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?
        } else {
            vec![]
        };
    let regroup = kind == "regroup"
        || kind == "events"
            && (c.query_row(
                "SELECT EXISTS(SELECT 1 FROM backend_effects WHERE status='pending' AND kind IN ('imported','index_changed'))",
                [],
                |r| r.get::<_, bool>(0),
            )? || !intents.is_empty());
    crate::sqlite::open(scratch.0.join("library.sqlite"))?
        .execute("DELETE FROM name_settings WHERE key IN ('model_key','deepseek_key')", [])?;
    let learning = crate::character_learning::snapshot(&mut crate::sqlite::open(scratch.0.join("library.sqlite"))?)?
        ["token"]
        .clone();
    c.execute_batch("ROLLBACK")?;
    src.execute_batch("ROLLBACK")?;
    let request = protocol::value(&protocol::ResearchRequest {
        format: protocol::Format::Research,
        identity: ctx.identity(),
        kind,
        args: &args,
        regroup,
        snapshot: &scratch.0,
        features: library.instance_path().join("features"),
        version: &version,
        learning_token: &learning,
        anchors,
        core: std::env::current_exe()?,
    })?;
    let input = scratch.0.join("request.json");
    let output = scratch.0.join("response.json");
    let progress = scratch.0.join("progress.json");
    fs::write(&input, serde_json::to_vec(&request).map_err(|_| bad())?)?;
    // A separate observer is unnecessary: progress is a bounded file polled by the process API callback.
    let mut cmd = Command::new(worker);
    cmd.arg(&input).arg(&output).env("ILLUSTASH_RESEARCH_PROGRESS", &progress);
    let mut last = std::time::Instant::now();
    let status = ctx.process_observed(&mut cmd, || {
        if last.elapsed() < Duration::from_millis(100) {
            return Ok(());
        }
        last = std::time::Instant::now();
        if fs::metadata(&progress).is_ok_and(|m| m.len() < 16384) {
            if let Ok(bytes) = fs::read(&progress) {
                if let Ok(v) = serde_json::from_slice::<Value>(&bytes) {
                    if ["learning", "grouping", "reading", "pairing", "comparing"]
                        .contains(&v["status"].as_str().unwrap_or(""))
                    {
                        *state.lock().expect("research state mutex poisoned") =
                            crate::http_json::parse_python(std::str::from_utf8(&bytes).unwrap_or("{}"))
                                .and_then(|v| State::parse(v, &progress))
                                .unwrap_or_else(|_| State::from(json!({"status":"running"})));
                    }
                }
            }
        }
        Ok(())
    })?;
    ctx.check()?;
    if !status.success() {
        return Err(Error::Invalid("研究 worker 崩溃或依赖不可用".into()));
    }
    if fs::metadata(&output)?.len() > 128 * 1024 * 1024 {
        return Err(bad());
    }
    let bytes = fs::read(&output)?;
    let ordered = crate::http_json::parse_python(std::str::from_utf8(&bytes).map_err(|_| bad())?)?;
    let response: protocol::ResearchResponse = protocol::parse(
        crate::strict_json::parse(
            std::str::from_utf8(&bytes).map_err(|e| Error::Invalid(format!("{}: {e}", output.display())))?,
        )
        .map_err(|e| Error::Invalid(format!("{}: {e}", output.display())))?,
        &output,
    )?;
    if response.format != protocol::Format::Research || response.version != version {
        return Err(bad());
    }
    *state.lock().expect("research state mutex poisoned") = json!({"status":"importing"}).into();
    ctx.receive(&response.identity, || {
        let mut c = crate::sqlite::open(library.db())?;
        crate::configure(&c)?;
        let mut src = crate::app_library::read_connection(&library.source())?;
        let src = src.transaction()?;
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if fingerprint(&tx, &src, library, anchors,ctx)? != version {
            return Err(Error::Conflict(INPUT_CHANGED.into()));
        }
        let result = Value::Object(response.result.clone());
        if kind == "oc" || kind == "relate" {
            validate_query(&tx, kind, &result, &args)?;
            return ordered.get("result").cloned().ok_or_else(bad);
        }
        crate::schema::begin_writing(&tx)?;
        if kind != "similar" {
            if let Some(batch) = response.learning.as_ref().filter(|v| !v.is_null()) {
                if batch["token"] != learning {
                    return Err(bad());
                }
                if crate::character_learning::publish_tx(&tx, batch)?["status"] == "stale" {
                    return Err(Error::Conflict(LEARNING_CHANGED.into()));
                }
            }
        }
        let caches = response.tables.as_ref().ok_or_else(|| Error::Invalid(format!("{}: 字段 tables: 缺少结果表", output.display())))?;
        let expected: Vec<&str> = if kind == "similar" {
            vec!["image_sigs", "relations", "similar_paired", "similar_retry"]
        } else if regroup {
            let mut v = vec!["clusters", "cluster_info"];
            if kind == "events" && args["similar"] == true {
                v.extend(["image_sigs", "relations", "similar_paired", "similar_retry"]);
            }
            v
        } else {
            vec![]
        };
        if caches.keys().map(String::as_str).collect::<std::collections::BTreeSet<_>>()
            != expected.iter().copied().collect()
        {
            return Err(bad());
        }
        tx.execute_batch("CREATE TEMP TABLE research_old_relations AS SELECT * FROM relations;CREATE TEMP TABLE research_old_sigs AS SELECT * FROM image_sigs;")?;
        for table in expected.iter() {
            replace(&tx, table, &caches[*table],ctx)?;
        }
        if expected.contains(&"relations") {
            crate::app_migrate_data::early_groups(&tx, &src)?;
            crate::character_projection::fold(&tx)?;
        }
        validate_references(&tx,&expected)?;
        if kind == "events" {
            for id in events {
                tx.execute("UPDATE backend_effects SET status='done',error='' WHERE id=?", [id])?;
            }
        }
        for id in intents {
            tx.execute("UPDATE research_runs SET status='done',error='' WHERE id=?", [id])?;
        }
        ctx.check()?;
        crate::schema::commit_writing(tx)?;
        ordered.get("result").cloned().ok_or_else(bad)
    })
}
fn replace(c: &Connection, table: &str, value: &Value, ctx: &TaskContext) -> Result<()> {
    let n = CACHES.iter().find(|(t, _)| *t == table).ok_or_else(bad)?.1;
    let rows = value.as_array().ok_or_else(bad)?;
    if rows.len() > 1000000 {
        return Err(bad());
    }
    if ["similar_retry", "similar_paired"].contains(&table) {
        c.execute_batch("CREATE TABLE IF NOT EXISTS similar_paired(uuid TEXT PRIMARY KEY);CREATE TABLE IF NOT EXISTS similar_retry(a TEXT NOT NULL,b TEXT NOT NULL,PRIMARY KEY(a,b));")?;
    }
    c.execute(&format!("DELETE FROM {table}"), [])?;
    let mut q = c.prepare(&format!("INSERT INTO {table} VALUES ({})", vec!["?"; n].join(",")))?;
    for row in rows {
        ctx.check()?;
        let r = row.as_array().filter(|r| r.len() == n).ok_or_else(bad)?;
        validate_row(table, r)?;
        let mut values = vec![];
        for (i, v) in r.iter().enumerate() {
            values.push(if table == "image_sigs" && i == 2 && !v.is_null() {
                let hex = v.as_str().filter(|s| s.len() == 512 && s.is_ascii()).ok_or_else(bad)?;
                Sql::Blob(
                    (0..hex.len())
                        .step_by(2)
                        .map(|j| u8::from_str_radix(&hex[j..j + 2], 16).map_err(|_| bad()))
                        .collect::<Result<Vec<_>>>()?,
                )
            } else {
                match v {
                    Value::Null => Sql::Null,
                    Value::String(s) => Sql::Text(s.clone()),
                    Value::Number(v) => {
                        if let Some(i) = v.as_i64() {
                            Sql::Integer(i)
                        } else {
                            Sql::Real(v.as_f64().filter(|f| f.is_finite()).ok_or_else(bad)?)
                        }
                    }
                    _ => return Err(bad()),
                }
            });
        }
        q.execute(rusqlite::params_from_iter(values))?;
    }
    Ok(())
}
fn validate_row(table: &str, r: &[Value]) -> Result<()> {
    let text = |i: usize| r[i].is_string();
    let int = |i: usize| r[i].as_i64().is_some_and(|v| v >= 0);
    let optional_num = |i: usize| r[i].is_null() || r[i].as_f64().is_some_and(|f| f.is_finite());
    let valid = match table {
        "clusters" => text(0) && int(1),
        "cluster_info" => {
            int(0)
                && int(1)
                && (r[2].is_null() || int(2))
                && optional_num(3)
                && text(4)
                && (r[5].is_null() || text(5))
                && text(6)
        }
        "image_sigs" => {
            text(0) && optional_num(1) && (r[2].is_null() || text(2)) && r[3].as_i64().is_some_and(|i| i == 0 || i == 1)
        }
        "relations" => text(0) && text(1) && text(2) && optional_num(3) && text(4) && text(5),
        "similar_paired" => text(0),
        "similar_retry" => text(0) && text(1),
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err(bad())
    }
}
fn validate_references(c: &Connection, expected: &[&str]) -> Result<()> {
    for (index,sql) in [
        "SELECT EXISTS(SELECT 1 FROM clusters WHERE uuid NOT IN (SELECT uuid FROM assets) OR cid NOT IN (SELECT cid FROM cluster_info))",
        "SELECT EXISTS(SELECT 1 FROM cluster_info WHERE size<2 OR (near IS NOT NULL AND near NOT IN (SELECT cid FROM cluster_info)) OR (near_kind IS NOT NULL AND near_kind NOT IN ('look','trait')) OR NOT json_valid(traits))",
        "SELECT EXISTS(SELECT 1 FROM relations r WHERE a>=b OR ((a NOT IN (SELECT uuid FROM assets) OR b NOT IN (SELECT uuid FROM assets)) AND NOT EXISTS(SELECT 1 FROM research_old_relations o WHERE o.a=r.a AND o.b=r.b AND o.kind=r.kind AND o.changed IS r.changed AND o.note=r.note AND o.checked_at=r.checked_at)) OR kind NOT IN ('identical','duplicate','subtle','crop','variant','edit','similar','unrelated') OR changed<0 OR changed>1)",
        "SELECT EXISTS(SELECT 1 FROM image_sigs r WHERE (uuid NOT IN (SELECT uuid FROM assets) AND NOT EXISTS(SELECT 1 FROM research_old_sigs o WHERE o.uuid=r.uuid AND o.ratio IS r.ratio AND o.grid IS r.grid AND o.ok=r.ok)) OR ok NOT IN (0,1) OR (ok=1 AND (length(grid)!=256 OR ratio<=0)))",
        "SELECT EXISTS(SELECT 1 FROM proto_claims WHERE uuid NOT IN (SELECT uuid FROM assets) OR entity NOT IN (SELECT id FROM entities) OR score<0 OR score>1)",
        "SELECT EXISTS(SELECT 1 FROM weak_claims WHERE uuid NOT IN (SELECT uuid FROM assets) OR entity NOT IN (SELECT id FROM entities) OR source NOT IN ('PROTO','CCIP') OR score<0 OR score>1)",
        "SELECT EXISTS(SELECT 1 FROM prototypes WHERE entity NOT IN (SELECT id FROM entities) OR threshold<0 OR threshold>1 OR hint<threshold OR hint>1)",
    ].into_iter().enumerate() {
        if index<2 && !expected.contains(&"clusters") || index==2 && !expected.contains(&"relations") || index==3 && !expected.contains(&"image_sigs") {continue;}
        if c.query_row(sql,[],|r|r.get::<_,bool>(0))? {return Err(bad());}
    }
    Ok(())
}
fn validate_query(c: &Connection, kind: &str, v: &Value, args: &Value) -> Result<()> {
    if !v.is_object() {
        return Err(bad());
    }
    if kind == "oc" {
        for group in v["groups"].as_array().ok_or_else(bad)? {
            for id in group.as_array().ok_or_else(bad)? {
                let id = id.as_str().ok_or_else(bad)?;
                if !c.query_row(
                    "SELECT EXISTS(SELECT 1 FROM memberships WHERE uuid=? AND entity=?)",
                    rusqlite::params![id, args["entity"].as_str().ok_or_else(bad)?],
                    |r| r.get::<_, bool>(0),
                )? {
                    return Err(bad());
                }
            }
        }
    } else {
        let ids = args["ids"].as_array().ok_or_else(bad)?;
        for p in v["pairs"].as_array().ok_or_else(bad)? {
            if !ids.contains(&p["a"]) || !ids.contains(&p["b"]) {
                return Err(bad());
            }
        }
    }
    Ok(())
}
