//! 会话后台保护人工修订；合并等待从首次变脏算起，文件成功发布后才清理旧快照。
//! 快照只用已有摘要，随后补算。每次开库第一次补算让最近一份快照缺的摘要全部补齐时，立刻再出一份，不等一小时（#516）。
use super::*;
use crate::{
    app_library::Library,
    app_tasks::{Deadline, Task, TaskContext, TaskIdentity},
    http_json::Json,
};
use serde::{Deserialize, Serialize};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

const COALESCE: i64 = 60;
const INTERVAL: i64 = 3600;
const RETRY: i64 = 300;
const KEEP: usize = 30;

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct State {
    protected_revision: Option<i64>,
    pending_revision: Option<i64>,
    pending_since: Option<i64>,
    last_success: Option<i64>,
    snapshot_id: Option<String>,
    state_sha256: Option<String>,
    hash_missing: u64,
    last_error: Option<String>,
    retry_at: Option<i64>,
    cleanup_error: Option<String>,
    #[serde(skip)]
    snapshots: Vec<files::Item>,
    #[serde(skip)]
    running: bool,
    #[serde(skip)]
    unreadable_snapshots: usize,
    /// 最近一份快照之后那一轮补算仍缺摘要的图数，即现在确实读不到的图；补算还没跑完时为空。
    #[serde(skip)]
    hash_unreadable: Option<u64>,
    /// 补算让最近一份快照缺的摘要全部补齐，下一份快照不受合并等待和一小时间隔限制；每次开库最多一次。
    #[serde(skip)]
    refill: bool,
}
impl State {
    fn observe(&mut self, revision: i64, now: i64) {
        self.pending_revision = (Some(revision) != self.protected_revision).then_some(revision);
        if self.pending_revision.is_some() {
            self.pending_since.get_or_insert(now);
        } else {
            self.pending_since = None;
        }
    }
    fn due(&self, now: i64) -> bool {
        if self.retry_at.is_some_and(|at| now < at) {
            return false;
        }
        if self.snapshots.is_empty() || self.refill {
            return true;
        }
        self.pending_revision.is_some()
            && self.pending_since.is_some_and(|at| now >= at + COALESCE)
            && self.last_success.is_none_or(|at| now >= at + INTERVAL)
    }
    fn next(&self) -> Option<i64> {
        if self.refill {
            return self.retry_at.or(self.pending_since);
        }
        if self.pending_revision.is_none() && !self.snapshots.is_empty() && self.retry_at.is_none() {
            return None;
        }
        Some(
            self.retry_at
                .unwrap_or(0)
                .max(self.pending_since.unwrap_or(0) + if self.snapshots.is_empty() { 0 } else { COALESCE })
                .max(if self.snapshots.is_empty() { 0 } else { self.last_success.unwrap_or(0) + INTERVAL }),
        )
    }
}

