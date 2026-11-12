//! 只读的文件夹遍历、哈希和图片元数据；不涉及来源身份，也不写数据库。
use crate::{Error, Result};
use md5::{Digest, Md5};
use std::{
    fs,
    io::{BufReader, Read},
    path::{Path, PathBuf},
    time::SystemTime,
};
#[derive(Clone)]
pub struct File {
    pub rel: String,
    pub path: PathBuf,
    pub size: i64,
    pub mtime: i64,
    pub born: SystemTime,
    pub hash: String,
}
/// 相对根目录的路径，用 `/` 分隔；根目录本身是空字符串。
fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .expect("app_folder internal invariant")
        .to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/")
}

/// `folder` 下的所有图片文件。读不到的东西（无法列出或含有读不出的条目的文件夹、取元数据失败的条目）
/// 以相对路径记进 `unreadable`，调用方就不会把「没读到」当成「已不在」。
pub fn walk(
    root: &Path,
    folder: &Path,
    out: &mut Vec<File>,
    unreadable: &mut Vec<String>,
    check: &dyn Fn() -> Result<()>,
) -> Result<()> {
    check()?;
    let Ok(entries) = fs::read_dir(folder) else {
        unreadable.push(relative(root, folder));
        return Ok(());
    };
    let mut listed = Vec::new();
    for entry in entries {
        match entry {
            Ok(entry) => listed.push(entry),
            Err(_) => {
                unreadable.push(relative(root, folder));
                break;
            }
        }
    }
    let mut entries = listed;
    entries.sort_by_key(|e| e.file_name());
    let mut dirs = Vec::new();
    for entry in entries {
        check()?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        let Ok(st) = path.metadata() else {
            unreadable.push(relative(root, &path));
            continue;
        };
        if st.is_dir() {
            if !name.ends_with(".pilib") && !entry.file_type().is_ok_and(|t| t.is_symlink()) {
                dirs.push(path);
            }
            continue;
        }
        if !st.is_file()
            || !matches!(
                path.extension().and_then(|e| e.to_str()).map(str::to_lowercase).as_deref(),
                Some("jpg" | "jpeg" | "png" | "webp" | "gif")
            )
        {
            continue;
        }
        let modified = st.modified()?;
        let mtime = match modified.duration_since(SystemTime::UNIX_EPOCH) {
            Ok(v) => i64::try_from(v.as_nanos()).map_err(|_| Error::Invalid("文件时间超出范围".into()))?,
            Err(e) => -i64::try_from(e.duration().as_nanos()).map_err(|_| Error::Invalid("文件时间超出范围".into()))?,
        };
        out.push(File {
            rel: relative(root, &path),
            path,
            size: st.len() as i64,
            mtime,
            born: st.created().unwrap_or(modified),
            hash: String::new(),
        });
    }
    for dir in dirs {
        walk(root, &dir, out, unreadable, check)?;
    }
    Ok(())
}
pub fn hash(path: &Path, check: &dyn Fn() -> Result<()>) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut h = Md5::new();
    let mut buf = vec![0; 1 << 20];
    loop {
        check()?;
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(format!("{:x}", h.finalize()))
}
pub fn describe(file: &File) -> (u32, u32, String) {
    let dimensions = (|| -> std::io::Result<(u32, u32)> {
        let reader = image::ImageReader::new(BufReader::new(fs::File::open(&file.path)?)).with_guessed_format()?;
        if reader.format() == Some(image::ImageFormat::Jpeg) {
            std::panic::catch_unwind(|| -> std::io::Result<(u32, u32)> {
                let decoder = mozjpeg::Decompress::new_reader(reader.into_inner())?;
                Ok((decoder.width() as u32, decoder.height() as u32))
            })
            .map_err(|_| std::io::Error::other("JPEG header"))?
        } else {
            reader.into_dimensions().map_err(std::io::Error::other)
        }
    })()
    .unwrap_or((0, 0));
    // 与 Pillow 的解压炸弹报错阈值（2 * MAX_IMAGE_PIXELS）相同，只看文件头。
    let (w, h) = if u64::from(dimensions.0) * u64::from(dimensions.1) > 178_956_970 { (0, 0) } else { dimensions };
    let born: chrono::DateTime<chrono::Local> = file.born.into();
    (w, h, born.format("%Y-%m-%dT%H:%M:%S").to_string())
}
