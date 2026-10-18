//! 识别结果导入与图库归档导入的后台任务（#88）。同一时间只运行一个导入；状态可轮询，可取消。
//! 发布点经 `TaskContext::receive` 与取消互斥：取消先到就不发布；发布已经开始，取消等它完成，结果如实记为完成。
use crate::{
    app_tasks::{Deadline, Task, TaskContext, TaskIdentity},
    http_json::Json,
    task_state::{State, Status},
    Error, Result,
};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, MutexGuard,
    },
    time::Duration,
};

/// What a long import reports while it runs, and the one point at which it publishes.
pub(crate) trait Observer {
    /// The step now running; any count of the previous step is cleared.
    fn stage(&self, _stage: &'static str) {}
    /// Real units of the current step (bytes unpacked, assets written, files read).
    fn count(&self, _done: u64, _total: u64) {}
    /// Publish once. Refused after a cancellation; a cancellation arriving meanwhile waits until it is done.
    fn publish(&self, apply: &mut dyn FnMut() -> Result<()>) -> Result<()> {
        apply()
    }
}
/// Callers with their own cancellation and no one to report to (CLI, recognition worker batches).
pub(crate) struct Silent;
impl Observer for Silent {}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Kind {
    Archive,
    Recognition,
}
impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Archive => "archive",
            Kind::Recognition => "recognition",
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Handed to the running import: its cancellation flag and its reports into the polled state.
pub(crate) struct Progress {
    state: Arc<Mutex<State>>,
    context: TaskContext,
    published: AtomicBool,
}
impl Progress {
    pub(crate) fn cancellation(&self) -> &AtomicBool {
        self.context.cancellation()
    }
    pub(crate) fn cancellation_arc(&self) -> Arc<AtomicBool> {
        self.context.cancellation_arc()
    }
}
impl Observer for Progress {
    fn stage(&self, stage: &'static str) {
        let mut state = lock(&self.state);
        state.remove("done");
        state.remove("total");
        state.set("stage", stage.into());
    }
    fn count(&self, done: u64, total: u64) {
        let mut state = lock(&self.state);
        state.set("done", serde_json::Value::from(done).into());
        state.set("total", serde_json::Value::from(total).into());
    }
    fn publish(&self, apply: &mut dyn FnMut() -> Result<()>) -> Result<()> {
        self.context.receive(self.context.identity(), apply)?;
        self.published.store(true, Ordering::SeqCst);
        Ok(())
    }
}

/// Reasons a running import was stopped, as the state reports them.
pub(crate) const CANCELLED: &str = "导入已取消";
pub(crate) const SWITCHED: &str = "图库已切换，导入已取消";
pub(crate) const QUITTING: &str = "程序正在退出，导入已取消";
pub(crate) const DAMAGED: &str = "图库数据库损坏，导入已取消";
/// How long quitting waits for a stopped import to remove what it unpacked.
pub(crate) const QUIT_WAIT: Duration = Duration::from_secs(5);

/// The HTTP status and message the former synchronous request answered with for this error.
fn classify(error: Error) -> (u16, String) {
    match error {
        Error::Invalid(message) => (400, message),
        Error::Network(message) => (502, message),
        Error::Conflict(message) => (409, message),
        other => {
            eprintln!("导入失败：{other}");
            (500, "保存失败，未提交的操作已回滚；请查看启动终端".into())
        }
    }
}

struct Current {
    task: Task,
    state: Arc<Mutex<State>>,
    reason: Arc<Mutex<Option<&'static str>>>,
    kind: Kind,
    generation: u64,
}
impl Current {
    fn snapshot(&self) -> Json {
        let state = lock(&self.state).clone();
        // A thread that ended without a final state panicked; never leave it reported as running.
        if matches!(state.status(), Status::Running | Status::Stopping) && !self.task.running() {
            let mut state = state;
            state.set("status", "error".into());
            state.set("error", "导入任务异常结束".into());
            state.set("code", serde_json::Value::from(500).into());
            return state.snapshot();
        }
        state.snapshot()
    }
    /// Records why and marks the import stopping. The caller cancels the returned context after releasing the
    /// slot: a cancellation may wait for a publication in progress, and polling must not wait with it.
    fn stop(&self, reason: &'static str) -> Option<TaskContext> {
        if !self.task.running() {
            return None;
        }
        lock(&self.reason).get_or_insert(reason);
        let mut state = lock(&self.state);
        if state.status() == Status::Running {
            state.set("status", "stopping".into());
        }
        Some(self.task.context())
    }
}

/// The application's import slot. Its state outlives a switch, so the page that reloads afterwards can still read
/// why the previous import stopped; only one import runs at a time, a stopping one included.
#[derive(Default)]
pub(crate) struct Imports {
    current: Mutex<Option<Current>>,
    closed: AtomicBool,
}
impl Imports {
    pub(crate) fn state(&self) -> Json {
        lock(&self.current).as_ref().map(Current::snapshot).unwrap_or_else(|| Json::object([("status", "idle".into())]))
    }
    /// Starts `work` in the background and answers with its first state. `file` names the input for the page.
    pub(crate) fn start(
        &self,
        identity: TaskIdentity,
        kind: Kind,
        file: &str,
        work: impl FnOnce(&Progress) -> Result<Json> + Send + 'static,
    ) -> Result<Json> {
        let mut current = lock(&self.current);
        if self.closed.load(Ordering::SeqCst) {
            return Err(Error::Invalid("服务正在退出".into()));
        }
        if current.as_ref().is_some_and(|c| c.task.running()) {
            return Err(Error::Invalid("已有导入正在进行，请等它结束或取消后再试".into()));
        }
        let generation = identity.generation;
        let state = Arc::new(Mutex::new(State::from(Json::object([
            ("status", "running".into()),
            ("kind", kind.name().into()),
            ("id", identity.task_id.as_str().into()),
            ("file", file.into()),
        ]))));
        // The answer is the task as started: one that fails at once must still reach the page as running, or the
        // page, which reports an import when a poll sees it end, never says why it failed.
        let answer = lock(&state).snapshot();
        let reason: Arc<Mutex<Option<&'static str>>> = Arc::default();
        let (shared, why) = (state.clone(), reason.clone());
        let task = Task::start_with(identity, Deadline::default(), move |context| {
            let progress =
                Progress { state: shared.clone(), context: context.clone(), published: AtomicBool::new(false) };
            let result = work(&progress);
            let stopped = context.cancellation().load(Ordering::SeqCst) || crate::platform_process::stopping();
            let mut state = lock(&shared);
            match result {
                Ok(value) => {
                    for key in ["stage", "done", "total"] {
                        state.remove(key);
                    }
                    state.set("status", "done".into());
                    state.set("result", value);
                }
                // Nothing was published: the import stopped because it was asked to, whatever error that surfaced as.
                Err(_) if stopped && !progress.published.load(Ordering::SeqCst) => {
                    state.set("status", "cancelled".into());
                    state.set("reason", lock(&why).unwrap_or(QUITTING).into());
                }
                Err(error) => {
                    let (code, message) = classify(error);
                    state.set("status", "error".into());
                    state.set("error", message.into());
                    state.set("code", serde_json::Value::from(code).into());
                }
            }
            Ok(())
        });
        *current = Some(Current { task, state, reason, kind, generation });
        Ok(answer)
    }
    /// A recognition import of this session is admitted or running: sync and index writes wait for it.
    pub(crate) fn recognition_running(&self, generation: u64) -> bool {
        lock(&self.current)
            .as_ref()
            .is_some_and(|c| c.kind == Kind::Recognition && c.generation == generation && c.task.running())
    }
    /// Asks the running import to stop and answers with the state at that moment; the page polls for the end.
    pub(crate) fn cancel(&self, reason: &'static str) -> Json {
        let (stopping, answer) = {
            let current = lock(&self.current);
            match current.as_ref() {
                Some(c) => (c.stop(reason), c.snapshot()),
                None => (None, Json::object([("status", "idle".into())])),
            }
        };
        if let Some(context) = stopping {
            context.cancel();
        }
        answer
    }
    /// Quitting: refuse new imports, stop the running one and give it `wait` to remove what it unpacked.
    pub(crate) fn shutdown(&self, wait: Duration) {
        self.closed.store(true, Ordering::SeqCst);
        let taken = {
            let mut current = lock(&self.current);
            current.take()
        };
        if let Some(mut c) = taken {
            if let Some(context) = c.stop(QUITTING) {
                context.cancel();
            }
            if !c.task.wait(wait) {
                eprintln!("导入未在 {} 秒内停下，退出时不再等待", wait.as_secs());
                c.task.abandon();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::mpsc, time::Instant};

    fn identity(generation: u64) -> TaskIdentity {
        TaskIdentity::at(&std::env::temp_dir(), "synthetic", generation, "session").unwrap()
    }
    fn status_of(imports: &Imports) -> String {
        imports.state().get("status").map(|v| v.text().to_owned()).unwrap_or_default()
    }
    fn settle(imports: &Imports) -> Json {
        let until = Instant::now() + Duration::from_secs(5);
        while matches!(status_of(imports).as_str(), "running" | "stopping") {
            assert!(Instant::now() < until, "import did not end");
            std::thread::sleep(Duration::from_millis(1));
        }
        imports.state()
    }

    #[test]
    fn stages_counts_and_result_are_polled_and_one_import_runs_at_a_time() {
        let imports = Imports::default();
        let (step, steps) = mpsc::channel::<()>();
        let (seen, saw) = mpsc::channel::<()>();
        let started = imports
            .start(identity(1), Kind::Archive, "synthetic.zip", move |p| {
                p.stage("unpacking");
                p.count(1, 4);
                seen.send(()).unwrap();
                steps.recv().unwrap();
                p.stage("checking");
                seen.send(()).unwrap();
                steps.recv().unwrap();
                p.publish(&mut || Ok(()))?;
                Ok(Json::object([("name", "合成".into())]))
            })
            .unwrap();
        assert_eq!(started.get("status").unwrap().text(), "running");
        assert_eq!(started.get("file").unwrap().text(), "synthetic.zip");
        saw.recv().unwrap();
        let running = imports.state();
        assert_eq!(
            running.dump(),
            Json::object([
                ("status", "running".into()),
                ("kind", "archive".into()),
                ("id", started.get("id").unwrap().clone()),
                ("file", "synthetic.zip".into()),
                ("stage", "unpacking".into()),
                ("done", serde_json::Value::from(1).into()),
                ("total", serde_json::Value::from(4).into()),
            ])
            .dump()
        );
        let refused =
            imports.start(identity(1), Kind::Recognition, "other", |_| Ok(Json::Object(Vec::new()))).unwrap_err();
        assert!(refused.to_string().contains("已有导入正在进行"), "{refused}");
        assert!(!imports.recognition_running(1));
        step.send(()).unwrap();
        saw.recv().unwrap();
        let checking = imports.state();
        assert_eq!(checking.get("stage").unwrap().text(), "checking");
        assert!(checking.get("done").is_none(), "a new stage clears the previous count");
        step.send(()).unwrap();
        let done = settle(&imports);
        assert_eq!(done.get("status").unwrap().text(), "done");
        assert!(done.get("stage").is_none());
        assert_eq!(done.get("result").unwrap().get("name").unwrap().text(), "合成");
        // A finished import makes room for the next one.
        imports.start(identity(1), Kind::Recognition, "next", |_| Ok(Json::Object(Vec::new()))).unwrap();
        settle(&imports);
    }

    #[test]
    fn cancellation_before_publication_publishes_nothing_and_says_why() {
        for (reason, expected) in [(CANCELLED, CANCELLED), (SWITCHED, SWITCHED)] {
            let imports = Imports::default();
            let published = Arc::new(AtomicBool::new(false));
            let (ready, wait) = mpsc::channel();
            let flag = published.clone();
            imports
                .start(identity(1), Kind::Recognition, "result", move |p| {
                    p.stage("models");
                    ready.send(()).unwrap();
                    while !p.cancellation().load(Ordering::SeqCst) {
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    p.publish(&mut || {
                        flag.store(true, Ordering::SeqCst);
                        Ok(())
                    })?;
                    Ok(Json::Object(Vec::new()))
                })
                .unwrap();
            wait.recv().unwrap();
            assert!(imports.recognition_running(1));
            assert!(!imports.recognition_running(2), "another session's import does not hold this one back");
            let answer = imports.cancel(reason);
            assert!(matches!(answer.get("status").unwrap().text(), "stopping" | "cancelled"));
            let end = settle(&imports);
            assert_eq!(end.get("status").unwrap().text(), "cancelled");
            assert_eq!(end.get("reason").unwrap().text(), expected);
            assert_eq!(end.get("stage").unwrap().text(), "models", "the state keeps where it stopped");
            assert!(!published.load(Ordering::SeqCst));
        }
    }

    #[test]
    fn a_cancellation_during_publication_waits_and_a_published_import_is_never_reported_cancelled() {
        // After publication the import either finishes (done) or fails in a later step (error), never "cancelled".
        for (after, status) in [(Ok(()), "done"), (Err("登记失败（合成）"), "error")] {
            let imports = Arc::new(Imports::default());
            let (inside, entered) = mpsc::channel();
            let (release, released) = mpsc::channel::<()>();
            imports
                .start(identity(1), Kind::Archive, "synthetic.zip", move |p| {
                    p.publish(&mut || {
                        inside.send(()).unwrap();
                        released.recv().unwrap();
                        Ok(())
                    })?;
                    after.map_err(|m| Error::Invalid(m.into()))?;
                    Ok(Json::object([("name", "合成".into())]))
                })
                .unwrap();
            entered.recv().unwrap();
            let cancelling = {
                let imports = imports.clone();
                std::thread::spawn(move || imports.cancel(CANCELLED))
            };
            // Polling is not held up by the waiting cancellation: it reads "stopping" while the cancel waits.
            let until = Instant::now() + Duration::from_secs(5);
            while status_of(&imports) != "stopping" {
                assert!(Instant::now() < until, "the cancellation was never recorded");
                std::thread::yield_now();
            }
            assert!(!cancelling.is_finished(), "cancel must wait for the publication in progress");
            release.send(()).unwrap();
            cancelling.join().unwrap();
            let end = settle(&imports);
            assert_eq!(end.get("status").unwrap().text(), status, "{}", end.dump());
        }
    }

    #[test]
    fn errors_keep_the_former_status_codes_and_a_panic_is_not_left_running() {
        for (error, code) in [
            (Error::Invalid("合成".into()), "400"),
            (Error::Conflict("合成".into()), "409"),
            (Error::Network("合成".into()), "502"),
            (Error::Io(std::io::Error::other("合成")), "500"),
        ] {
            let imports = Imports::default();
            imports.start(identity(1), Kind::Archive, "a.zip", move |_| Err(error)).unwrap();
            let end = settle(&imports);
            assert_eq!(end.get("status").unwrap().text(), "error");
            assert_eq!(end.get("code").unwrap().dump(), code);
        }
        let imports = Imports::default();
        imports.start(identity(1), Kind::Archive, "a.zip", |_| panic!("synthetic import crash")).unwrap();
        let end = settle(&imports);
        assert_eq!(end.get("error").unwrap().text(), "导入任务异常结束");
    }

    #[test]
    fn quitting_refuses_new_imports_and_does_not_wait_forever() {
        let imports = Imports::default();
        let (ready, wait) = mpsc::channel();
        // Blocked work that never checks its cancellation (a pipe nobody writes).
        let (_hold, held) = mpsc::channel::<()>();
        imports
            .start(identity(1), Kind::Recognition, "result", move |_| {
                ready.send(()).unwrap();
                let _ = held.recv_timeout(Duration::from_secs(30));
                Ok(Json::Object(Vec::new()))
            })
            .unwrap();
        wait.recv().unwrap();
        let started = Instant::now();
        imports.shutdown(Duration::from_millis(100));
        assert!(started.elapsed() < Duration::from_secs(5));
        assert_eq!(status_of(&imports), "idle");
        let refused = imports.start(identity(1), Kind::Archive, "a.zip", |_| Ok(Json::Object(Vec::new()))).unwrap_err();
        assert!(refused.to_string().contains("服务正在退出"), "{refused}");
    }
}
