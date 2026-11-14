//! 最近图库登记表。列出是只读的；记住图库是会话成功之后的显式操作。
use crate::{
    app_library::Library,
    app_paths::{app_support, user_home},
    http_json::Json,
    Error, Result,
};
use serde_json::{json, Value};
use std::{
    fs,
    path::{Path, PathBuf},
};

pub(crate) fn info(library: &Library) -> Result<Json> {
    let home = user_home()?;
    info_at(library, &registry(&home), &home.join("Pictures").join("Illustash"))
}

pub(crate) fn remember(library: &Library) -> Result<()> {
    let home = user_home()?;
    remember_at(library.display_path(), library.name(), &registry(&home), &now()?, true)
}

/// 列出一个创建完成的图库，但不把它设为下次启动的图库或当前图库。
pub(crate) fn remember_created(path: &Path, name: &str) -> Result<()> {
    let home = user_home()?;
    remember_at(path, name, &registry(&home), &now()?, false)
}
/// 默认启动时重新打开最后登记的图库包；从不隐式创建或迁移。
pub(crate) fn startup() -> Result<PathBuf> {
    recent(&registry(&user_home()?)).first().and_then(|entry| entry["path"].as_str()).map(PathBuf::from).ok_or_else(
        || Error::Invalid("没有最近图库；请用 --library 指定已初始化的 .pilib，新建请使用 library-create".into()),
    )
}
pub(crate) fn home() -> Result<PathBuf> {
    Ok(user_home()?.join("Pictures").join("Illustash"))
}
/// 新的 Photos 来源指向的 Photos 图库（~/Pictures/Photos Library.photoslibrary）。只是配置。
pub(crate) fn photos_library() -> Result<PathBuf> {
    Ok(home()?.parent().expect("internal file path must have a parent").join("Photos Library.photoslibrary"))
}
pub(crate) fn unique_path(name: &str) -> Result<PathBuf> {
    unique_path_at(&home()?, name)
}
fn unique_path_at(folder: &Path, name: &str) -> Result<PathBuf> {
    let cleaned: String = name.chars().filter(|c| !['/', ':', '\\'].contains(c)).collect();
    let cleaned = crate::http_read::trim(&cleaned);
    let cleaned = if cleaned.is_empty() { "图库" } else { cleaned };
    let mut path = folder.join(format!("{cleaned}.pilib"));
    let mut n = 2;
    while path_exists(&path)? {
        path = folder.join(format!("{cleaned} {n}.pilib"));
        n += 1;
    }
    Ok(path)
}
fn path_exists(path: &Path) -> Result<bool> {
    // 与旧实现（Python 3.12 的 Path.exists）一致：忽略这些错误，但传出 NAME_MAX、权限之类的错误。
    if path.as_os_str().to_string_lossy().contains('\0') {
        return Ok(false);
    }
    match fs::metadata(path) {
        Ok(_) => Ok(true),
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound
                // ErrorKind::NotADirectory 稳定的时间晚于我们的最低版本 Rust 1.82。
                || cfg!(unix) && error.raw_os_error() == Some(libc::ENOTDIR)
                || cfg!(windows) && error.raw_os_error() == Some(267) // ERROR_DIRECTORY
                || matches!(error.raw_os_error(), Some(libc::EBADF | libc::ELOOP)) =>
        {
            Ok(false)
        }
        Err(error) => Err(Error::Invalid(crate::error::filesystem_message(&error, path, None))),
    }
}

