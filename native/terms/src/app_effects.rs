//! 没有配置研究 worker 时：Server._after_operation('verdict') 在 learner=None 下是成功的空操作。
//! 配置了 worker 时，把事件留在待处理状态，由 app_research 原子发布。
use crate::Result;
use rusqlite::{OpenFlags, OptionalExtension, TransactionBehavior};
use std::path::Path;

/// 人工操作提交之后调用：无论这里发生什么，它的结果都成立。失败时事件留待下次处理（另一次操作、启动或定期恢复），
/// 只记日志，所以响应从不把已提交的操作报成已回滚。
pub(crate) fn drain_after_commit(db: &Path) {
    if let Err(error) = drain(db) {
        eprintln!("人工操作已提交，verdict事件处理失败，稍后重试：{error}");
    }
}

pub(crate) fn drain(db: &Path) -> Result<()> {
    if crate::app_research::configured() {
        return Ok(());
    }
    // 没有待处理的工作：只打开读连接，不建表、不写连接日志，也不写应用数据。
    let read = crate::sqlite::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    crate::configure(&read)?;
    let pending: bool = read.query_row(
        "SELECT EXISTS(SELECT 1 FROM backend_effects WHERE kind='verdict' AND status='pending')",
        [],
        |r| r.get(0),
    )?;
    drop(read);
    if !pending {
        return Ok(());
    }
    let mut c = crate::sqlite::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    crate::configure(&c)?;
    loop {
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let id: Option<i64> = tx
            .query_row(
                "SELECT id FROM backend_effects WHERE kind='verdict' AND status='pending' ORDER BY id LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        let Some(id) = id else {
            return Ok(());
        };
        // 与没有 learner 时的 verdict 处理完全一致。不要确认导入的或未知的事件，也不要启动 worker。
        tx.execute("UPDATE backend_effects SET status='done',error='' WHERE id=?", [id])?;
        tx.commit()?;
    }
}

/// 会话在这一刻对研究的看法（`GET /api/learning`，#404）：在会话锁下取，取完就放锁，再去读数据库。
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Learner {
    /// 同步在运行：研究要等它结束才启动（`Research::recover` 的 `sync_busy`）。
    pub(crate) sync: bool,
    /// learner 的公开状态是 `error`：约 60 秒后自动重试，重试用完就停。
    pub(crate) error: bool,
    /// 本会话里研究不会自己继续：用户取消过、自动重试用完，或者图库是只读的验收图库。
    pub(crate) stopped: bool,
}

/// 编号不超过 `upto` 的 verdict 事件是否都已处理完，以及据以判断的 `upto`。
///
/// 没有给 `upto` 时取当前最大的 verdict 编号（没有就是 0）：写入提交以后才回复，所以回复之后第一次读到的编号一定不小于
/// 那次写入的编号。`upto` 不是非负整数，或者比当前最大编号还大（比如换了图库），按请求不合法拒绝。
/// 只看 `verdict`：`imported`、`index_changed` 没配 worker 时没人确认，算进来就永远等不到。
pub(crate) fn settled(db: &Path, upto: Option<&str>) -> Result<(i64, bool)> {
    let c = crate::app_library::read_connection(db)?;
    let latest: i64 =
        c.query_row("SELECT coalesce(max(id),0) FROM backend_effects WHERE kind='verdict'", [], |r| r.get(0))?;
    let upto = match upto {
        None => latest,
        Some(text) => match text.parse::<i64>() {
            Ok(n) if n >= 0 && n <= latest => n,
            _ => return crate::error::invalid("upto 须为不超过当前最大事件编号的非负整数"),
        },
    };
    let pending: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM backend_effects WHERE kind='verdict' AND status='pending' AND id<=?)",
        [upto],
        |r| r.get(0),
    )?;
    Ok((upto, !pending))
}

