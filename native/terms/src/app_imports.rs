//! 识别结果导入与图库归档导入的后台任务（#88）。同一时间只运行一个导入；状态可轮询，可取消。
//! 发布点经 `TaskContext::receive` 与取消互斥：取消先到就不发布；发布已经开始，取消等它完成，结果如实记为完成。
use crate::{
    app_tasks::{lock_ignoring_poison, Deadline, Task, TaskContext, TaskIdentity},
    http_json::Json,
    task_state::{State, Status},
    Error, Result,
};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

/// 长时间的导入在运行中报告什么，以及它发布的那一个时间点。
pub(crate) trait Observer {
    /// 当前正在运行的步骤；上一步的计数清空。
    fn stage(&self, _stage: &'static str) {}
    /// 当前步骤的实际单位（解出的字节、写入的 asset、读过的文件）。
    fn count(&self, _done: u64, _total: u64) {}
    /// 只发布一次。取消之后拒绝发布；发布期间到达的取消等它完成。
    fn publish(&self, apply: &mut dyn FnMut() -> Result<()>) -> Result<()> {
        apply()
    }
}
/// 自己处理取消、也没有报告对象的调用方（CLI、识别 worker 的批次）。
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

/// 交给正在运行的导入：它的取消标志，以及写进可轮询状态的报告。
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
        let mut state = lock_ignoring_poison(&self.state);
        state.remove("done");
        state.remove("total");
        state.set("stage", stage.into());
    }
    fn count(&self, done: u64, total: u64) {
        let mut state = lock_ignoring_poison(&self.state);
        state.set("done", serde_json::Value::from(done).into());
        state.set("total", serde_json::Value::from(total).into());
    }
    fn publish(&self, apply: &mut dyn FnMut() -> Result<()>) -> Result<()> {
        self.context.receive(self.context.identity(), apply)?;
        self.published.store(true, Ordering::SeqCst);
        Ok(())
    }
}

/// 正在运行的导入被停下的原因，与状态里报告的一致。
pub(crate) const CANCELLED: &str = "导入已取消";
pub(crate) const SWITCHED: &str = "图库已切换，导入已取消";
pub(crate) const QUITTING: &str = "程序正在退出，导入已取消";
pub(crate) const DAMAGED: &str = "图库数据库损坏，导入已取消";
/// 退出时等待已停下的导入删掉它解出内容的时长。
pub(crate) const QUIT_WAIT: Duration = Duration::from_secs(5);

