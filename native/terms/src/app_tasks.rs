//! 共用的协作式任务生命周期。业务上的持久化和重试仍由使用方负责。
use crate::{app_library::Library, Error, Result};
use std::{
    path::{Path, PathBuf},
    process::{Command, ExitStatus},
    sync::{
        atomic::{AtomicBool, AtomicU8, Ordering},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

/// 计算 worker 的四个种类。安装包只带一个 `illustash-worker`，四个
/// `ILLUSTASH_<KIND>_WORKER` 都指向它（#37）。
pub const WORKER_KINDS: [&str; 4] = ["research", "index", "provenance", "recognition"];
/// 宿主每次启动 worker 都用这个环境变量显式告诉它本次的种类；请求/响应两参数协议不变，
/// 旧布局里按文件名分派的适配器和测试替身 worker 不读这个变量。
pub const WORKER_KIND_ENV: &str = "ILLUSTASH_WORKER_KIND";

/// worker 本次的种类。旧安装布局的 `illustash-<kind>` 文件名仍按文件名分派（兼容旧安装和
/// 显式覆盖）；其他文件名（安装包里的 `illustash-worker`）必须由 [`WORKER_KIND_ENV`] 给出。
/// 两者都给出而不一致时拒绝，不猜。
pub fn worker_kind<'a>(stem: &'a str, requested: Option<&'a str>) -> std::result::Result<&'a str, String> {
    let named = stem.strip_prefix("illustash-").filter(|kind| WORKER_KINDS.contains(kind));
    match (named, requested.filter(|kind| !kind.is_empty())) {
        (Some(named), Some(requested)) if named != requested => {
            Err(format!("worker 文件名对应 {named}，但 {WORKER_KIND_ENV}={requested}"))
        }
        (Some(named), _) => Ok(named),
        (None, Some(requested)) if WORKER_KINDS.contains(&requested) => Ok(requested),
        (None, Some(requested)) => Err(format!("未知的计算种类 {WORKER_KIND_ENV}={requested}")),
        (None, None) => {
            Err(format!("请由 Illustash 启动，或设置 {WORKER_KIND_ENV}=research / index / provenance / recognition"))
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TaskIdentity {
    pub package: PathBuf,
    pub library_id: String,
    pub generation: u64,
    session: String,
    pub task_id: String,
}
impl TaskIdentity {
    pub fn new(library: &Library, generation: u64, session: &str) -> Result<Self> {
        Self::at(library.instance_path(), library.id(), generation, session)
    }
    pub(crate) fn at(package: &Path, library_id: &str, generation: u64, session: &str) -> Result<Self> {
        let mut nonce = [0u8; 16];
        getrandom::fill(&mut nonce).map_err(|e| Error::Invalid(e.to_string()))?;
        Ok(Self {
            package: crate::app_paths::canonical(package)?,
            library_id: library_id.into(),
            generation,
            session: crate::strict_json::sha256_hex(session.as_bytes()),
            task_id: nonce.iter().map(|b| format!("{b:02x}")).collect(),
        })
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TaskOutcome {
    Running,
    Done,
    Cancelled,
    Failed(String),
}
struct Boundary {
    received: bool,
    closed: bool,
}
/// 任务什么时候超时。`limit` 是从开始算起的固定总时长；`idle` 是一个时间窗，
/// 任务每报告一次进展就重新计时，所以稳定推进的工作永远不会被切断。
#[derive(Clone, Copy, Debug, Default)]
pub struct Deadline {
    pub idle: Option<Duration>,
    pub limit: Option<Duration>,
}
impl Deadline {
    pub fn fixed(limit: Option<Duration>) -> Self {
        Self { idle: None, limit }
    }
}
const RUNNING: u8 = 0;
const IDLE: u8 = 1;
const LIMIT: u8 = 2;
/// 截止时间从哪里读时间：单调时钟，或者在单元测试里用手动拨动的时钟，这样检查截止规则时
/// 不依赖负载高的机器跑得多快（#219）。
#[derive(Clone, Default)]
struct Time(Option<Arc<Mutex<Instant>>>);
impl Time {
    fn now(&self) -> Instant {
        match &self.0 {
            None => Instant::now(),
            Some(manual) => *manual.lock().unwrap_or_else(|e| e.into_inner()),
        }
    }
}
struct Clock {
    deadline: Deadline,
    time: Time,
    started: Instant,
    progressed: Mutex<Instant>,
    /// 任务自己停下的原因（IDLE/LIMIT），记下来以区分取消和超时。
    expired: AtomicU8,
}
impl Clock {
    fn reason(&self) -> u8 {
        let now = self.time.now();
        let reason = if self.deadline.limit.is_some_and(|d| now >= self.started + d) {
            LIMIT
        } else if self
            .deadline
            .idle
            .is_some_and(|d| now >= *self.progressed.lock().unwrap_or_else(|e| e.into_inner()) + d)
        {
            IDLE
        } else {
            RUNNING
        };
        // 第一次超时为准；之后的检查一直报告它。
        match self.expired.compare_exchange(RUNNING, reason, Ordering::SeqCst, Ordering::SeqCst) {
            Ok(_) => reason,
            Err(earlier) => earlier,
        }
    }
}
fn human(d: Duration) -> String {
    let ms = d.as_millis();
    if ms < 1000 {
        format!("{n} 毫秒", n = ms)
    } else if ms < 120_000 {
        format!("{n} 秒", n = ms / 1000)
    } else if ms < 7_200_000 {
        format!("{n} 分钟", n = ms / 60_000)
    } else {
        format!("{n} 小时", n = ms / 3_600_000)
    }
}
#[derive(Clone)]
pub struct TaskContext {
    identity: TaskIdentity,
    cancel: Arc<AtomicBool>,
    clock: Arc<Clock>,
    boundary: Arc<Mutex<Boundary>>,
}
impl TaskContext {
    pub fn identity(&self) -> &TaskIdentity {
        &self.identity
    }
    pub fn cancellation_arc(&self) -> Arc<AtomicBool> {
        self.cancel.clone()
    }
    pub fn cancellation(&self) -> &AtomicBool {
        &self.cancel
    }
    /// 任务自己停下的原因；先到的取消绝不会被改标为超时。
    fn expiry(&self) -> u8 {
        if self.cancel.load(Ordering::SeqCst) {
            self.clock.expired.load(Ordering::SeqCst)
        } else {
            self.clock.reason()
        }
    }
    pub fn check(&self) -> Result<()> {
        match self.stop_reason() {
            Some(reason) => Err(Error::Invalid(reason)),
            None => Ok(()),
        }
    }
    /// 任务停下的原因，用给用户看的文字：已取消，或哪一个截止时间到了。
    pub fn stop_reason(&self) -> Option<String> {
        let reason = self.expiry();
        if reason != RUNNING {
            self.cancel.store(true, Ordering::SeqCst);
        }
        if !self.cancel.load(Ordering::SeqCst) {
            return None;
        }
        let deadline = self.clock.deadline;
        Some(match (reason, deadline.idle, deadline.limit) {
            (IDLE, Some(idle), _) => format!("任务超时：{duration}没有进展", duration = human(idle)),
            (LIMIT, _, Some(limit)) => format!("任务超时：超过 {duration}上限", duration = human(limit)),
            _ => "任务已取消".into(),
        })
    }
    /// 任务停下是因为时间用完了，而不是被取消。
    pub fn timed_out(&self) -> bool {
        self.expiry() != RUNNING
    }
    /// 确实有工作完成：重新开始空闲时间窗。对总时长上限没有影响。
    pub fn progressed(&self) {
        *self.clock.progressed.lock().unwrap_or_else(|e| e.into_inner()) = self.clock.time.now();
    }
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
        let mut boundary = self.boundary.lock().unwrap_or_else(|e| e.into_inner());
        boundary.closed = true;
    }
    /// 校验完整的信封，并让接收与取消互斥、按顺序进行。导入方还必须把自己的幂等键和业务结果
    /// 在同一个事务里持久化。
    pub fn receive<T>(&self, identity: &TaskIdentity, apply: impl FnOnce() -> Result<T>) -> Result<T> {
        self.check()?;
        let mut boundary = self.boundary.lock().unwrap_or_else(|e| e.into_inner());
        if boundary.closed
            || boundary.received
            || self.cancel.load(Ordering::SeqCst)
            || self.clock.reason() != RUNNING
            || identity != &self.identity
        {
            return Err(Error::Conflict("任务结果已过期或重复".into()));
        }
        let value = apply()?;
        boundary.received = true;
        Ok(value)
    }
    /// 结果分批到达、worker 还在运行时（识别批次）的其中一批。与 `receive` 做同样的信封、取消、
    /// 关闭和截止时间检查，但可以重复；每一批的导入方自己必须是幂等且原子的。
    pub fn receive_part<T>(&self, identity: &TaskIdentity, apply: impl FnOnce() -> Result<T>) -> Result<T> {
        self.check()?;
        let boundary = self.boundary.lock().unwrap_or_else(|e| e.into_inner());
        if boundary.closed
            || boundary.received
            || self.cancel.load(Ordering::SeqCst)
            || self.clock.reason() != RUNNING
            || identity != &self.identity
        {
            return Err(Error::Conflict("任务结果已过期或重复".into()));
        }
        apply()
    }
    /// 运行一个可选的 worker，不会因管道死锁。大量输出由调用方提供文件；stdout/stderr 默认为 null。
    /// 总是回收主进程并杀掉剩下的后代进程，包括主进程先于子进程退出的情况。worker 不得脱离自己的进程组。
    pub fn process(&self, command: &mut Command) -> Result<ExitStatus> {
        self.process_observed(command, || Ok(()))
    }
    /// `observe` 在等待期间运行；它返回错误时停止并回收 worker。
    pub fn process_observed(&self, command: &mut Command, observe: impl FnMut() -> Result<()>) -> Result<ExitStatus> {
        crate::platform_process::worker(command, || self.check(), observe)
    }
}
pub struct Task {
    context: TaskContext,
    outcome: Arc<Mutex<TaskOutcome>>,
    thread: Option<JoinHandle<()>>,
}
impl Task {
    pub fn start(
        identity: TaskIdentity,
        timeout: Option<Duration>,
        work: impl FnOnce(TaskContext) -> Result<()> + Send + 'static,
    ) -> Self {
        Self::start_with(identity, Deadline::fixed(timeout), work)
    }
    pub fn start_with(
        identity: TaskIdentity,
        deadline: Deadline,
        work: impl FnOnce(TaskContext) -> Result<()> + Send + 'static,
    ) -> Self {
        Self::start_timed(identity, deadline, Time::default(), work)
    }
    fn start_timed(
        identity: TaskIdentity,
        deadline: Deadline,
        time: Time,
        work: impl FnOnce(TaskContext) -> Result<()> + Send + 'static,
    ) -> Self {
        let now = time.now();
        let context = TaskContext {
            identity,
            cancel: Arc::new(AtomicBool::new(false)),
            clock: Arc::new(Clock {
                deadline,
                time,
                started: now,
                progressed: Mutex::new(now),
                expired: AtomicU8::new(RUNNING),
            }),
            boundary: Arc::new(Mutex::new(Boundary { received: false, closed: false })),
        };
        let outcome = Arc::new(Mutex::new(TaskOutcome::Running));
        let (ctx, state) = (context.clone(), outcome.clone());
        let thread = thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(ctx.clone())));
            let end = if ctx.cancel.load(Ordering::SeqCst) {
                TaskOutcome::Cancelled
            } else {
                match result {
                    Ok(Ok(())) => TaskOutcome::Done,
                    Ok(Err(e)) => TaskOutcome::Failed(e.to_string()),
                    Err(_) => {
                        use crate::diagnostics::{self, Cause, Operation, Publication, Retry, Stage};
                        let d = diagnostics::record(
                            Operation::Task,
                            Stage::Background,
                            Cause::Panic,
                            None,
                            Publication::Unknown,
                            Retry::CheckState,
                        );
                        TaskOutcome::Failed(format!("任务线程崩溃（{id}）", id = d.id))
                    }
                }
            };
            ctx.boundary.lock().unwrap_or_else(|e| e.into_inner()).closed = true;
            *state.lock().expect("app_tasks mutex poisoned") = end;
        });
        Self { context, outcome, thread: Some(thread) }
    }
    pub fn context(&self) -> TaskContext {
        self.context.clone()
    }
    pub fn running(&self) -> bool {
        self.thread.as_ref().is_some_and(|t| !t.is_finished())
    }
    pub fn outcome(&self) -> TaskOutcome {
        self.outcome.lock().expect("app_tasks mutex poisoned").clone()
    }
    /// 有时限的观察；超时不会让任务脱离。要取消并等线程结束，用 stop。
    pub fn wait(&mut self, timeout: Duration) -> bool {
        let until = Instant::now() + timeout;
        while self.running() {
            if Instant::now() >= until {
                return false;
            }
            thread::sleep(Duration::from_millis(1));
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        true
    }
    /// 取消，不再等待：线程会在下一个检查点自己结束，但阻塞在输入上的工作（没人写的管道）可能永远等不到那个检查点。
    /// 只用于退出，并在有时限的 `wait` 之后调用。
    pub fn abandon(mut self) {
        self.context.cancel();
        drop(self.thread.take());
    }
    pub fn stop(&mut self) {
        self.context.cancel();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
impl Drop for Task {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn identity() -> TaskIdentity {
        TaskIdentity::at(&std::env::temp_dir(), "synthetic", 1, "session").unwrap()
    }
    #[test]
    fn single_worker_kind_is_explicit_and_legacy_names_still_dispatch() {
        for kind in WORKER_KINDS {
            assert_eq!(worker_kind("illustash-worker", Some(kind)), Ok(kind));
            let legacy = format!("illustash-{kind}");
            assert_eq!(worker_kind(&legacy, None), Ok(kind));
            assert_eq!(worker_kind(&legacy, Some("")), Ok(kind));
            assert_eq!(worker_kind(&legacy, Some(kind)), Ok(kind));
            assert_eq!(worker_kind("renamed copy", Some(kind)), Ok(kind));
        }
        assert!(worker_kind("illustash-worker", None).unwrap_err().contains(WORKER_KIND_ENV));
        assert!(worker_kind("illustash-worker", Some("")).unwrap_err().contains(WORKER_KIND_ENV));
        assert!(worker_kind("illustash-worker", Some("ocr")).unwrap_err().contains("未知"));
        assert!(worker_kind("illustash-index", Some("research")).unwrap_err().contains("index，但"));
        assert!(worker_kind("illustash-terms", None).is_err());
    }
    impl Time {
        fn manual() -> Self {
            Self(Some(Arc::new(Mutex::new(Instant::now()))))
        }
        fn advance(&self, by: Duration) {
            *self.0.as_ref().expect("manual clock").lock().unwrap() += by;
        }
    }
    /// 一个一直运行到停下的任务，它的截止时间读自一个只在被告知时才走的时钟。
    fn waiting(deadline: Deadline) -> (Task, Time) {
        let time = Time::manual();
        let task = Task::start_timed(identity(), deadline, time.clone(), |ctx| loop {
            ctx.check()?;
            thread::sleep(Duration::from_millis(1));
        });
        (task, time)
    }
    #[test]
    fn idle_deadline_follows_progress_and_limit_bounds_the_whole_task() {
        let (mut task, time) = waiting(Deadline { idle: Some(Duration::from_millis(80)), limit: None });
        let ctx = task.context();
        // 稳定的进展让任务活过远超一个空闲时间窗的时长。
        for _ in 0..10 {
            time.advance(Duration::from_millis(79));
            assert!(ctx.check().is_ok(), "progressing task stopped");
            ctx.progressed();
        }
        // 它在最后一次进展后整整一个空闲时间窗时超时，不会更早。
        time.advance(Duration::from_millis(79));
        assert!(ctx.check().is_ok());
        time.advance(Duration::from_millis(1));
        assert_eq!(ctx.check().unwrap_err().to_string(), "不合法：任务超时：80 毫秒没有进展");
        assert!(ctx.timed_out());
        assert!(task.wait(Duration::from_secs(10)));
        assert_eq!(task.outcome(), TaskOutcome::Cancelled);
        // 进展只重启空闲时间窗；总时长上限仍从开始算起。
        let (mut task, time) =
            waiting(Deadline { idle: Some(Duration::from_secs(60)), limit: Some(Duration::from_millis(150)) });
        let ctx = task.context();
        for _ in 0..2 {
            time.advance(Duration::from_millis(70));
            ctx.progressed();
            assert!(ctx.check().is_ok());
        }
        time.advance(Duration::from_millis(10));
        assert_eq!(ctx.check().unwrap_err().to_string(), "不合法：任务超时：超过 150 毫秒上限");
        assert!(ctx.timed_out());
        assert!(task.wait(Duration::from_secs(10)));
        assert_eq!(super::human(Duration::from_secs(600)), "10 分钟");
        assert_eq!(super::human(Duration::from_secs(86_400)), "24 小时");
    }
    #[test]
    fn idle_deadline_on_the_real_clock_stops_a_task_without_progress() {
        // 在忙的机器上，等一个短时间窗只会等得更久，所以这里不会过早失败。
        let mut task =
            Task::start_with(identity(), Deadline { idle: Some(Duration::from_millis(30)), limit: None }, |ctx| loop {
                ctx.check()?;
                thread::sleep(Duration::from_millis(1));
            });
        let ctx = task.context();
        assert!(task.wait(Duration::from_secs(30)));
        assert!(ctx.timed_out());
        assert_eq!(ctx.check().unwrap_err().to_string(), "不合法：任务超时：30 毫秒没有进展");
        assert_eq!(task.outcome(), TaskOutcome::Cancelled);
    }
    #[test]
    fn cancellation_is_never_reported_as_timeout() {
        let (mut task, time) = waiting(Deadline { idle: Some(Duration::from_millis(30)), limit: None });
        let ctx = task.context();
        task.stop();
        time.advance(Duration::from_millis(60));
        assert!(!ctx.timed_out());
        assert_eq!(ctx.check().unwrap_err().to_string(), "不合法：任务已取消");
        assert_eq!(task.outcome(), TaskOutcome::Cancelled);
    }
    #[test]
    fn exact_envelope_retry_duplicate_and_closed_receipts() {
        let mut task = Task::start(identity(), None, |ctx| {
            let encoded = serde_json::to_vec(&ctx.identity).unwrap();
            assert_eq!(serde_json::from_slice::<TaskIdentity>(&encoded).unwrap(), ctx.identity);
            let mut wrong = ctx.identity.clone();
            wrong.package = PathBuf::from("other.pilib");
            assert!(ctx.receive(&wrong, || Ok(())).is_err());
            wrong = ctx.identity.clone();
            wrong.generation += 1;
            assert!(ctx.receive(&wrong, || Ok(())).is_err());
            wrong = ctx.identity.clone();
            wrong.session.push('x');
            assert!(ctx.receive(&wrong, || Ok(())).is_err());
            wrong = ctx.identity.clone();
            wrong.task_id.push('x');
            assert!(ctx.receive(&wrong, || Ok(())).is_err());
            assert!(ctx.receive(&ctx.identity, || Err::<(), _>(Error::Invalid("synthetic rollback".into()))).is_err());
            ctx.receive(&ctx.identity, || Ok(()))?;
            assert!(ctx.receive(&ctx.identity, || Ok(())).is_err());
            Ok(())
        });
        let ctx = task.context();
        assert!(task.wait(Duration::from_secs(2)));
        assert_eq!(task.outcome(), TaskOutcome::Done);
        assert!(ctx.receive(&ctx.identity, || Ok(())).is_err());
    }
    #[test]
    fn cancellation_wait_panic_and_new_session() {
        let mut task = Task::start(identity(), None, |ctx| loop {
            ctx.check()?;
            thread::sleep(Duration::from_millis(1));
        });
        let ctx = task.context();
        assert!(!task.wait(Duration::from_millis(5)));
        task.stop();
        assert_eq!(task.outcome(), TaskOutcome::Cancelled);
        assert!(ctx.receive(&ctx.identity, || Ok(())).is_err());
        let mut task = Task::start(identity(), None, |_| panic!("synthetic worker crash"));
        assert!(task.wait(Duration::from_secs(2)));
        assert!(matches!(task.outcome(), TaskOutcome::Failed(_)));
        let mut task = Task::start(identity(), None, |ctx| ctx.receive(&ctx.identity, || Ok(())));
        assert!(task.wait(Duration::from_secs(2)));
        assert_eq!(task.outcome(), TaskOutcome::Done);
        assert_ne!(ctx.identity, task.context().identity);
    }
    #[cfg(unix)]
    #[test]
    fn worker_timeout_cancel_failure_and_parent_first_exit_clean_group() {
        for (script, timeout, cancel) in [
            ("wait", Some(Duration::from_millis(60)), false),
            ("wait", None, true),
            ("exit 0", None, false),
            ("exit 7", None, false),
        ] {
            let file = std::env::temp_dir().join(format!("illustash-group-{}", identity().task_id));
            let output = file.clone();
            // 超时用的时钟只在进程组公布后才拨动：shell 启动慢时，不会再在它写文件之前就被杀掉。
            let time = Time::manual();
            let mut task = Task::start_timed(identity(), Deadline::fixed(timeout), time.clone(), move |ctx| {
                let mut cmd = Command::new("/bin/sh");
                cmd.arg("-c").arg(format!("sleep 60 & echo $$ > '{}'; {script}", output.display()));
                let status = ctx.process(&mut cmd)?;
                if status.success() {
                    Ok(())
                } else {
                    Err(Error::Invalid("worker failed".into()))
                }
            });
            let until = Instant::now() + Duration::from_secs(10);
            let pgid = loop {
                if let Ok(text) = std::fs::read_to_string(&file) {
                    if let Ok(pgid) = text.trim().parse::<i32>() {
                        break pgid;
                    }
                }
                assert!(Instant::now() < until, "worker did not publish its process group");
                thread::sleep(Duration::from_millis(1));
            };
            time.advance(Duration::from_millis(60));
            if cancel {
                task.stop();
            } else {
                assert!(task.wait(Duration::from_secs(10)));
            }
            let until = Instant::now() + Duration::from_secs(10);
            // SAFETY: kill 的信号 0 只检查进程组是否存在，不发送信号。
            while unsafe { libc::kill(-pgid, 0) } == 0 && Instant::now() < until {
                thread::sleep(Duration::from_millis(10));
            }
            // SAFETY: kill 的信号 0 只检查进程组是否存在，不发送信号。
            assert_ne!(unsafe { libc::kill(-pgid, 0) }, 0, "residual process group {pgid}");
            if cancel || timeout.is_some() {
                assert_eq!(task.outcome(), TaskOutcome::Cancelled);
            }
            std::fs::remove_file(file).unwrap();
        }
    }
}
