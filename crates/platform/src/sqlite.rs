//! SQLite filenames are internal: keep display/config paths unchanged.
use rusqlite::{Connection, OpenFlags};
use std::path::{Path, PathBuf};

/// Win32 extended paths bypass MAX_PATH even without a longPathAware manifest.
/// Canonicalize the parent as new database files do not exist yet. This also
/// resolves relative paths, dot segments and UNC roots before adding the prefix.
pub fn path(value: impl AsRef<Path>) -> PathBuf {
    let value = value.as_ref();
    #[cfg(windows)]
    {
        if value == Path::new(":memory:") || value.as_os_str().is_empty() {
            return value.to_owned();
        }
        if let Ok(full) = std::fs::canonicalize(value) {
            return full;
        }
        if let (Some(parent), Some(name)) = (value.parent(), value.file_name()) {
            let parent = if parent.as_os_str().is_empty() { Path::new(".") } else { parent };
            if let Ok(parent) = std::fs::canonicalize(parent) {
                return parent.join(name);
            }
        }
    }
    value.to_owned()
}

pub fn open(value: impl AsRef<Path>) -> rusqlite::Result<Connection> {
    open_with_flags(value, OpenFlags::default())
}

pub fn open_with_flags(value: impl AsRef<Path>, flags: OpenFlags) -> rusqlite::Result<Connection> {
    #[cfg(windows)]
    {
        // Bundled SQLite already supplies the larger mxPathname buffer. ATTACH
        // inherits this VFS; extended filenames cover its journal/WAL files too.
        Connection::open_with_flags_and_vfs(path(value), flags, "win32-longpath")
    }
    #[cfg(not(windows))]
    {
        Connection::open_with_flags(value, flags)
    }
}

#[cfg(test)]
mod tests {
    #[cfg(windows)]
    use super::*;
    #[cfg(windows)]
    use std::fs;
    #[cfg(windows)]
    #[test]
    fn extended_unicode_path_exceeds_default_vfs_buffer() {
        let root = std::env::temp_dir().join(format!("illustash-vfs-{}", crate::store::new_key().unwrap()));
        let mut parent = root.clone();
        for _ in 0..30 {
            parent = parent.join("synthetic-unicode-合成路径-0123456789");
        }
        fs::create_dir_all(&parent).unwrap();
        let file = parent.join("probe.sqlite");
        assert!(path(&file).to_string_lossy().len() > 1040);
        let c = open(&file).unwrap();
        c.execute_batch("CREATE TABLE probe(value); INSERT INTO probe VALUES (1);").unwrap();
        drop(c);
        fs::remove_dir_all(root).unwrap();
    }
}
