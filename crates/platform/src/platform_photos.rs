//! 只读取显式给出的 Photos 来源，并探查它的本地文件。
use crate::{Error, Result};
use rusqlite::OpenFlags;
use serde_json::Value;
use std::path::{Path, PathBuf};
pub fn path(root: &Path, uuid: &str, directory: &str, filename: &str) -> Result<(String, PathBuf)> {
    // 数据库里的路径相对于 Photos；先拒绝越出目录的路径，再考虑原图。
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

fn root(raw: &str) -> Result<PathBuf> {
    Ok(if let Some(rest) = raw.strip_prefix("~/") {
        PathBuf::from(std::env::var_os("HOME").ok_or_else(|| Error::Invalid("HOME 未设置".into()))?).join(rest)
    } else {
        PathBuf::from(raw)
    })
}

/// 一张照片在本机的原图（只在 iCloud 里、还没下载时为 None）和它在「照片」里的原文件名（没有时为空），用于导出和
/// 打开时的副本（#450）。只读打开 Photos.sqlite。
pub fn original(raw: &str, uuid: &str) -> Result<(Option<PathBuf>, String)> {
    let root = root(raw)?;
    let photos = crate::sqlite::open_with_flags(root.join("database/Photos.sqlite"), OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    photos.busy_timeout(std::time::Duration::from_millis(250))?;
    let row: Option<(String, String, String)> = photos
        .query_row(
            "SELECT coalesce(a.ZDIRECTORY,''),coalesce(a.ZFILENAME,''),coalesce(d.ZORIGINALFILENAME,'') FROM ZASSET a LEFT JOIN ZADDITIONALASSETATTRIBUTES d ON a.ZADDITIONALATTRIBUTES=d.Z_PK WHERE a.ZUUID=?",
            [uuid],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .map(Some)
        .or_else(|e| if e == rusqlite::Error::QueryReturnedNoRows { Ok(None) } else { Err(e) })?;
    let Some((directory, filename, name)) = row else { return Ok((None, String::new())) };
    // 原文件名只用来起名，不拼进路径；目录和文件名和 `path` 一样先拒绝越出目录的值。
    let name = Path::new(&name).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if filename.is_empty()
        || [&directory, &filename]
            .iter()
            .any(|v| Path::new(v.as_str()).components().any(|c| !matches!(c, std::path::Component::Normal(_))))
    {
        return Ok((None, name));
    }
    let original = root.join("originals").join(directory).join(filename);
    Ok((original.is_file().then_some(original), name))
}

pub fn read(raw: &str) -> Result<(PathBuf, Vec<Value>)> {
    let root = root(raw)?;
    let db = root.join("database/Photos.sqlite");
    // 打开被拒时 SQLite 只报「unable to open database file」；直接打开文件才能知道原因（#202）。
    if let Err(error) = std::fs::File::open(&db) {
        if error.raw_os_error() == Some(libc::EPERM) {
            return Err(illustash_core::error::io_at(error, &db, None));
        }
    }
    let photos = crate::sqlite::open_with_flags(&db, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    photos.busy_timeout(std::time::Duration::from_millis(250))?;
    let rows=crate::source_rows::rows(&photos,"SELECT a.ZUUID,a.Z_PK,a.ZWIDTH,a.ZHEIGHT,a.ZADDEDDATE,a.ZHIDDEN,a.ZDIRECTORY,a.ZFILENAME,coalesce(d.ZIMPORTEDBYBUNDLEIDENTIFIER,''),d.ZORIGINALSTABLEHASH,d.ZORIGINALFILESIZE FROM ZASSET a LEFT JOIN ZADDITIONALASSETATTRIBUTES d ON a.ZADDITIONALATTRIBUTES=d.Z_PK WHERE a.ZKIND=0 AND a.ZTRASHEDSTATE=0")?;
    Ok((root, rows))
}

/// macOS 隐私设置没有允许本应用访问 Photos 图库（#202）：系统以 EPERM 拒绝打开，文件权限不足时则是 EACCES。
/// 唯一的出路是请用户在「系统设置」里允许。
pub fn denied(error: &Error) -> bool {
    matches!(error, Error::IoAt { source, .. } if source.raw_os_error() == Some(libc::EPERM))
}
