//! Package archives and session-owned exports. Only explicit newly created staging paths are removed.
use crate::{app_library::Library, http_json::Json, Error, Result};
use rusqlite::{
    backup::{Backup, StepResult},
    Connection,
};
use std::collections::HashMap;
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
fn import_cancelled(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::SeqCst) || crate::platform_process::stopping() {
        Err(Error::Conflict("程序正在退出，导入已取消".into()))
    } else {
        Ok(())
    }
}
/// The configuration is buffered and parsed in memory; real ones are a few kilobytes.
const CONFIG_LIMIT: u64 = 1 << 20;
/// Kept free after unpacking, so an import never fills the disk the library lives on.
const SPACE_MARGIN: u64 = 256 << 20;
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
/// Only call on an export snapshot or a newly unpacked package, never the sender's database.
/// Model connection settings (including future keys), lasting network consent and the first-run guide record
/// (which questions were answered, what was ticked: #116) are personal.
fn sanitize_database(path: &Path, cancel: Option<Arc<AtomicBool>>) -> Result<()> {
    let mut c = crate::sqlite::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    // Compaction may dominate large exports. Switching libraries/closing the app must
    // interrupt these SQLite operations as well as the streaming copy and backup.
    c.progress_handler(1000, cancel.map(|cancel| move || cancel.load(Ordering::SeqCst)));
    c.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA secure_delete=ON;")?;
    let tx = c.transaction()?;
    for (table, sql) in [
        ("name_settings", "DELETE FROM name_settings"),
        ("prefs", "DELETE FROM prefs WHERE key IN ('names_network','onboarding')"),
    ] {
        if tx.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?)", [table], |r| {
            r.get::<_, bool>(0)
        })? {
            tx.execute(sql, [])?;
        }
    }
    // VACUUM removes secrets left in free pages by older writes with secure_delete disabled.
    // SQLite rewrites implicit rowids only in unindexed tables. Temporary indexes preserve
    // those identities, including historical/user tables, without changing the final schema.
    let tables = tx
        .prepare("SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%'")?
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let quote = |s: &str| format!("\"{}\"", s.replace('"', "\"\""));
    let mut indexes = Vec::new();
    for table in tables {
        let table = quote(&table);
        if tx.prepare(&format!("PRAGMA index_list({table})"))?.exists([])? {
            continue;
        }
        let column: String = tx.query_row(&format!("PRAGMA table_info({table})"), [], |r| r.get(1))?;
        let index = quote(&format!("archive_rowid_{}", random()?));
        tx.execute_batch(&format!("CREATE INDEX {index} ON {table}({})", quote(&column)))?;
        indexes.push(index);
    }
    tx.commit()?;
    c.execute_batch("VACUUM")?;
    let tx = c.transaction()?;
    for index in indexes {
        tx.execute_batch(&format!("DROP INDEX {index}"))?;
    }
    tx.commit()?;
    Ok(())
}
pub(crate) fn export(library: &Library, dest: &Path, cancel: &Arc<AtomicBool>) -> Result<PathBuf> {
    let work = stage(dest.parent().ok_or_else(|| invalid("无效导出路径"))?)?;
    let archive = work.0.join("archive.zip");
    // The databases first, as one consistent set; compression afterwards holds no database.
    let copies = snapshot(library, &work.0, cancel)?;
    if let Some(copy) = copies.get("library.sqlite") {
        sanitize_database(copy, Some(cancel.clone()))?;
        cancelled(cancel)?;
    }
    let mut zip = ZipWriter::new(BufWriter::with_capacity(
        1 << 20,
        OpenOptions::new().write(true).create_new(true).open(&archive)?,
    ));
    for rel in EXPORTED {
        cancelled(cancel)?;
        let source = match copies.get(rel) {
            Some(copy) => copy.clone(),
            None => {
                let src = library.instance_path().join(rel);
                if rel.ends_with(".sqlite") || !src.try_exists()? {
                    continue;
                }
                src
            }
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
/// Copies of the package databases that belong together: every copy is retaken until no database has committed
/// anything between the first and the last copy (SQLite data_version), so the archive never pairs a source
/// from before a cross-database update with an application database from after it.
fn snapshot(library: &Library, work: &Path, cancel: &AtomicBool) -> Result<HashMap<&'static str, PathBuf>> {
    let mut readers = Vec::new();
    for rel in EXPORTED.iter().filter(|rel| rel.ends_with(".sqlite")) {
        let src = library.instance_path().join(rel);
        if src.try_exists()? {
            let c = crate::app_library::read_connection(&src)?;
            c.busy_timeout(Duration::from_millis(50))?;
            readers.push((*rel, c, work.join(rel)));
        }
    }
    let versions = |readers: &[(&str, rusqlite::Connection, PathBuf)]| -> Result<Vec<i64>> {
        readers.iter().map(|(_, c, _)| Ok(c.query_row("PRAGMA data_version", [], |r| r.get(0))?)).collect()
    };
    for _ in 0..SNAPSHOT_ATTEMPTS {
        cancelled(cancel)?;
        let before = versions(&readers)?;
        for (_, c, copy) in &readers {
            if copy.try_exists()? {
                fs::remove_file(copy)?;
            }
            backup_into(c, copy, cancel)?;
        }
        if versions(&readers)? == before {
            return Ok(readers.into_iter().map(|(rel, _, copy)| (rel, copy)).collect());
        }
    }
    Err(invalid("导出期间图库一直在写入，请稍后再导出"))
}
/// Retaking a snapshot is cheap next to compressing it; a library that keeps changing is reported instead.
const SNAPSHOT_ATTEMPTS: usize = 5;
fn backup_into(source: &rusqlite::Connection, copy: &Path, cancel: &AtomicBool) -> Result<()> {
    let mut b = crate::sqlite::open(copy)?;
    let backup = Backup::new(source, &mut b)?;
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        cancelled(cancel)?;
        match backup.step(256)? {
            StepResult::Done => return Ok(()),
            StepResult::More => {}
            _ if std::time::Instant::now() >= deadline => return Err(invalid("数据库备份超时")),
            _ => thread::sleep(Duration::from_millis(10)),
        }
    }
}
struct CheckedWriter<'a, W> {
    output: W,
    crc: crc32fast::Hasher,
    bytes: u64,
    cancel: &'a AtomicBool,
    written: &'a dyn Fn(u64),
}
impl<W: Write> Write for CheckedWriter<'_, W> {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        if self.cancel.load(Ordering::SeqCst) || crate::platform_process::stopping() {
            return Err(std::io::Error::other("cancelled"));
        }
        let n = self.output.write(data)?;
        self.crc.update(&data[..n]);
        self.bytes += n as u64;
        (self.written)(n as u64);
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.output.flush()
    }
}
/// Streams one entry, writing no more than the size it declares (the import's space check counts on that).
/// `written` hears every block written, for the progress the page polls.
fn unpack<W: Write>(
    zip: &mut ZipArchive<BufReader<File>>,
    name: &str,
    mut output: W,
    buffer: &mut [u8],
    cancel: &AtomicBool,
    written: &dyn Fn(u64),
) -> Result<()> {
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
        let mut checked = CheckedWriter { output, crc: crc32fast::Hasher::new(), bytes: 0, cancel, written };
        let decoded = lzma_rs::lzma_decompress(&mut input, &mut checked);
        import_cancelled(cancel)?;
        decoded.map_err(|error| Error::Io(std::io::Error::other(error)))?;
        if checked.bytes != size || checked.crc.finalize() != crc {
            return Err(Error::Io(std::io::Error::other("Invalid checksum")));
        }
    } else {
        let size = raw.size();
        drop(raw);
        let mut source = zip.by_name(name).map_err(zip_failure)?;
        let mut emitted = 0u64;
        loop {
            import_cancelled(cancel)?;
            let n = source.read(buffer).map_err(zip_read_failure)?;
            if n == 0 {
                break;
            }
            emitted += n as u64;
            if emitted > size {
                return Err(Error::Io(std::io::Error::other("归档内容比声明的大小更大")));
            }
            output.write_all(&buffer[..n]).map_err(invalid)?;
            written(n as u64);
        }
    }
    Ok(())
}
/// Runs as a background task (#88): `cancel` is set by the cancel button, a switch or quitting, and an import
/// stopped before publication removes what it unpacked. `observer` hears the stages and the bytes unpacked; the
/// new directory is kept only through its publication point.
pub(crate) fn import(
    archive: &Path,
    cancel: &Arc<AtomicBool>,
    observer: &dyn crate::app_imports::Observer,
) -> Result<(PathBuf, String)> {
    observer.stage("reading");
    let mut zip = ZipArchive::new(BufReader::with_capacity(1 << 20, File::open(archive).map_err(invalid)?))
        .map_err(zip_failure)?;
    let mut names = std::collections::BTreeSet::new();
    let mut declared = 0u64;
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
        // The archive index is keyed by name, so the entries are at most the exported set.
        names.insert(name.to_owned());
        if name == "library.json" && file.size() > CONFIG_LIMIT {
            return Err(invalid("导出文件里的图库配置过大，已拒绝导入"));
        }
        declared = declared.saturating_add(file.size());
    }
    if !names.contains("library.json") || !names.contains("source.sqlite") {
        return Err(invalid("不是导出的图库（缺少 library.json 或 source.sqlite）"));
    }
    // Only the small configuration is buffered; database/features contents are streamed with CRC verification.
    let mut buffer = vec![0u8; 1 << 20];
    let config_text = {
        let mut bytes = Vec::new();
        unpack(&mut zip, "library.json", &mut bytes, &mut buffer, cancel, &|_| ())?;
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
    // Unpacking writes at most the declared sizes; compaction of library.sqlite may need as much again.
    let library_size = zip.index_for_name("library.sqlite").map_or(Ok(0), |i| zip.by_index_raw(i).map(|f| f.size()));
    let needed = declared.saturating_add(library_size.map_err(zip_failure)?).saturating_add(SPACE_MARGIN);
    let available =
        crate::platform_filesystem::available_space(path.parent().expect("internal file path must have a parent"))
            .map_err(invalid)?;
    if needed > available {
        return Err(invalid(format!(
            "磁盘空间不足：导入需要约 {needed} MiB，可用 {available} MiB",
            needed = needed.div_ceil(1 << 20),
            available = available >> 20
        )));
    }
    import_cancelled(cancel)?;
    fs::create_dir(&path).map_err(invalid)?;
    let mut cleanup = Stage(path.clone(), false);
    observer.stage("unpacking");
    observer.count(0, declared);
    let unpacked = std::cell::Cell::new(0u64);
    let written = |n: u64| {
        unpacked.set(unpacked.get() + n);
        observer.count(unpacked.get(), declared);
    };
    for name in names {
        let out = path.join(&name);
        fs::create_dir_all(out.parent().expect("internal file path must have a parent")).map_err(invalid)?;
        let mut file = BufWriter::with_capacity(
            1 << 20,
            OpenOptions::new().write(true).create_new(true).open(out).map_err(invalid)?,
        );
        unpack(&mut zip, &name, &mut file, &mut buffer, cancel, &written)?;
        file.flush().map_err(invalid)?;
    }
    // Old exports can still contain credentials and the sender's lasting consent.
    // Strip them before publishing or registering this newly created package.
    let database = path.join("library.sqlite");
    observer.stage("cleaning");
    if database.try_exists()? {
        sanitize_database(&database, Some(cancel.clone())).map_err(|e| match import_cancelled(cancel) {
            Err(stop) => stop,
            Ok(()) => e,
        })?;
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
    observer.stage("checking");
    // Publish only a package that opens: a damaged database or an incomplete configuration is refused here and
    // the new directory removed, instead of registering a library that then fails to open.
    if fmt_is_current(&fmt) {
        opens(&path).map_err(|e| invalid(format!("导入的图库无法打开，已取消导入：{e}")))?;
    }
    import_cancelled(cancel)?;
    observer.stage("publishing");
    // The publication point: a cancellation that came first keeps the new directory out; one that comes during
    // it waits, and the import then counts as done.
    observer.publish(&mut || {
        import_cancelled(cancel)?;
        cleanup.1 = true;
        Ok(())
    })?;
    Ok((path, name))
}
/// The archive declares the current package format (older ones keep their explicit upgrade path).
fn fmt_is_current(fmt: &Json) -> bool {
    match fmt {
        Json::Integer(v) => v.parse::<i64>().is_ok_and(|v| v == crate::app_library::FORMAT),
        Json::Scalar(serde_json::Value::Number(v)) => v.as_i64() == Some(crate::app_library::FORMAT),
        _ => false,
    }
}
/// The same validation as opening, including both integrity checks. A package whose core still needs the
/// explicit upgrade is valid input: it is checked the way that upgrade will check it.
fn opens(path: &Path) -> Result<()> {
    match Library::open(path) {
        Ok(_) => Ok(()),
        Err(Error::Schema(_)) => Library::open_for_upgrade(path).map(|_| ()),
        Err(e) => Err(e),
    }
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
                    ("error", e.reason().chars().take(200).collect::<String>().into()),
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
    fn exported_databases_are_one_snapshot_while_both_keep_changing() {
        let root =
            std::env::temp_dir().join(format!("illustash-archive-snapshot-{}", crate::store::new_key().unwrap()));
        fs::create_dir_all(root.join("folder")).unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--ignored", "--exact", "app_archives::tests::snapshot_fixture"])
            .env("ILLUSTASH_ARCHIVE_FIXTURE", &root)
            .env("ILLUSTASH_APP_SUPPORT", root.join("support"))
            .env("ILLUSTASH_SHARED", root.join("absent-reference"))
            .env("ILLUSTASH_NO_KEYCHAIN", "1")
            .status()
            .unwrap();
        fs::remove_dir_all(&root).unwrap();
        assert!(status.success());
    }
    #[test]
    #[ignore = "isolated fixture, run with synthetic support by the parent test"]
    fn snapshot_fixture() {
        // #23: a writer commits each update to the source and then the application database, as sync does.
        let root = PathBuf::from(std::env::var_os("ILLUSTASH_ARCHIVE_FIXTURE").expect("isolated fixture only"));
        let package = root.join("synthetic.pilib");
        crate::app_create::create_folder(&package, &root.join("folder"), None, None, None).unwrap();
        let library = Library::open(&package).unwrap();
        for db in [library.source(), library.db()] {
            crate::sqlite::open(&db)
                .unwrap()
                .execute_batch(
                    "CREATE TABLE epoch(n INTEGER); INSERT INTO epoch VALUES (0); CREATE TABLE bulk(x BLOB);
                     WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<1500) INSERT INTO bulk SELECT randomblob(4096) FROM n;",
                )
                .unwrap();
        }
        let stop = Arc::new(AtomicBool::new(false));
        let writer = {
            let (source, app, stop) = (library.source(), library.db(), stop.clone());
            thread::spawn(move || {
                let (s, a) = (crate::sqlite::open(&source).unwrap(), crate::sqlite::open(&app).unwrap());
                let mut n = 0;
                while !stop.load(Ordering::SeqCst) {
                    n += 1;
                    s.execute("UPDATE epoch SET n=?", [n]).unwrap();
                    a.execute("UPDATE epoch SET n=?", [n]).unwrap();
                    thread::sleep(Duration::from_millis(40));
                }
                n
            })
        };
        let dest = root.join("export.zip");
        let mut outcomes = Vec::new();
        for _ in 0..3 {
            let _ = fs::remove_file(&dest);
            outcomes.push(export(&library, &dest, &Arc::new(AtomicBool::new(false))).map(|_| {
                let mut zip = ZipArchive::new(File::open(&dest).unwrap()).unwrap();
                let mut epoch = |name: &str| {
                    let copy = root.join(format!("check-{name}"));
                    std::io::copy(&mut zip.by_name(name).unwrap(), &mut File::create(&copy).unwrap()).unwrap();
                    let n: i64 =
                        crate::sqlite::open(&copy).unwrap().query_row("SELECT n FROM epoch", [], |r| r.get(0)).unwrap();
                    fs::remove_file(copy).unwrap();
                    n
                };
                (epoch("source.sqlite"), epoch("library.sqlite"))
            }));
        }
        stop.store(true, Ordering::SeqCst);
        assert!(writer.join().unwrap() > 0);
        // One instant of the pair: never the application ahead of the source, nor the source a whole update ahead.
        for outcome in outcomes {
            match outcome {
                Ok((source, app)) => assert!(app <= source && source <= app + 1, "source {source}, app {app}"),
                Err(Error::Invalid(m)) => assert!(m.contains("一直在写入"), "{m}"),
                Err(e) => panic!("{e}"),
            }
        }
    }

    #[test]
    fn legacy_database_without_settings_preserves_sparse_rowids() {
        let work = stage(&std::env::temp_dir()).unwrap();
        let path = work.0.join("library.sqlite");
        let c = crate::sqlite::open(&path).unwrap();
        c.execute_batch("CREATE TABLE human(value BLOB); INSERT INTO human(rowid,value) VALUES (37,x'00ff');").unwrap();
        drop(c);
        sanitize_database(&path, None).unwrap();
        let c = crate::sqlite::open(&path).unwrap();
        let row: (i64, Vec<u8>) =
            c.query_row("SELECT rowid,value FROM human", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(row, (37, vec![0, 255]));
        assert_eq!(
            c.query_row("SELECT count(*) FROM sqlite_schema WHERE type='index'", [], |r| r.get::<_, i64>(0)).unwrap(),
            0
        );
    }

    #[test]
    fn sanitization_honors_export_cancellation_inside_sqlite() {
        let work = stage(&std::env::temp_dir()).unwrap();
        let path = work.0.join("library.sqlite");
        let c = crate::sqlite::open(&path).unwrap();
        c.execute_batch("CREATE TABLE human(value); WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<20000) INSERT INTO human SELECT x FROM n;").unwrap();
        drop(c);
        let error = sanitize_database(&path, Some(Arc::new(AtomicBool::new(true)))).unwrap_err();
        assert!(matches!(error, Error::Sql(_)), "{error}");
    }

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
        let error = import(&path, &Arc::default(), &crate::app_imports::Silent).unwrap_err().to_string();
        fs::remove_file(&path).unwrap();
        assert!(error.contains("不认识的内容"), "{error}");
    }
}
