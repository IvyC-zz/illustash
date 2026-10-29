//! 必需的、落到文件的 SQLite 备份：私有的预留文件，保留 WAL 和 rowid 的复制，失败时只清理自己建的文件。
use crate::{Error, Result};
#[cfg(test)]
use rusqlite::Connection;

fn reserve_backup(path: &std::path::Path, purpose: &str) -> Result<std::path::PathBuf> {
    let folder = path.parent().unwrap_or(std::path::Path::new(".")).join("library-backups");
    std::fs::create_dir_all(&folder)?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| Error::Invalid(e.to_string()))?
        .as_nanos();
    let backup = folder.join(format!("terms-{purpose}-{stamp}-{}.sqlite", std::process::id()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    crate::platform_filesystem::private_file_options(&mut options);
    drop(options.open(&backup)?);
    Ok(backup)
}

/// 没有空的或默认的备份实现：成功时返回归调用方所有的备份路径。
pub fn capability() -> crate::backup::RequiredBackup {
    crate::backup::RequiredBackup::new(
        reserve_backup,
        |path| crate::sqlite::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE).map_err(Error::from),
        |path| {
            let _ = std::fs::remove_file(path);
        },
    )
}
#[cfg(test)]
fn create(conn: &Connection, source: &std::path::Path, purpose: &str) -> Result<std::path::PathBuf> {
    capability().create(conn, source, purpose)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failed_sqlite_copy_removes_only_its_reserved_file() {
        let root = std::env::temp_dir().join(format!("illustash-backup-copy-{}", crate::store::new_key().unwrap()));
        let folder = root.join("library-backups");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("sentinel"), "unrelated backup").unwrap();
        let source = root.join("source.sqlite");
        let conn = crate::sqlite::open(&source).unwrap();
        conn.execute_batch("CREATE TABLE synthetic(value); BEGIN IMMEDIATE; INSERT INTO synthetic VALUES (1);")
            .unwrap();
        assert!(create(&conn, &source, "synthetic-failure").is_err());
        assert_eq!(std::fs::read_dir(&folder).unwrap().count(), 1);
        assert_eq!(std::fs::read_to_string(folder.join("sentinel")).unwrap(), "unrelated backup");
        conn.execute_batch("ROLLBACK").unwrap();
        drop(conn);
        std::fs::remove_dir_all(root).unwrap();
    }
}