#[derive(Default)]
pub(crate) struct Backups {
    task: Option<Task>,
    state: Arc<Mutex<State>>,
}
impl Backups {
    pub(crate) fn start(&mut self, library: &Library, generation: u64, token: &str) -> Result<()> {
        if self.task.is_some() {
            return Ok(());
        }
        let identity = TaskIdentity::new(library, generation, token)?;
        let db = library.db();
        let library_id = library.id().to_owned();
        let shared = self.state.clone();
        self.task = Some(Task::start_with(identity, Deadline::default(), move |context| {
            run(&db, &library_id, &context, &shared)
        }));
        Ok(())
    }
    pub(crate) fn status(&self) -> Json {
        let state = self.state.lock().expect("人工备份状态锁中毒");
        let mut value = serde_json::to_value(&*state).expect("备份状态可序列化");
        value["snapshots"] = serde_json::to_value(&state.snapshots).expect("快照清单可序列化");
        value["next_attempt"] = serde_json::json!(state.next());
        value["running"] = serde_json::json!(state.running);
        value["unreadable_snapshots"] = serde_json::json!(state.unreadable_snapshots);
        value["hash_unreadable"] = serde_json::json!(state.hash_unreadable);
        value["refill"] = serde_json::json!(state.refill);
        if self.task.as_ref().is_some_and(|t| matches!(t.outcome(), crate::app_tasks::TaskOutcome::Failed(_))) {
            value["last_error"] = serde_json::json!("人工备份任务异常结束");
        }
        value.into()
    }
    pub(crate) fn restore_path(&self, db: &Path, id: &str) -> Result<(PathBuf, String)> {
        let state = self.state.lock().expect("人工备份状态锁中毒");
        let Some(item) = state.snapshots.iter().find(|s| s.backup_id == id) else {
            return Err(Error::invalid("备份不存在或尚未通过完整性检查"));
        };
        Ok((files::path(&files::directory(db), id)?, item.state_sha256.clone()))
    }
    pub(crate) fn stop(&mut self) {
        if let Some(mut task) = self.task.take() {
            // 不等待整库哈希；取消先到则接收边界拒绝后续发布。已在发布中的短事务完成后即释放。
            task.context().cancel();
            task.wait(Duration::ZERO);
            task.abandon();
        }
        self.state.lock().expect("人工备份状态锁中毒").running = false;
    }
}
impl Drop for Backups {
    fn drop(&mut self) {
        self.stop();
    }
}

