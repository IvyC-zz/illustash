// Isolated test-only storage adapter. No HTTP, CLI or platform host dependency.
use crate::{Error, Result};
use rusqlite::Connection;
use std::path::Path;
pub fn open(path: &Path) -> Result<Connection> {
    let c = Connection::open(path)?;
    crate::configure(&c)?;
    Ok(c)
}
pub fn capability() -> crate::backup::RequiredBackup {
    crate::backup::RequiredBackup::new(
        |source, purpose| {
            let folder = source.parent().unwrap().join("library-backups");
            std::fs::create_dir_all(&folder)?;
            let path = folder.join(format!("terms-{purpose}-{}.sqlite", crate::store::new_key()?));
            drop(std::fs::OpenOptions::new().write(true).create_new(true).open(&path)?);
            Ok(path)
        },
        |path| Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE).map_err(Error::from),
        |path| {
            let _ = std::fs::remove_file(path);
        },
    )
}
pub mod sqlite {
    pub use rusqlite::Connection;
    pub fn open(path: impl AsRef<std::path::Path>) -> rusqlite::Result<Connection> {
        Connection::open(path)
    }
    pub fn open_with_flags(
        path: impl AsRef<std::path::Path>,
        flags: rusqlite::OpenFlags,
    ) -> rusqlite::Result<Connection> {
        Connection::open_with_flags(path, flags)
    }
}
pub mod platform_backup {
    pub fn capability() -> crate::backup::RequiredBackup {
        super::capability()
    }
}
pub mod schema {
    pub use crate::schema_rules::*;
    pub fn migrate(c: &mut Connection) -> Result<i64> {
        crate::schema_rules::migrate_with_backup(c, &super::capability())
    }
    use super::*;
}
pub use schema::migrate;
