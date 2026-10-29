//! SQLite 文件名只在内部使用：显示和配置里的路径保持不变。
use rusqlite::{Connection, OpenFlags};
use std::path::{Path, PathBuf};

/// Win32 扩展路径即使没有 longPathAware 清单也不受 MAX_PATH 限制。
/// 新的数据库文件还不存在，所以规范化的是上级目录。加前缀之前，这一步也解析了相对路径、点号段和 UNC 根。
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
        // 内置的 SQLite 已经提供了更大的 mxPathname 缓冲区。ATTACH 沿用这个 VFS；
        // 扩展文件名也覆盖它的 journal 和 WAL 文件。
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