fn save(db: &Path, state: &State, context: &TaskContext) -> Result<()> {
    let encoded = serde_json::to_string(state).expect("备份状态可序列化");
    context.receive_part(context.identity(), || {
        let c = crate::open(db)?;
        c.execute("INSERT INTO human_backup_state(id,state_json) VALUES (1,?1) ON CONFLICT(id) DO UPDATE SET state_json=excluded.state_json", [encoded])?;
        Ok(())
    })
}
fn load(db: &Path) -> Result<State> {
    use rusqlite::OptionalExtension;
    let c = crate::app_library::read_connection(db)?;
    let encoded: Option<String> =
        c.query_row("SELECT state_json FROM human_backup_state WHERE id=1", [], |r| r.get(0)).optional()?;
    encoded
        .map(|s| serde_json::from_str(&s).map_err(|_| Error::invalid("人工备份状态不完整")))
        .transpose()
        .map(Option::unwrap_or_default)
}
fn run(db: &Path, library: &str, context: &TaskContext, shared: &Arc<Mutex<State>>) -> Result<()> {
    // 仅供旧库升级等价测试：闸门释放后仍执行真实首备份；等待期间不打开数据库或持有发布锁。
    if let Some(gate) = std::env::var_os("ILLUSTASH_TEST_HUMAN_BACKUP_GATE") {
        while Path::new(&gate).try_exists()? {
            context.check()?;
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    let mut state = State::default();
    let mut initialized = false;
    let mut fill = true;
    let mut refilled = false;
    loop {
        context.check()?;
        let now = chrono::Utc::now().timestamp();
        if state.retry_at.is_none_or(|at| now >= at) {
            let result = (|| {
                if !initialized {
                    state = load(db)?;
                    let catalog = files::list(&files::directory(db), library, &|| context.check())?;
                    state.unreadable_snapshots = catalog.unreadable;
                    state.snapshots = catalog.items;
                    if let Some(latest) = state.snapshots.first() {
                        // 发布后进程若来不及保存状态，仍从已校验的文件恢复最近成功事实。
                        if state.snapshot_id.as_deref() != Some(&latest.backup_id) {
                            state.protected_revision = None;
                            // 崩溃后无持久发布时间，不从生成头猜发布时刻；从本次开库等满间隔。
                            state.last_success = Some(now);
                        }
                        state.snapshot_id = Some(latest.backup_id.clone());
                        state.state_sha256 = Some(latest.state_sha256.clone());
                    }
                    initialized = true;
                }
                if state.retry_at.is_some_and(|at| now < at) {
                    return Ok(());
                }
                let retrying = state.last_error.is_some();
                let current = revision::read(&crate::app_library::read_connection(db)?)?;
                state.observe(current, now);
                if state.due(now) {
                    state.running = true;
                    *shared.lock().expect("人工备份状态锁中毒") = state.clone();
                    let captured = capture_guarded(db, library, Mode::Cached, &|| context.check(), Some(context))?;
                    let item = files::Item::from_snapshot(&captured.snapshot)?;
                    if state.state_sha256.as_deref() != Some(&item.state_sha256) || state.snapshots.is_empty() {
                        let destination = files::path(&files::directory(db), &item.backup_id)?;
                        let prepared = files::Prepared::new(&captured.snapshot, &destination, &|| context.check())?;
                        context.receive_part(context.identity(), || prepared.publish())?;
                        state.last_success = Some(chrono::Utc::now().timestamp());
                        state.snapshot_id = Some(item.backup_id.clone());
                        state.state_sha256 = Some(item.state_sha256.clone());
                        state.snapshots.insert(0, item.clone());
                        let mut catalog =
                            files::Catalog { items: state.snapshots.clone(), unreadable: state.unreadable_snapshots };
                        state.cleanup_error =
                            files::prune(&files::directory(db), &mut catalog, KEEP, &|| context.check());
                        state.snapshots = catalog.items;
                    }
                    state.hash_missing = item.hash_coverage.missing;
                    state.hash_unreadable = None;
                    state.refill = false;
                    state.protected_revision = Some(captured.revision);
                    state.observe(revision::read(&crate::app_library::read_connection(db)?)?, now);
                    state.last_error = None;
                    state.retry_at = None;
                    state.running = false;
                    save(db, &state, context)?;
                    fill = true;
                }
                if fill {
                    // 快照只用已有摘要，随后补算；补齐触发下一修订。两份自动文件至少间隔一小时，例外是每次开库
                    // 第一次这一轮把最近一份缺的摘要全部补齐：从 v8 升级的图库第一份快照全缺摘要（#516）。只放开
                    // 一次，平时新标的图照旧进下一小时的快照，不让每份快照后面都跟一份。
                    state.running = true;
                    *shared.lock().expect("人工备份状态锁中毒") = state.clone();
                    let filled = capture_guarded(db, library, Mode::Fill, &|| context.check(), Some(context))?;
                    let unreadable = filled.snapshot.coverage().missing;
                    state.hash_unreadable = Some(unreadable);
                    state.refill = !refilled && state.hash_missing > 0 && unreadable == 0;
                    refilled |= state.refill;
                    state.observe(revision::read(&crate::app_library::read_connection(db)?)?, now);
                    state.running = false;
                    fill = false;
                    save(db, &state, context)?;
                }
                if retrying {
                    state.last_error = None;
                    state.retry_at = None;
                    save(db, &state, context)?;
                }
                Ok::<_, Error>(())
            })();
            if let Err(error) = result {
                context.check()?;
                state.last_error = Some(error.reason());
                state.retry_at = Some(now + RETRY);
                state.running = false;
                // 数据库本身不能写时仍把错误留在可轮询状态，旧快照保持不动。
                if let Err(save_error) = save(db, &state, context) {
                    eprintln!("人工备份状态未能保存：{save_error}");
                }
            }
        }
        *shared.lock().expect("人工备份状态锁中毒") = state.clone();
        std::thread::sleep(Duration::from_secs(2));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn continuous_changes_do_not_postpone_and_retry_and_hour_are_independent() {
        let item = files::Item {
            backup_id: "a".into(),
            created_at: "2026-11-15T00:00:00Z".into(),
            state_sha256: "s".into(),
            hash_coverage: Default::default(),
            portable_complete: true,
        };
        let mut state =
            State { snapshots: vec![item], protected_revision: Some(1), last_success: Some(0), ..Default::default() };
        state.observe(2, 10);
        state.observe(3, 3590);
        assert!(!state.due(3599));
        assert!(state.due(3600));
        state.retry_at = Some(3900);
        assert!(!state.due(3899));
        assert!(state.due(3900));
        state.protected_revision = Some(3);
        state.observe(3, 3900);
        assert!(!state.due(5000));
        state.snapshots.clear();
        assert!(state.due(5000));
    }
}