/// `GET /api/learning` 的 `state`：从上往下取第一条成立的（契约见 #404）。
pub(crate) fn learning_state(done: bool, learner: Learner) -> &'static str {
    if done {
        "done"
    } else if learner.sync {
        "sync"
    } else if learner.error {
        "error"
    } else if learner.stopped {
        "stopped"
    } else {
        "pending"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Error;
    use std::{
        fs,
        sync::atomic::{AtomicU64, Ordering},
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Fixture(std::path::PathBuf);
    impl Fixture {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!(
                "illustash-native-effects-{}-{}.sqlite",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let c = crate::sqlite::open(&p).unwrap();
            c.execute_batch("CREATE TABLE backend_effects(id INTEGER PRIMARY KEY,kind TEXT,status TEXT NOT NULL DEFAULT 'pending',error TEXT NOT NULL DEFAULT '',created_at TEXT DEFAULT CURRENT_TIMESTAMP);").unwrap();
            Self(p)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }
    #[test]
    fn acknowledges_only_verdict_and_idle_drain_is_readonly() {
        let f = Fixture::new();
        let c = crate::sqlite::open(&f.0).unwrap();
        c.execute_batch("INSERT INTO backend_effects(kind,error) VALUES ('verdict','old failure'),('imported','keep'),('future-kind','keep');").unwrap();
        drain(&f.0).unwrap();
        assert_eq!(
            c.query_row("SELECT status||':'||error FROM backend_effects WHERE kind='verdict'", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            "done:"
        );
        assert_eq!(
            c.query_row("SELECT count(*) FROM backend_effects WHERE status='pending'", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            2
        );
        drop(c);
        let before = fs::read(&f.0).unwrap();
        drain(&f.0).unwrap();
        assert_eq!(before, fs::read(&f.0).unwrap());
    }
    #[test]
    fn ack_sql_failure_is_reported_pending_survives_and_later_recovery_succeeds() {
        let f = Fixture::new();
        let c = crate::sqlite::open(&f.0).unwrap();
        c.execute_batch("INSERT INTO backend_effects(kind) VALUES ('verdict'); CREATE TRIGGER fail_ack BEFORE UPDATE ON backend_effects BEGIN SELECT RAISE(ABORT,'synthetic ack failure'); END;").unwrap();
        assert!(matches!(drain(&f.0), Err(Error::Sql(_))));
        assert_eq!(
            c.query_row("SELECT status FROM backend_effects", [], |r| r.get::<_, String>(0)).unwrap(),
            "pending"
        );
        c.execute_batch("DROP TRIGGER fail_ack").unwrap();
        drain(&f.0).unwrap();
        assert_eq!(c.query_row("SELECT status FROM backend_effects", [], |r| r.get::<_, String>(0)).unwrap(), "done");
    }
    #[test]
    fn settled_defaults_to_latest_verdict_and_ignores_other_kinds() {
        let f = Fixture::new();
        assert_eq!(settled(&f.0, None).unwrap(), (0, true));
        let c = crate::sqlite::open(&f.0).unwrap();
        c.execute_batch(
            "INSERT INTO backend_effects(id,kind,status) VALUES (1,'verdict','done'),(2,'verdict','pending'),\
             (3,'imported','pending'),(4,'verdict','done'),(5,'index_changed','pending');",
        )
        .unwrap();
        assert_eq!(settled(&f.0, None).unwrap(), (4, false));
        assert_eq!(settled(&f.0, Some("1")).unwrap(), (1, true));
        assert_eq!(settled(&f.0, Some("2")).unwrap(), (2, false));
        c.execute("UPDATE backend_effects SET status='done' WHERE id=2", []).unwrap();
        // imported 和 index_changed 留在 pending 也不挡。
        assert_eq!(settled(&f.0, Some("4")).unwrap(), (4, true));
        assert_eq!(settled(&f.0, Some("0")).unwrap(), (0, true));
    }
    #[test]
    fn settled_rejects_bad_or_out_of_range_upto() {
        let f = Fixture::new();
        let c = crate::sqlite::open(&f.0).unwrap();
        c.execute_batch("INSERT INTO backend_effects(id,kind,status) VALUES (7,'verdict','pending');").unwrap();
        for bad in ["8", "-1", "x", "", "1.5", "99999999999999999999"] {
            assert!(matches!(settled(&f.0, Some(bad)), Err(Error::Invalid(_))), "{bad}");
        }
        assert_eq!(settled(&f.0, Some("7")).unwrap(), (7, false));
    }
    #[test]
    fn state_takes_the_first_matching_row() {
        let l = |sync, error, stopped| Learner { sync, error, stopped };
        // 落定优先于一切：同步、出错、停下都不影响已经处理完的事件。
        for (sync, error, stopped) in [(false, false, false), (true, true, true), (false, true, true)] {
            assert_eq!(learning_state(true, l(sync, error, stopped)), "done");
        }
        assert_eq!(learning_state(false, l(true, true, true)), "sync");
        assert_eq!(learning_state(false, l(false, true, true)), "error");
        assert_eq!(learning_state(false, l(false, false, true)), "stopped");
        assert_eq!(learning_state(false, Learner::default()), "pending");
    }
}