/// 以前的同步请求对这个错误回答的 HTTP 状态和消息。
fn classify(error: Error) -> (u16, String) {
    match error {
        Error::Invalid(message) => (400, message),
        Error::Network(message) => (502, message),
        Error::Conflict(message) => (409, message),
        other => {
            eprintln!("导入失败：{other}");
            (500, "导入失败，请核对提交状态；可在设置的关于页复制诊断信息".into())
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
        let state = lock_ignoring_poison(&self.state).clone();
        // 没有最终状态就结束的线程是 panic 了；不要让它一直报告为运行中。
        if matches!(state.status(), Status::Running | Status::Stopping) && !self.task.running() {
            let mut state = state;
            state.set("status", "error".into());
            state.set("error", "导入任务异常结束".into());
            state.set("code", serde_json::Value::from(500).into());
            return state.snapshot();
        }
        state.snapshot()
    }
    /// 记下原因，并把导入标为正在停止。调用方在释放槽位之后再取消返回的上下文：
    /// 取消可能要等正在进行的发布，轮询不能跟着等。
    fn stop(&self, reason: &'static str) -> Option<TaskContext> {
        if !self.task.running() {
            return None;
        }
        lock_ignoring_poison(&self.reason).get_or_insert(reason);
        let mut state = lock_ignoring_poison(&self.state);
        if state.status() == Status::Running {
            state.set("status", "stopping".into());
        }
        Some(self.task.context())
    }
}

/// 应用的导入槽位。它的状态在切换图库后仍然保留，这样之后重新加载的页面仍能读到上一次导入为什么停下；
/// 同一时间只运行一个导入，正在停止的也算。
#[derive(Default)]
pub(crate) struct Imports {
    current: Mutex<Option<Current>>,
    closed: AtomicBool,
}
impl Imports {
    pub(crate) fn state(&self) -> Json {
        lock_ignoring_poison(&self.current)
            .as_ref()
            .map(Current::snapshot)
            .unwrap_or_else(|| Json::object([("status", "idle".into())]))
    }
    /// 在后台启动 `work`，回答它的初始状态。`file` 是给页面看的输入名称。
    pub(crate) fn start(
        &self,
        identity: TaskIdentity,
        kind: Kind,
        file: &str,
        work: impl FnOnce(&Progress) -> Result<Json> + Send + 'static,
    ) -> Result<Json> {
        let mut current = lock_ignoring_poison(&self.current);
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
        // 回答的是刚启动时的任务：立即失败的任务也必须以运行中的状态送到页面，否则页面（它在轮询看到导入结束时才报告）
        // 永远不会说出失败的原因。
        let answer = lock_ignoring_poison(&state).snapshot();
        let reason: Arc<Mutex<Option<&'static str>>> = Arc::default();
        let (shared, why) = (state.clone(), reason.clone());
        let task = Task::start_with(identity, Deadline::default(), move |context| {
            let progress =
                Progress { state: shared.clone(), context: context.clone(), published: AtomicBool::new(false) };
            let result = work(&progress);
            let stopped = context.cancellation().load(Ordering::SeqCst) || crate::platform_process::stopping();
            let mut state = lock_ignoring_poison(&shared);
            match result {
                Ok(value) => {
                    // 发布成功、投影/登记尚未完成仍是需要反馈的故障；保留业务成功结果，不能诱导重复导入。
                    if value.get("status").map(Json::text) == Some("pending")
                        || value.get("listed").is_some_and(|v| v.dump() == "false")
                    {
                        use crate::diagnostics::{self, Operation, Publication};
                        let reason = Error::Invalid(value.get("error").map(Json::text).unwrap_or("").to_owned());
                        let d = diagnostics::failure(
                            if kind == Kind::Recognition { Operation::Recognition } else { Operation::Archive },
                            diagnostics::stage(state.get("stage").map(Json::text).unwrap_or("")),
                            &reason,
                            Publication::Committed,
                        );
                        state.set("diagnostic", d.value().into());
                    }
                    for key in ["stage", "done", "total"] {
                        state.remove(key);
                    }
                    state.set("status", "done".into());
                    state.set("result", value);
                }
                // 什么都没有发布：导入是因为被要求才停下的，无论它表现为什么错误。
                Err(_) if stopped && !progress.published.load(Ordering::SeqCst) => {
                    state.set("status", "cancelled".into());
                    state.set("reason", lock_ignoring_poison(&why).unwrap_or(QUITTING).into());
                }
                Err(error) => {
                    use crate::diagnostics::{self, Operation, Publication};
                    let d = diagnostics::failure(
                        if kind == Kind::Recognition { Operation::Recognition } else { Operation::Archive },
                        diagnostics::stage(state.get("stage").map(Json::text).unwrap_or("")),
                        &error,
                        if progress.published.load(Ordering::SeqCst) {
                            Publication::Committed
                        } else {
                            Publication::NotCommitted
                        },
                    );
                    state.set("diagnostic", d.value().into());
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
    /// 这个会话的识别导入已被接受或正在运行：同步和索引写入要等它。
    pub(crate) fn recognition_running(&self, generation: u64) -> bool {
        lock_ignoring_poison(&self.current)
            .as_ref()
            .is_some_and(|c| c.kind == Kind::Recognition && c.generation == generation && c.task.running())
    }
    /// 请正在运行的导入停下，回答那一刻的状态；页面轮询直到结束。
    pub(crate) fn cancel(&self, reason: &'static str) -> Json {
        let (stopping, answer) = {
            let current = lock_ignoring_poison(&self.current);
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
    /// 退出时：拒绝新的导入，停下正在运行的导入，给它 `wait` 的时间删掉它解出的内容。
    pub(crate) fn shutdown(&self, wait: Duration) {
        self.closed.store(true, Ordering::SeqCst);
        let taken = {
            let mut current = lock_ignoring_poison(&self.current);
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
        // 导入完成后就能开始下一个。
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
        // 发布之后，导入要么完成（done），要么在之后的步骤失败（error），从不是 "cancelled"。
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
            // 轮询不会被等待中的取消挡住：取消等待期间它读到 "stopping"。
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
            if status == "error" {
                let d = end.get("diagnostic").unwrap();
                assert_eq!(d.get("publication").unwrap().text(), "committed");
                assert_eq!(d.get("retry").unwrap().text(), "check_state");
            }
        }
    }

    #[test]
    fn published_pending_result_keeps_success_and_reports_committed_diagnostic() {
        let imports = Imports::default();
        imports
            .start(identity(1), Kind::Recognition, "synthetic", |p| {
                p.publish(&mut || Ok(()))?;
                p.stage("projecting");
                Ok(Json::object([("status", "pending".into()), ("error", "synthetic projection failure".into())]))
            })
            .unwrap();
        let end = settle(&imports);
        assert_eq!(end.get("status").unwrap().text(), "done");
        assert_eq!(end.get("result").unwrap().get("status").unwrap().text(), "pending");
        let d = end.get("diagnostic").unwrap();
        assert_eq!(d.get("publication").unwrap().text(), "committed");
        assert_eq!(d.get("retry").unwrap().text(), "check_state");
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
            let d = end.get("diagnostic").unwrap();
            assert_eq!(d.get("publication").unwrap().text(), "not_committed");
            assert_eq!(d.get("stored").unwrap().dump(), "false", "unit tests never write user diagnostics");
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
        // 被阻塞、从不检查取消的工作（一个没人写的管道）。
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
