//! Read only the explicitly supplied Photos source and probe its local files.
use crate::{Error, Result};
use rusqlite::OpenFlags;
use serde_json::Value;
use std::path::{Path, PathBuf};
pub fn path(root: &Path, uuid: &str, directory: &str, filename: &str) -> Result<(String, PathBuf)> {
    // Database paths are relative to Photos; reject traversal before considering originals.
    for value in [directory, filename] {
        if Path::new(value).components().any(|c| !matches!(c, std::path::Component::Normal(_))) {
            return Err(Error::Invalid("Photos 路径不合法".into()));
        }
    }
    if uuid.is_empty() || !uuid.is_ascii() || uuid.contains(['/', '\\']) {
        return Err(Error::Invalid("Photos 标识不合法".into()));
    }
    let derivative = root.join("resources/derivatives").join(&uuid[..1]).join(format!("{uuid}_1_105_c.jpeg"));
    let original = root.join("originals").join(directory).join(filename);
    Ok(if derivative.is_file() || filename.is_empty() || !original.is_file() {
        ("deriv".into(), derivative)
    } else {
        ("orig".into(), original)
    })
}

pub fn read(raw: &str) -> Result<(PathBuf, Vec<Value>)> {
    let root = if let Some(rest) = raw.strip_prefix("~/") {
        PathBuf::from(std::env::var_os("HOME").ok_or_else(|| Error::Invalid("HOME 未设置".into()))?).join(rest)
    } else {
        PathBuf::from(raw)
    };
    let photos = crate::sqlite::open_with_flags(root.join("database/Photos.sqlite"), OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    photos.busy_timeout(std::time::Duration::from_millis(250))?;
    let rows=crate::source_rows::rows(&photos,"SELECT a.ZUUID,a.Z_PK,a.ZWIDTH,a.ZHEIGHT,a.ZADDEDDATE,a.ZHIDDEN,a.ZDIRECTORY,a.ZFILENAME,coalesce(d.ZIMPORTEDBYBUNDLEIDENTIFIER,''),d.ZORIGINALSTABLEHASH,d.ZORIGINALFILESIZE FROM ZASSET a LEFT JOIN ZADDITIONALASSETATTRIBUTES d ON a.ZADDITIONALATTRIBUTES=d.Z_PK WHERE a.ZKIND=0 AND a.ZTRASHEDSTATE=0")?;
    Ok((root, rows))
}
