//! Read-only folder enumeration, hashing and image metadata; no source identity or database writes.
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
pub fn walk(root: &Path, folder: &Path, out: &mut Vec<File>, check: &dyn Fn() -> Result<()>) -> Result<()> {
    check()?;
    let Ok(entries) = fs::read_dir(folder) else { return Ok(()) };
    let mut entries = entries.filter_map(|e| e.ok()).collect::<Vec<_>>();
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
        let Ok(st) = path.metadata() else { continue };
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
            rel: path
                .strip_prefix(root)
                .expect("app_folder internal invariant")
                .to_string_lossy()
                .replace(std::path::MAIN_SEPARATOR, "/"),
            path,
            size: st.len() as i64,
            mtime,
            born: st.created().unwrap_or(modified),
            hash: String::new(),
        });
    }
    for dir in dirs {
        walk(root, &dir, out, check)?;
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
    // Same Pillow decompression bomb error threshold (2 * MAX_IMAGE_PIXELS), header only.
    let (w, h) = if u64::from(dimensions.0) * u64::from(dimensions.1) > 178_956_970 { (0, 0) } else { dimensions };
    let born: chrono::DateTime<chrono::Local> = file.born.into();
    (w, h, born.format("%Y-%m-%dT%H:%M:%S").to_string())
}
