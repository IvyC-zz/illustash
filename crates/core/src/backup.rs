//! 必需的 SQLite 备份：规则调用不可替换的复制算法，宿主提供文件预留/VFS/owned 清理。
use crate::{Error, Result};
use rusqlite::Connection;
use std::path::{Path, PathBuf};

/// Host storage operations. There is deliberately no callback for "backup succeeded":
/// every file-backed migration performs the SQLite copy and quick_check in Core.
pub struct RequiredBackup {
    reserve: fn(&Path, &str) -> Result<PathBuf>,
    open: fn(&Path) -> Result<Connection>,
    remove_owned: fn(&Path),
}
impl RequiredBackup {
    pub fn new(
        reserve: fn(&Path, &str) -> Result<PathBuf>,
        open: fn(&Path) -> Result<Connection>,
        remove_owned: fn(&Path),
    ) -> Self {
        Self { reserve, open, remove_owned }
    }
    pub fn create(&self, conn: &Connection, source: &Path, purpose: &str) -> Result<PathBuf> {
        let path = (self.reserve)(source, purpose)?;
        if let Err(error) = self.copy_backup(conn, &path) {
            (self.remove_owned)(&path);
            return Err(error);
        }
        Ok(path)
    }
    fn copy_backup(&self, conn: &Connection, path: &std::path::Path) -> Result<()> {
        (|| -> Result<()> {
            let mut target = (self.open)(path)?;
            // A storage adapter returning an in-memory/source connection cannot acknowledge a file backup.
            if target.path().filter(|p| !p.is_empty()).is_none() || target.path() == conn.path() {
                return Err(Error::Invalid("升级备份未完成".into()));
            }
            let copy = rusqlite::backup::Backup::new(conn, &mut target)?;
            loop {
                match copy.step(100)? {
                    rusqlite::backup::StepResult::Done => break,
                    rusqlite::backup::StepResult::More => continue,
                    rusqlite::backup::StepResult::Busy | rusqlite::backup::StepResult::Locked => {
                        return Err(Error::Conflict("升级备份被占用，未执行迁移".into()));
                    }
                    _ => return Err(Error::Invalid("升级备份未完成".into())),
                }
            }
            drop(copy);
            // This invocation owns the quiescent backup. Make it standalone without WAL sidecars.
            target.pragma_update(None, "journal_mode", "DELETE")?;
            let check: String = target.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
            if check != "ok" {
                return Err(Error::Schema("升级备份完整性检查失败，未执行迁移".into()));
            }
            Ok(())
        })()
    }
}
/// Only SQLite's original empty main filename denotes an in-memory database.
pub(crate) fn required(conn: &Connection, purpose: &str, backup: &RequiredBackup) -> Result<Option<PathBuf>> {
    let Some(path) = conn.path().filter(|p| !p.is_empty()) else { return Ok(None) };
    backup.create(conn, Path::new(path), purpose).map(Some)
}