/// 不同图库的宿主共用登记表：每次读取、修改、写回都持有它的锁，所以一个宿主的更新从不会被另一个宿主的更新冲掉，
/// 也不会同时经同一个临时文件写入。锁文件本身打不开时，更新照旧进行，并报告它自己的失败。
pub(crate) fn registry_locked<T>(file: &Path, update: impl FnOnce() -> Result<T>) -> Result<T> {
    let lock = file.parent().and_then(|parent| {
        fs::create_dir_all(parent).ok()?;
        fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(file.with_extension("lock"))
            .ok()
    });
    if let Some(lock) = &lock {
        // 另一个宿主只为一次小的写入持有锁；等它，而不是覆盖它的结果。
        for _ in 0..500 {
            if crate::app_file_lock::acquire(lock).is_ok() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
    update()
}

pub(crate) fn forget(path: &Path) -> Result<()> {
    let file = registry(&user_home()?);
    registry_locked(&file, || forget_at(path, &file))
}
fn forget_at(path: &Path, file: &Path) -> Result<()> {
    let mut entries = recent(file);
    entries.retain(|entry| Path::new(entry["path"].as_str().expect("validated internal text field")) != path);
    let text =
        serde_json::to_string_pretty(&json!({"recent":entries})).map_err(|e| Error::Invalid(e.to_string()))? + "\n";
    fs::create_dir_all(file.parent().expect("internal file path must have a parent"))?;
    let tmp = file.with_extension("tmp");
    fs::write(&tmp, text)?;
    fs::rename(tmp, file)?;
    Ok(())
}
fn registry(home: &Path) -> PathBuf {
    app_support(home).join("libraries.json")
}
fn display(path: &Path) -> Result<&str> {
    path.to_str().ok_or_else(|| Error::Invalid("图库路径不是 UTF-8".into()))
}
fn persona(config: &Value) -> Value {
    config.get("test").and_then(|v| v.get("persona")).cloned().unwrap_or(Value::Null)
}
// 与 web.library.open_library 不同，这里不创建缓存目录，也不检查或升级数据库。
// 最近图库卡片可能指向一个有效的配置，而它的数据库仍需显式升级。
fn recent_config(entry: &Value) -> Option<Value> {
    let path = Path::new(entry.get("path")?.as_str()?);
    if path.extension()?.to_str()? != "pilib" {
        return None;
    }
    let config: Value = serde_json::from_str(&fs::read_to_string(path.join("library.json")).ok()?).ok()?;
    if config.get("format")?.as_i64()? > crate::app_library::FORMAT || !config.get("name")?.is_string() {
        return None;
    }
    Some(config)
}
fn recent(path: &Path) -> Vec<Value> {
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return Vec::new();
    };
    value
        .get("recent")
        .and_then(Value::as_array)
        .map(|entries| entries.iter().filter(|entry| recent_config(entry).is_some()).cloned().collect())
        .unwrap_or_default()
}
/// 仍能打开的最近图库，最近的在前：`[{path, name}]`。
pub(crate) fn recent_entries() -> Vec<Value> {
    let Ok(home) = user_home() else { return Vec::new() };
    recent(&registry(&home))
        .into_iter()
        .filter_map(|entry| {
            let config = recent_config(&entry)?;
            Some(serde_json::json!({"path": entry["path"], "name": config["name"]}))
        })
        .collect()
}
fn info_at(library: &Library, file: &Path, home: &Path) -> Result<Json> {
    let mut current = library.info()?;
    current.set("test", persona(library.config()).into());
    let items = recent(file)
        .into_iter()
        .filter_map(|entry| {
            let config = recent_config(&entry)?;
            Some(Json::object([
                ("path", entry["path"].clone().into()),
                ("name", config["name"].clone().into()),
                ("test", persona(&config).into()),
            ]))
        })
        .collect();
    Ok(Json::object([
        ("current", display(library.display_path())?.into()),
        ("info", current),
        ("libraries", Json::Array(items)),
        ("export", Json::object([("status", "idle".into())])),
        ("home", display(home)?.into()),
    ]))
}
fn remember_at(path: &Path, name: &str, file: &Path, opened_at: &str, front: bool) -> Result<()> {
    registry_locked(file, || remember_unlocked(path, name, file, opened_at, front))
}
fn remember_unlocked(path: &Path, name: &str, file: &Path, opened_at: &str, front: bool) -> Result<()> {
    let mut entries = recent(file);
    entries.retain(|entry| Path::new(entry["path"].as_str().expect("validated internal text field")) != path);
    let entry = json!({"path": display(path)?, "name": name, "opened_at": opened_at});
    if front {
        entries.insert(0, entry);
    } else {
        entries.push(entry);
    }
    entries.truncate(20);
    // 保持旧版的对象/数组结构、Unicode、两个空格缩进、结尾换行和 .tmp 文件名。
    let text = serde_json::to_string_pretty(&json!({"recent": entries}))
        .map_err(|e| Error::Invalid(format!("无法序列化图库记录：{e}")))?
        + "\n";
    let parent = file.parent().ok_or_else(|| Error::Invalid("无效图库注册表路径".into()))?;
    // 每个调用方都只说明图库为什么没有列出（#97、#111、#176）：保留操作和它的文件。
    fs::create_dir_all(parent).map_err(|e| crate::error::io_at(e, parent, None))?;
    let tmp = file.with_extension("tmp");
    fs::write(&tmp, text).map_err(|e| crate::error::io_at(e, &tmp, None))?;
    fs::rename(&tmp, file).map_err(|e| crate::error::io_at(e, &tmp, Some(file)))?;
    Ok(())
}
/// 图库为什么没能列进最近图库，给页面的一句话（#179）：哪个文件写不进去，常见情况下还有可能的原因，
/// 而不是内部错误前缀或 errno。
pub(crate) fn unlisted_reason(error: &Error) -> String {
    match error {
        Error::IoAt { source, path, destination } => {
            let file = destination.as_deref().unwrap_or(path);
            let why = if source.kind() == std::io::ErrorKind::PermissionDenied {
                "没有写入权限（文件或目录可能是只读的）".to_owned()
            } else {
                source.to_string()
            };
            format!("最近图库记录（{path}）写不进去：{why}", path = file.display())
        }
        other => other.to_string(),
    }
}

fn now() -> Result<String> {
    // SQLite 已经内置，在两个受支持的平台上都用宿主机的时区和夏令时规则。
    let c = rusqlite::Connection::open_in_memory()?;
    let (stamp, offset): (String, i64) = c.query_row(
        "SELECT strftime('%Y-%m-%dT%H:%M:%S','now','localtime'), unixepoch('now','localtime')-unixepoch('now')",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    Ok(format!(
        "{stamp}{}{:02}:{:02}",
        if offset < 0 { '-' } else { '+' },
        offset.abs() / 3600,
        offset.abs() % 3600 / 60
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "illustash-recent-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn package(&self, name: &str) -> PathBuf {
            let path = self.0.join(format!("{name}.pilib"));
            fs::create_dir(&path).unwrap();
            fs::write(
                path.join("library.json"),
                json!({"format":1,"id":"fixture","name":name,"test":{"persona":"合成人设"}}).to_string(),
            )
            .unwrap();
            path
        }
        fn library(&self) -> Library {
            let path = self.package("current");
            let db = crate::sqlite::open(path.join("library.sqlite")).unwrap();
            db.execute_batch(&format!("CREATE TABLE meta(key TEXT PRIMARY KEY,value TEXT); INSERT INTO meta VALUES ('schema_version','36'),('terms_schema','{v}'),('terms_artists','core'),('terms_labels','core'),('terms_characters','core'),('terms_names','core');", v = crate::SCHEMA_VERSION)).unwrap();
            let source = crate::sqlite::open(path.join("source.sqlite")).unwrap();
            source.execute_batch("CREATE TABLE assets(uuid TEXT,filename TEXT,src_path TEXT,width INTEGER,height INTEGER,added_at TEXT,src_kind TEXT,in_scope INTEGER,trashed INTEGER); CREATE TABLE labels(uuid TEXT,dim TEXT,value TEXT,conf REAL);").unwrap();
            Library::open(&path).unwrap()
        }
        fn registry(&self) -> PathBuf {
            self.0.join("support").join("libraries.json")
        }
        fn write(&self, value: Value) {
            let file = self.registry();
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(file, value.to_string()).unwrap();
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }
    fn value(json: Json) -> Value {
        serde_json::from_str(&json.dump()).unwrap()
    }

    #[test]
    fn listing_does_not_create_registry_or_recent_package_directories() {
        let f = Fixture::new();
        let lib = f.library();
        let other = f.package("needs-upgrade");
        let missing = f.registry();
        let home = f.0.join("Pictures/Illustash");
        let empty = value(info_at(&lib, &missing, &home).unwrap());
        assert_eq!(empty["libraries"], json!([]));
        assert!(!missing.parent().unwrap().exists());
        f.write(json!({"recent":[{"path":other,"name":"stale"}]}));
        let before = fs::read(&missing).unwrap();
        let out = value(info_at(&lib, &missing, &home).unwrap());
        assert_eq!(out["current"], json!(lib.display_path()));
        assert_eq!(out["home"], json!(home));
        assert_eq!(out["export"], json!({"status":"idle"}));
        assert_eq!(out["info"]["test"], "合成人设");
        assert_eq!(out["libraries"], json!([{"path":other,"name":"needs-upgrade","test":"合成人设"}]));
        assert_eq!(fs::read(missing).unwrap(), before);
        assert_eq!(fs::read_dir(other).unwrap().count(), 1);
    }
    #[test]
    fn corrupt_or_missing_recent_is_empty_and_invalid_configs_are_filtered() {
        let f = Fixture::new();
        let lib = f.library();
        let file = f.registry();
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        for text in ["broken", "[]", "{}", "{\"recent\":true}", "{\"recent\":[null,{}]}"] {
            fs::write(&file, text).unwrap();
            assert!(recent(&file).is_empty());
        }
        let valid = f.package("valid");
        let broken = f.package("broken");
        fs::write(broken.join("library.json"), "{").unwrap();
        let newer = f.package("newer");
        fs::write(newer.join("library.json"), r#"{"format":2,"name":"newer"}"#).unwrap();
        f.write(
            json!({"recent":[{"path":valid},{"path":broken},{"path":newer},{"path":f.0.join("missing.pilib")},{}]}),
        );
        assert_eq!(recent(&file).len(), 1);
        remember_at(lib.display_path(), lib.name(), &file, "2026-10-01T12:00:00+08:00", true).unwrap();
        assert_eq!(recent(&file).len(), 2);
    }
    #[test]
    fn remember_front_dedupes_caps_and_atomically_replaces_legacy_registry() {
        let f = Fixture::new();
        let lib = f.library();
        let mut entries = vec![json!({"path":lib.display_path(),"name":"stale","opened_at":"old"})];
        entries.extend((0..24).map(|n| json!({"path":f.package(&format!("recent-{n}")),"extra":true})));
        f.write(json!({"recent":entries}));
        let file = f.registry();
        remember_at(lib.display_path(), lib.name(), &file, "2026-10-01T12:00:00+08:00", true).unwrap();
        let text = fs::read_to_string(&file).unwrap();
        assert!(text.ends_with('\n'));
        assert!(text.contains("  \"recent\""));
        let entries = recent(&file);
        assert_eq!(entries.len(), 20);
        assert_eq!(
            entries[0],
            json!({"path":lib.display_path(),"name":"current","opened_at":"2026-10-01T12:00:00+08:00"})
        );
        assert_eq!(entries[1]["extra"], true);
        assert!(!file.with_extension("tmp").exists());
        remember_at(lib.display_path(), lib.name(), &file, "second", true).unwrap();
        assert_eq!(recent(&file).len(), 20);
        assert_eq!(recent(&file)[0]["opened_at"], "second");
    }
    #[test]
    fn created_append_dedupes_without_changing_current_or_front() {
        let f = Fixture::new();
        let lib = f.library();
        let first = f.package("front");
        let second = f.package("second");
        f.write(json!({"recent":[{"path":first,"opened_at":"front"},
            {"path":lib.display_path(),"name":"stale","opened_at":"old"},{"path":second}]}));
        let before_info = value(info_at(&lib, &f.registry(), &f.0).unwrap());
        remember_at(lib.display_path(), lib.name(), &f.registry(), "created", false).unwrap();
        let entries = recent(&f.registry());
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0]["path"], json!(first));
        assert_eq!(entries[0]["opened_at"], "front");
        assert_eq!(entries[1]["path"], json!(second));
        assert_eq!(entries[2], json!({"path":lib.display_path(),"name":"current","opened_at":"created"}));
        let after_info = value(info_at(&lib, &f.registry(), &f.0).unwrap());
        assert_eq!(after_info["current"], before_info["current"]);
        assert_eq!(after_info["info"], before_info["info"]);
        remember_at(lib.display_path(), lib.name(), &f.registry(), "opened", true).unwrap();
        assert_eq!(recent(&f.registry())[0]["path"], json!(lib.display_path()));
    }
    #[test]
    fn append_at_twenty_preserves_legacy_truncation_including_lost_new_card() {
        let f = Fixture::new();
        let lib = f.library();
        let entries: Vec<Value> = (0..20).map(|n| json!({"path":f.package(&format!("recent-{n}"))})).collect();
        f.write(json!({"recent":entries}));
        remember_at(lib.display_path(), lib.name(), &f.registry(), "created", false).unwrap();
        assert_eq!(recent(&f.registry()), entries);
        let file = f.registry();
        assert!(!file.with_extension("tmp").exists());
        fs::create_dir(file.with_extension("tmp")).unwrap();
        let before = fs::read(&file).unwrap();
        assert!(remember_at(lib.display_path(), lib.name(), &file, "created", false).is_err());
        assert_eq!(fs::read(file).unwrap(), before);
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn registry_failures_keep_the_failing_operation_and_its_files() {
        // #179：创建、导入和打开都只说明图库为什么没有列出，来自同一个 IoAt。
        let f = Fixture::new();
        let lib = f.library();
        f.write(json!({"recent":[]}));
        let file = f.registry();
        let tmp = file.with_extension("tmp");
        fs::create_dir(&tmp).unwrap();
        let before = fs::read(&file).unwrap();
        for front in [false, true] {
            let failure = remember_at(lib.display_path(), lib.name(), &file, "created", front).unwrap_err();
            assert!(matches!(&failure, Error::IoAt { path, destination: None, .. } if path == &tmp), "{failure:?}");
        }
        assert_eq!(fs::read(&file).unwrap(), before);
        fs::remove_dir(&tmp).unwrap();
        fs::remove_file(&file).unwrap();
        fs::create_dir(&file).unwrap();
        let keep = file.join("keep");
        fs::write(&keep, b"existing").unwrap();
        let failure = remember_at(lib.display_path(), lib.name(), &file, "created", false).unwrap_err();
        assert!(
            matches!(&failure, Error::IoAt { path, destination: Some(to), .. } if path == &tmp && to == &file),
            "{failure:?}"
        );
        assert!(unlisted_reason(&failure).starts_with(&format!("最近图库记录（{}）写不进去：", file.display())));
        assert_eq!(fs::read(keep).unwrap(), b"existing");
        assert!(tmp.is_file());
    }
    #[test]
    fn created_registry_takes_validated_path_and_name_without_opening_databases() {
        let f = Fixture::new();
        let path = f.package("published-no-session");
        remember_at(&path, "published-no-session", &f.registry(), "created", false).unwrap();
        assert_eq!(recent(&f.registry())[0]["path"], json!(path));
        assert_eq!(fs::read_dir(&path).unwrap().count(), 1);
    }
    #[test]
    fn unique_names_only_clean_legacy_separators_trim_and_count_taken_paths() {
        let f = Fixture::new();
        let folder = f.0.join("never-created");
        assert_eq!(unique_path_at(&folder, " /:\\\t\u{1c} ").unwrap(), folder.join("图库.pilib"));
        assert!(!folder.exists());
        assert_eq!(unique_path_at(&f.0, "  /名:称\\  ").unwrap(), f.0.join("名称.pilib"));
        fs::create_dir(f.0.join("名称.pilib")).unwrap();
        fs::write(f.0.join("名称 2.pilib"), b"existing file").unwrap();
        assert_eq!(unique_path_at(&f.0, "  /名:称\\  ").unwrap(), f.0.join("名称 3.pilib"));
        let ordinary_file = f.0.join("ordinary-file");
        fs::write(&ordinary_file, b"synthetic").unwrap();
        assert_eq!(unique_path_at(&ordinary_file, "child").unwrap(), ordinary_file.join("child.pilib"));
        // 失效的符号链接按不存在处理（沿用旧实现的 Path.exists）；之后独占创建会拒绝它。
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(f.0.join("missing"), f.0.join("dangling.pilib")).unwrap();
            assert_eq!(unique_path_at(&f.0, "dangling").unwrap(), f.0.join("dangling.pilib"));
        }
    }
    #[cfg(unix)]
    #[test]
    fn aliases_keep_display_identity_and_are_not_canonical_deduped() {
        let f = Fixture::new();
        let lib = f.library();
        let alias = f.0.join("alias.pilib");
        std::os::unix::fs::symlink(lib.display_path(), &alias).unwrap();
        let aliased = Library::open(&alias).unwrap();
        remember_at(lib.display_path(), lib.name(), &f.registry(), "original", true).unwrap();
        remember_at(aliased.display_path(), aliased.name(), &f.registry(), "alias", true).unwrap();
        let entries = recent(&f.registry());
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["path"], json!(alias));
        let out = value(info_at(&aliased, &f.registry(), &f.0).unwrap());
        assert_eq!(out["current"], json!(alias));
        assert_eq!(out["info"]["path"], json!(alias));
    }
    #[test]
    fn write_failure_is_reported_and_existing_registry_survives() {
        let f = Fixture::new();
        let lib = f.library();
        f.write(json!({"recent":[]}));
        let file = f.registry();
        let before = fs::read(&file).unwrap();
        fs::create_dir(file.with_extension("tmp")).unwrap();
        assert!(remember_at(lib.display_path(), lib.name(), &file, "now", true).is_err());
        assert_eq!(fs::read(file).unwrap(), before);
        let blocker = f.0.join("not-a-directory");
        fs::write(&blocker, "x").unwrap();
        assert!(remember_at(lib.display_path(), lib.name(), &blocker.join("libraries.json"), "now", true).is_err());
    }
    #[test]
    fn unlisted_reason_names_the_registry_file_without_internal_prefixes() {
        // #179：页面说明哪个文件写不进去以及原因，而不是「读取失败」或旧实现那种 errno 文字。
        let f = Fixture::new();
        let lib = f.library();
        f.write(json!({"recent":[]}));
        let file = f.registry();
        fs::create_dir(file.with_extension("tmp")).unwrap();
        let error = remember_at(lib.display_path(), lib.name(), &file, "now", false).unwrap_err();
        let reason = unlisted_reason(&error);
        assert!(reason.starts_with("最近图库记录（") && reason.contains("写不进去："), "{reason}");
        assert!(reason.contains("libraries.tmp"), "the failing write is the temporary file: {reason}");
        for prefix in ["读取失败", "不合法", "[Errno"] {
            assert!(!reason.contains(prefix), "{reason}");
        }
        let denied = crate::error::io_at(
            std::io::Error::from(std::io::ErrorKind::PermissionDenied),
            &file.with_extension("tmp"),
            Some(&file),
        );
        let reason = unlisted_reason(&denied);
        assert!(
            reason.contains("libraries.json") && reason.ends_with("没有写入权限（文件或目录可能是只读的）"),
            "{reason}"
        );
        assert_eq!(unlisted_reason(&Error::Invalid("无效图库注册表路径".into())), "不合法：无效图库注册表路径");
    }
    #[test]
    fn concurrent_hosts_never_lose_each_others_registry_updates() {
        // 每个线程代表一个不同图库的宿主，同时写共用的登记表。
        let f = Fixture::new();
        let file = f.registry();
        let names: Vec<String> = (0..8).map(|i| format!("library-{i}")).collect();
        let paths: Vec<PathBuf> = names.iter().map(|name| f.package(name)).collect();
        std::thread::scope(|scope| {
            for (name, path) in names.iter().zip(&paths) {
                let file = &file;
                scope.spawn(move || {
                    for round in 0..10 {
                        remember_at(path, name, file, &format!("round-{round}"), true).unwrap();
                    }
                });
            }
        });
        let written: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
        let mut seen = written["recent"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["name"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        seen.sort();
        assert_eq!(seen, names);
        assert!(!file.with_extension("tmp").exists());
    }
    #[test]
    fn timestamp_has_legacy_local_iso_seconds_shape() {
        let stamp = now().unwrap();
        assert_eq!(stamp.len(), 25);
        for (i, c) in [(4, '-'), (7, '-'), (10, 'T'), (13, ':'), (16, ':'), (22, ':')] {
            assert_eq!(stamp.as_bytes()[i], c as u8);
        }
        assert!(matches!(stamp.as_bytes()[19], b'+' | b'-'));
        // 对生产入口做类型检查，不碰用户的环境和登记表。
        let _ = info as fn(&Library) -> Result<Json>;
        let _ = remember as fn(&Library) -> Result<()>;
        let _ = remember_created as fn(&Path, &str) -> Result<()>;
        let _ = home as fn() -> Result<PathBuf>;
        let _ = unique_path as fn(&str) -> Result<PathBuf>;
    }
}
