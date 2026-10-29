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
}
