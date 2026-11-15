//! 人工修订的持久计数；不把机器证据与学习缓存当成人工操作。
use crate::Result;
use rusqlite::Connection;

/// 当前人工修订号；与读取状态使用同一个事务。
pub fn read(c: &Connection) -> Result<i64> {
    Ok(c.query_row("SELECT CAST(value AS INTEGER) FROM meta WHERE key='human_revision'", [], |r| r.get(0))?)
}

/// 无 action 的人工结构入口在原事务内标脏；旧版本迁移调用这些入口时尚无此键。
pub(crate) fn changed(c: &Connection) -> Result<()> {
    c.execute("UPDATE meta SET value=CAST(value AS INTEGER)+1 WHERE key='human_revision'", [])?;
    Ok(())
}

pub(crate) fn migrate(c: &Connection) -> Result<()> {
    c.execute_batch("CREATE TRIGGER human_action_revision_insert AFTER INSERT ON actions BEGIN
        UPDATE meta SET value=CAST(value AS INTEGER)+1 WHERE key='human_revision'; END;
        CREATE TRIGGER human_action_revision_undo AFTER UPDATE OF undone ON actions WHEN NEW.undone IS NOT OLD.undone BEGIN
        UPDATE meta SET value=CAST(value AS INTEGER)+1 WHERE key='human_revision'; END;
        CREATE TRIGGER human_verdict_revision AFTER INSERT ON term_verdicts BEGIN
        UPDATE meta SET value=CAST(value AS INTEGER)+1 WHERE key='human_revision'; END;
        ")?;
    for (table, condition) in
        [("term_names", "source='user'"), ("term_links", "source='user'"), ("term_records", "origin='user'")]
    {
        for (event, alias) in [("INSERT", "NEW"), ("DELETE", "OLD")] {
            c.execute_batch(&format!("CREATE TRIGGER human_{table}_revision_{event} AFTER {event} ON {table}
                WHEN {alias}.{condition} BEGIN UPDATE meta SET value=CAST(value AS INTEGER)+1 WHERE key='human_revision'; END;"))?;
        }
        c.execute_batch(&format!("CREATE TRIGGER human_{table}_revision_UPDATE AFTER UPDATE ON {table}
            WHEN OLD.{condition} OR NEW.{condition} BEGIN UPDATE meta SET value=CAST(value AS INTEGER)+1 WHERE key='human_revision'; END;"))?;
    }
    Ok(())
}
