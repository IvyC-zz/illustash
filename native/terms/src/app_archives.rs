//! Package archives and session-owned exports. Only explicit newly created staging paths are removed.
use crate::{app_library::Library, http_json::Json, Error, Result};
use rusqlite::{
    backup::{Backup, StepResult},
    Connection,
};
use std::{
    fs::{self, File, OpenOptions},
    io::{BufReader, BufWriter, Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::Duration,
};
use zip::{write::SimpleFileOptions, ZipArchive, ZipWriter};
const EXPORTED: &[&str] = &[
    "library.json",
    "source.sqlite",
    "library.sqlite",
    "features/full-features.npy",
    "features/full-manifest.csv",
    "album-char-map.json",
];
fn invalid(e: impl std::fmt::Display) -> Error {
    Error::Invalid(e.to_string())
}
// Legacy library_action catches OSError/ValueError, but BadZipFile and decompression failures
// reach HTTP 500. Preserve status codes without depending on exception message text.
fn zip_failure(error: zip::result::ZipError) -> Error {
    match error {
        zip::result::ZipError::Io(error) => invalid(error),
        other => Error::Io(std::io::Error::other(other)),
    }
}
fn zip_read_failure(error: std::io::Error) -> Error {
    if matches!(error.kind(), std::io::ErrorKind::InvalidData | std::io::ErrorKind::UnexpectedEof) {
        Error::Io(error)
    } else {
        invalid(error)
    }
}
fn cancelled(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::SeqCst) {
        Err(Error::Invalid("导出已取消".into()))
    } else {
        Ok(())
    }
}
fn random() -> Result<String> {
    let mut bytes = [0; 16];
    getrandom::fill(&mut bytes).map_err(invalid)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
struct Stage(PathBuf, bool);
impl Drop for Stage {
    fn drop(&mut self) {
        if !self.1 {
            if let Err(error) = fs::remove_dir_all(&self.0) {
                eprintln!("归档临时目录清理失败 {}: {error}", self.0.display());
            }
        }
    }
}
fn stage(parent: &Path) -> Result<Stage> {
    fs::create_dir_all(parent)?;
    let path = parent.join(format!(".illustash-export-{}", random()?));
    fs::create_dir(&path)?;
    Ok(Stage(path, false))
}
fn copy_cancelled(mut from: impl Read, mut to: impl Write, cancel: &AtomicBool) -> Result<()> {
    let mut bytes = [0u8; 65536];
    loop {
        cancelled(cancel)?;
        let n = from.read(&mut bytes)?;
        if n == 0 {
            return Ok(());
        }
        to.write_all(&bytes[..n])?;
    }
}
pub(crate) fn export(library: &Library, dest: &Path, cancel: &AtomicBool) -> Result<PathBuf> {
    let work = stage(dest.parent().ok_or_else(|| invalid("无效导出路径"))?)?;
    let archive = work.0.join("archive.zip");
    let mut zip = ZipWriter::new(BufWriter::with_capacity(
        1 << 20,
        OpenOptions::new().write(true).create_new(true).open(&archive)?,
    ));
    for rel in EXPORTED {
        cancelled(cancel)?;
        let src = library.instance_path().join(rel);
        if !src.try_exists()? {
            continue;
        }
        let source = if src.extension().and_then(|s| s.to_str()) == Some("sqlite") {
            let copy = work.0.join(src.file_name().expect("app_archives internal invariant"));
            let a = crate::app_library::read_connection(&src)?;
            a.busy_timeout(Duration::from_millis(50))?;
            let mut b = crate::sqlite::open(&copy)?;
            let backup = Backup::new(&a, &mut b)?;
            let deadline = std::time::Instant::now() + Duration::from_secs(30);
            loop {
                cancelled(cancel)?;
                match backup.step(256)? {
                    StepResult::Done => break,
                    StepResult::More => {}
                    _ if std::time::Instant::now() >= deadline => return Err(invalid("数据库备份超时")),
                    _ => thread::sleep(Duration::from_millis(10)),
                }
            }
            drop(backup);
            drop(b);
            copy
        } else {
            src
        };
        zip.start_file(
            *rel,
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated).large_file(true),
        )
        .map_err(invalid)?;
        copy_cancelled(File::open(source)?, &mut zip, cancel)?;
    }
    let mut file = zip.finish().map_err(invalid)?;
    file.flush()?;
    drop(file);
    cancelled(cancel)?;
    fs::rename(&archive, dest)?;
    Ok(dest.to_owned())
}
struct CheckedWriter<W> {
    output: W,
    crc: crc32fast::Hasher,
    bytes: u64,
}
impl<W: Write> Write for CheckedWriter<W> {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        let n = self.output.write(data)?;
        self.crc.update(&data[..n]);
        self.bytes += n as u64;
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.output.flush()
    }
}
fn unpack<W: Write>(zip: &mut ZipArchive<BufReader<File>>, name: &str, mut output: W, buffer: &mut [u8]) -> Result<()> {
    let index = zip.index_for_name(name).ok_or_else(|| invalid("缺少归档内容"))?;
    let mut raw = zip.by_index_raw(index).map_err(zip_failure)?;
    if raw.compression() == zip::CompressionMethod::LZMA {
        // ZIP-LZMA has a version/property-length prefix and no .lzma unpacked-size header.
        // Adapt only that header; the decoder streams directly to the destination with size/CRC checks.
        let (size, crc) = (raw.size(), raw.crc32());
        let mut prefix = [0; 4];
        raw.read_exact(&mut prefix).map_err(zip_read_failure)?;
        if u16::from_le_bytes([prefix[2], prefix[3]]) != 5 {
            return Err(Error::Io(std::io::Error::other("无效 ZIP LZMA 属性")));
        }
        let mut header = [0; 13];
        raw.read_exact(&mut header[..5]).map_err(zip_read_failure)?;
        header[5..].copy_from_slice(&size.to_le_bytes());
        let mut input = BufReader::with_capacity(65536, std::io::Cursor::new(header).chain(raw));
        let mut checked = CheckedWriter { output, crc: crc32fast::Hasher::new(), bytes: 0 };
        lzma_rs::lzma_decompress(&mut input, &mut checked).map_err(|error| Error::Io(std::io::Error::other(error)))?;
        if checked.bytes != size || checked.crc.finalize() != crc {
            return Err(Error::Io(std::io::Error::other("Invalid checksum")));
        }
    } else {
        drop(raw);
        let mut source = zip.by_name(name).map_err(zip_failure)?;
        loop {
            let n = source.read(buffer).map_err(zip_read_failure)?;
            if n == 0 {
                break;
            }
            output.write_all(&buffer[..n]).map_err(invalid)?;
        }
    }
    Ok(())
}
pub(crate) fn import(archive: &Path) -> Result<(PathBuf, String)> {
    let mut zip = ZipArchive::new(BufReader::with_capacity(1 << 20, File::open(archive).map_err(invalid)?))
        .map_err(zip_failure)?;
    let mut names = std::collections::BTreeSet::new();
    for i in 0..zip.len() {
        let file = zip.by_index_raw(i).map_err(zip_failure)?;
        let name = file.name();
        // ZIP 只用 `/` 分隔；按原始字节拒绝反斜杠，不依赖各平台对 `features\x` 的解释。
        if file.name_raw().contains(&b'\\')
            || !EXPORTED.contains(&name)
            || file.enclosed_name().as_deref() != Some(Path::new(name))
            || file.is_dir()
        {
            return Err(invalid("导出文件里有不认识的内容，已拒绝导入"));
        }
        names.insert(name.to_owned());
    }
    if !names.contains("library.json") || !names.contains("source.sqlite") {
        return Err(invalid("不是导出的图库（缺少 library.json 或 source.sqlite）"));
    }
    // Only the small configuration is buffered; database/features contents are streamed with CRC verification.
    let mut buffer = vec![0u8; 1 << 20];
    let config_text = {
        let mut bytes = Vec::new();
        unpack(&mut zip, "library.json", &mut bytes, &mut buffer)?;
        String::from_utf8(bytes).map_err(invalid)?
    };
    let value: serde_json::Value = serde_json::from_str(&config_text).map_err(invalid)?;
    if !value.is_object() {
        return Err(Error::Io(std::io::Error::other("无效图库配置")));
    }
    let mut config = crate::app_settings::config_from_text(&config_text)?;
    let fmt = config.clone().response().get("format").cloned().ok_or_else(|| invalid("无效图库格式"))?;
    let valid = match &fmt {
        Json::Integer(v) => v.parse::<i64>().is_ok_and(|v| v <= crate::app_library::FORMAT),
        Json::Scalar(serde_json::Value::Number(v)) => v.as_i64().is_some_and(|v| v <= crate::app_library::FORMAT),
        Json::Scalar(serde_json::Value::Bool(_)) => true,
        _ => false,
    };
    if !valid {
        return Err(invalid("图库格式需要升级程序"));
    }
    let name = config
        .clone()
        .response()
        .get("name")
        .map(|v| v.text().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| archive.file_stem().unwrap_or_default().to_string_lossy().into_owned());
    let path = crate::app_libraries::unique_path(&name)?;
    fs::create_dir_all(path.parent().expect("internal file path must have a parent")).map_err(invalid)?;
    fs::create_dir(&path).map_err(invalid)?;
    let mut cleanup = Stage(path.clone(), false);
    for name in names {
        let out = path.join(&name);
        fs::create_dir_all(out.parent().expect("internal file path must have a parent")).map_err(invalid)?;
        let mut file = BufWriter::with_capacity(
            1 << 20,
            OpenOptions::new().write(true).create_new(true).open(out).map_err(invalid)?,
        );
        unpack(&mut zip, &name, &mut file, &mut buffer)?;
        file.flush().map_err(invalid)?;
    }
    for sub in ["features", "thumbnails", "backups"] {
        fs::create_dir_all(path.join(sub)).map_err(invalid)?;
    }
    config.set(
        "imported_from".into(),
        crate::app_settings::SettingValue::Scalar(
            archive.file_name().unwrap_or_default().to_string_lossy().into_owned().into(),
        ),
    );
    crate::app_settings::write_config(&path, &config).map_err(invalid)?;
    cleanup.1 = true;
    Ok((path, name))
}
struct Task {
    state: Arc<Mutex<Json>>,
    cancel: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    instance: PathBuf,
    generation: u64,
}
#[derive(Default)]
pub(crate) struct Exports(Mutex<Option<Task>>, AtomicBool);
impl Exports {
    pub(crate) fn state(&self, library: &Library, generation: u64) -> Json {
        let tasks = self.0.lock().expect("app_archives mutex poisoned");
        tasks
            .as_ref()
            .filter(|t| t.instance == library.instance_path() && t.generation == generation)
            .map(|t| t.state.lock().expect("app_archives mutex poisoned").clone())
            .unwrap_or_else(|| Json::object([("status", "idle".into())]))
    }
    pub(crate) fn start(&self, library: &Library, generation: u64) -> Result<Json> {
        let mut tasks = self.0.lock().expect("app_archives mutex poisoned");
        if self.1.load(Ordering::SeqCst) {
            return Err(invalid("服务正在退出"));
        }
        if tasks.as_ref().is_some_and(|t| t.handle.as_ref().is_some_and(|h| !h.is_finished())) {
            return Err(invalid("正在导出"));
        }
        if let Some(mut old) = tasks.take() {
            if let Some(handle) = old.handle.take() {
                let _ = handle.join();
            }
        }
        let date: String =
            Connection::open_in_memory()?.query_row("SELECT strftime('%Y%m%d','now','localtime')", [], |r| r.get(0))?;
        let dest =
            crate::app_paths::user_home()?.join("Downloads").join(format!("{}-{date}.pilib.zip", library.name()));
        let initial = Json::object([("status", "running".into()), ("path", dest.to_string_lossy().as_ref().into())]);
        let state = Arc::new(Mutex::new(initial.clone()));
        let cancel = Arc::new(AtomicBool::new(false));
        let (s, c, lib) = (state.clone(), cancel.clone(), library.clone());
        let handle = thread::Builder::new().name("library-export".into()).spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| export(&lib, &dest, &c)));
            let result = result.unwrap_or_else(|_| Err(invalid("导出任务异常退出")));
            *s.lock().expect("app_archives mutex poisoned") = match result {
                Ok(path) => Json::object([
                    ("status", "done".into()),
                    ("path", path.to_string_lossy().as_ref().into()),
                    ("bytes", serde_json::Value::from(fs::metadata(&path).map(|m| m.len()).unwrap_or(0)).into()),
                ]),
                Err(e) => Json::object([
                    ("status", "error".into()),
                    ("path", dest.to_string_lossy().as_ref().into()),
                    ("error", e.to_string().chars().take(200).collect::<String>().into()),
                ]),
            };
        })?;
        *tasks = Some(Task {
            state,
            cancel,
            handle: Some(handle),
            instance: library.instance_path().to_owned(),
            generation,
        });
        Ok(initial)
    }
    pub(crate) fn shutdown(&self) {
        self.1.store(true, Ordering::SeqCst);
        self.stop();
    }
    pub(crate) fn stop(&self) {
        if let Some(mut task) = self.0.lock().expect("app_archives mutex poisoned").take() {
            task.cancel.store(true, Ordering::SeqCst);
            if let Some(h) = task.handle.take() {
                let _ = h.join();
            }
        }
    }
}
impl Drop for Exports {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn backslash_entry_names_are_rejected_on_every_platform() {
        let path = std::env::temp_dir().join(format!("illustash-archive-backslash-{}.zip", std::process::id()));
        {
            let mut zip = ZipWriter::new(File::create(&path).unwrap());
            for name in ["library.json", "source.sqlite", "features\\full-features.npy"] {
                zip.start_file(name, SimpleFileOptions::default()).unwrap();
                zip.write_all(b"synthetic").unwrap();
            }
            zip.finish().unwrap();
        }
        let raw = fs::read(&path).unwrap();
        let needle: &[u8] = b"features\\full-features.npy";
        assert!(raw.windows(needle.len()).any(|w| w == needle), "fixture must keep the backslash");
        let error = import(&path).unwrap_err().to_string();
        fs::remove_file(&path).unwrap();
        assert!(error.contains("不认识的内容"), "{error}");
    }
}
