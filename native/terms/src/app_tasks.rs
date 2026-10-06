//! Shared cooperative task lifetime. Business persistence/retry remains with the consumer.
use crate::{app_library::Library, Error, Result};
use std::{
    path::{Path, PathBuf},
    process::{Command, ExitStatus},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

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
#[derive(Clone)]
pub struct TaskContext {
    identity: TaskIdentity,
    cancel: Arc<AtomicBool>,
    deadline: Option<Instant>,
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
    pub fn check(&self) -> Result<()> {
        if self.deadline.is_some_and(|d| Instant::now() >= d) {
            self.cancel.store(true, Ordering::SeqCst);
        }
        if self.cancel.load(Ordering::SeqCst) {
            Err(Error::Invalid("任务已取消或超时".into()))
        } else {
            Ok(())
        }
    }
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
        let mut boundary = self.boundary.lock().unwrap_or_else(|e| e.into_inner());
        boundary.closed = true;
    }
    /// Validate the complete envelope and serialize receipt against cancellation. Importers must
    /// additionally persist their idempotency key in the same transaction as the business result.
    pub fn receive<T>(&self, identity: &TaskIdentity, apply: impl FnOnce() -> Result<T>) -> Result<T> {
        self.check()?;
        let mut boundary = self.boundary.lock().unwrap_or_else(|e| e.into_inner());
        if boundary.closed
            || boundary.received
            || self.cancel.load(Ordering::SeqCst)
            || self.deadline.is_some_and(|d| Instant::now() >= d)
            || identity != &self.identity
        {
            return Err(Error::Conflict("任务结果已过期或重复".into()));
        }
        let value = apply()?;
        boundary.received = true;
        Ok(value)
    }
    /// Run an optional worker without pipe deadlocks. The caller supplies files for large output;
    /// stdout/stderr are null by default. Always reap the leader and kill remaining descendants,
    /// including when the leader exits before its children. Workers must not detach their group.
    pub fn process(&self, command: &mut Command) -> Result<ExitStatus> {
        self.process_observed(command, || {})
    }
    pub fn process_observed(&self, command: &mut Command, observe: impl FnMut()) -> Result<ExitStatus> {
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
        let context = TaskContext {
            identity,
            cancel: Arc::new(AtomicBool::new(false)),
            deadline: timeout.map(|d| Instant::now() + d),
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
                    Err(_) => TaskOutcome::Failed("任务线程崩溃".into()),
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
    /// Bounded observation; expiry does not detach the task. Use stop for cancellation and join.
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
            let mut task = Task::start(identity(), timeout, move |ctx| {
                let mut cmd = Command::new("/bin/sh");
                cmd.arg("-c").arg(format!("sleep 60 & echo $$ > '{}'; {script}", output.display()));
                let status = ctx.process(&mut cmd)?;
                if status.success() {
                    Ok(())
                } else {
                    Err(Error::Invalid("worker failed".into()))
                }
            });
            let until = Instant::now() + Duration::from_secs(2);
            let pgid = loop {
                if let Ok(text) = std::fs::read_to_string(&file) {
                    if let Ok(pgid) = text.trim().parse::<i32>() {
                        break pgid;
                    }
                }
                assert!(Instant::now() < until, "worker did not publish its process group");
                thread::sleep(Duration::from_millis(1));
            };
            if cancel {
                task.stop();
            } else {
                assert!(task.wait(Duration::from_secs(2)));
            }
            let until = Instant::now() + Duration::from_secs(2);
            while unsafe { libc::kill(-pgid, 0) } == 0 && Instant::now() < until {
                thread::sleep(Duration::from_millis(10));
            }
            assert_ne!(unsafe { libc::kill(-pgid, 0) }, 0, "residual process group {pgid}");
            if cancel || timeout.is_some() {
                assert_eq!(task.outcome(), TaskOutcome::Cancelled);
            }
            std::fs::remove_file(file).unwrap();
        }
    }
}
