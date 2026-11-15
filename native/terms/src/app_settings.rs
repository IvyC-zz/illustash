//! 图库包设置和显式的应用数据库备份。偏好设置从不授予联网许可，也不启动任务。
use crate::{app_library::Library, character_json, http_json::Json, Error, Result};
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior};
use serde::{
    de::{MapAccess, SeqAccess, Visitor},
    Deserialize, Deserializer,
};
use serde_json::Value;
use std::{
    fmt, fs,
    path::{Path, PathBuf},
    time::{Duration, UNIX_EPOCH},
};

const KEYS: [&str; 5] = ["sync", "similar", "index_check", "names", "provenance"];
/// 已移除的偏好项（#100：本地识别上限）。已存的值会被忽略，并在下次保存时去掉。
const REMOVED: [&str; 1] = ["local_limit"];

// 持久化的设置保留已存的键顺序（插入顺序）。标量编码复用持久化的序列化器；HTTP 的有序表示只在构造回复时生成，
// 从不用来序列化已存的偏好设置。
#[derive(Clone, Debug)]
pub(crate) enum SettingValue {
    Scalar(Value),
    Array(Vec<Self>),
    Object(Vec<(String, Self)>),
}
impl SettingValue {
    fn object() -> Self {
        Self::Object(Vec::new())
    }
    pub(crate) fn set(&mut self, key: String, value: Self) {
        if let Self::Object(fields) = self {
            if let Some((_, v)) = fields.iter_mut().find(|(k, _)| k == &key) {
                *v = value;
            } else {
                fields.push((key, value));
            }
        }
    }
    fn persisted(&self) -> String {
        match self {
            Self::Scalar(value) => character_json::dump(value, true),
            Self::Array(values) => format!("[{}]", values.iter().map(Self::persisted).collect::<Vec<_>>().join(", ")),
            Self::Object(fields) => format!(
                "{{{}}}",
                fields
                    .iter()
                    .map(|(k, v)| format!(
                        "{}: {}",
                        character_json::dump(&Value::String(k.clone()), true),
                        v.persisted()
                    ))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }
    pub(crate) fn response(self) -> Json {
        match self {
            Self::Scalar(v) => v.into(),
            Self::Array(values) => Json::Array(values.into_iter().map(Self::response).collect()),
            Self::Object(fields) => Json::object(fields.into_iter().map(|(k, v)| (k, v.response()))),
        }
    }
}
impl<'de> Deserialize<'de> for SettingValue {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        struct Ordered;
        impl<'de> Visitor<'de> for Ordered {
            type Value = SettingValue;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                write!(f, "JSON settings")
            }
            fn visit_unit<E>(self) -> std::result::Result<Self::Value, E> {
                Ok(SettingValue::Scalar(Value::Null))
            }
            fn visit_bool<E>(self, v: bool) -> std::result::Result<Self::Value, E> {
                Ok(SettingValue::Scalar(v.into()))
            }
            fn visit_i64<E>(self, v: i64) -> std::result::Result<Self::Value, E> {
                Ok(SettingValue::Scalar(v.into()))
            }
            fn visit_u64<E>(self, v: u64) -> std::result::Result<Self::Value, E> {
                Ok(SettingValue::Scalar(v.into()))
            }
            fn visit_f64<E>(self, v: f64) -> std::result::Result<Self::Value, E> {
                Ok(SettingValue::Scalar(v.into()))
            }
            fn visit_str<E>(self, v: &str) -> std::result::Result<Self::Value, E> {
                Ok(SettingValue::Scalar(v.into()))
            }
            fn visit_string<E>(self, v: String) -> std::result::Result<Self::Value, E> {
                Ok(SettingValue::Scalar(v.into()))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> std::result::Result<Self::Value, A::Error> {
                let mut values = Vec::new();
                while let Some(v) = a.next_element()? {
                    values.push(v);
                }
                Ok(SettingValue::Array(values))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> std::result::Result<Self::Value, A::Error> {
                let mut value = SettingValue::object();
                while let Some((k, v)) = a.next_entry()? {
                    value.set(k, v);
                }
                Ok(value)
            }
        }
        d.deserialize_any(Ordered)
    }
}
impl serde::Serialize for SettingValue {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::{SerializeMap, SerializeSeq};
        match self {
            Self::Scalar(value) => value.serialize(serializer),
            Self::Array(values) => {
                let mut seq = serializer.serialize_seq(Some(values.len()))?;
                for v in values {
                    seq.serialize_element(v)?;
                }
                seq.end()
            }
            Self::Object(fields) => {
                let mut map = serializer.serialize_map(Some(fields.len()))?;
                for (k, v) in fields {
                    map.serialize_entry(k, v)?;
                }
                map.end()
            }
        }
    }
}
pub(crate) fn config_from_text(text: &str) -> Result<SettingValue> {
    let value: SettingValue = serde_json::from_str(text).map_err(|e| Error::Invalid(e.to_string()))?;
    if !matches!(value, SettingValue::Object(_)) {
        return Err(Error::Invalid("无效图库配置".into()));
    }
    Ok(value)
}
pub(crate) fn write_config(path: &Path, value: &SettingValue) -> Result<()> {
    let text = serde_json::to_string_pretty(value).map_err(|e| Error::Invalid(e.to_string()))? + "\n";
    let tmp = path.join("library.json.tmp");
    fs::write(&tmp, text)?;
    fs::rename(tmp, path.join("library.json"))?;
    Ok(())
}
pub(crate) fn preset(library: &mut Library, source_id: &str, preset: &str) -> Result<Json> {
    if crate::origin::find(preset).is_none() {
        return Err(Error::Invalid("来源预设只能是 x、pixiv、other".into()));
    }
    let mut config = config_from_text(&fs::read_to_string(library.instance_path().join("library.json"))?)?;
    let SettingValue::Object(fields) = &mut config else { unreachable!() };
    let source = fields
        .iter_mut()
        .find(|(k, _)| k == "sources")
        .and_then(|(_, v)| if let SettingValue::Array(a) = v { Some(a) } else { None })
        .and_then(|a| {
            a.iter_mut().find(|s| {
                let v = (**s).clone().response();
                v.get("id").is_some_and(|v| v.text() == source_id)
                    && v.get("kind").is_some_and(|v| v.text() == "folder")
            })
        })
        .ok_or_else(|| Error::Invalid("没有这个文件夹来源".into()))?;
    source.set("preset".into(), SettingValue::Scalar(preset.into()));
    source.set("preset_origin".into(), SettingValue::Scalar("user".into()));
    let response = source.clone().response();
    // 与现有的先写配置的边界一致。SQLite 失败时留下一个明确、可重试的出处设置。
    write_config(library.instance_path(), &config)?;
    library.reload_config()?;
    set_asset_preset(library, source_id, preset)?;
    Ok(response)
}
/// 来源下的每张图片都采用来源的出处。
pub(crate) fn set_asset_preset(library: &Library, source_id: &str, preset: &str) -> Result<()> {
    let mut conn = crate::sqlite::open_with_flags(library.source(), OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    conn.busy_timeout(Duration::from_secs(30))?;
    let tx = conn.transaction()?;
    let columns: Vec<String> =
        tx.prepare("PRAGMA table_info(assets)")?.query_map([], |r| r.get(1))?.collect::<rusqlite::Result<_>>()?;
    for column in ["source_id", "preset"] {
        if !columns.iter().any(|c| c == column) {
            tx.execute_batch(&format!("ALTER TABLE assets ADD COLUMN {column} TEXT"))?;
        }
    }
    tx.execute_batch(include_str!("app_source_schema.sql"))?;
    tx.execute("UPDATE assets SET preset=?1 WHERE source_id=?2", [preset, source_id])?;
    tx.commit()?;
    Ok(())
}
fn connection(lib: &Library, write: bool) -> Result<Connection> {
    // 不用 CREATE：被移除的图库绝不能被新数据库悄悄顶替。
    let conn = crate::sqlite::open_with_flags(
        lib.db(),
        if write { OpenFlags::SQLITE_OPEN_READ_WRITE } else { OpenFlags::SQLITE_OPEN_READ_ONLY },
    )?;
    conn.busy_timeout(Duration::from_secs(20))?;
    Ok(conn)
}
fn load_startup(conn: &Connection) -> Result<SettingValue> {
    let mut result = SettingValue::object();
    for key in KEYS {
        result.set(key.into(), SettingValue::Scalar(true.into()));
    }
    let stored: Option<String> =
        conn.query_row("SELECT value FROM prefs WHERE key='startup'", [], |r| r.get(0)).optional()?;
    if let Some(stored) = stored {
        let saved: SettingValue =
            serde_json::from_str(&stored).map_err(|e| Error::Invalid(format!("无效 startup 设置：{e}")))?;
        let SettingValue::Object(fields) = saved else {
            return Err(Error::Invalid("无效 startup 设置".into()));
        };
        for (key, value) in fields {
            if !REMOVED.contains(&key.as_str()) {
                result.set(key, value);
            }
        }
    }
    Ok(result)
}
pub(crate) fn startup(lib: &Library) -> Result<Json> {
    Ok(load_startup(&connection(lib, false)?)?.response())
}

/// 只提交偏好设置。会话、任务和联网授权归调用它的应用会话管。
/// 接收原始请求体，以保留按第一个无效键报错的先后顺序（沿用旧实现）。
pub(crate) fn set_startup_json(lib: &Library, body: &str) -> Result<()> {
    let Json::Object(changes) = crate::http_json::parse_request(body)? else {
        return Err(Error::Invalid("未知设置".into()));
    };
    if changes.is_empty() || changes.iter().any(|(key, _)| !KEYS.contains(&key.as_str())) {
        return Err(Error::Invalid("未知设置".into()));
    }
    let mut validated = Vec::new();
    for (key, value) in changes {
        let value = if let Json::Scalar(value @ Value::Bool(_)) = value {
            value
        } else {
            return Err(Error::Invalid("开关取值不正确".into()));
        };
        validated.push((key, SettingValue::Scalar(value)));
    }
    update_pref(lib, "startup", |c| {
        let mut merged = load_startup(c)?;
        for (key, value) in validated {
            merged.set(key, value);
        }
        Ok(merged.persisted())
    })
}
/// 用 `change` 根据当前状态生成的文本替换一个偏好项。读取前先拿写入锁，所以并发的部分更新不会互相覆盖丢失。
/// 偏好设置从不授予联网，也不启动任务。
pub(crate) fn update_pref(lib: &Library, key: &str, change: impl FnOnce(&Connection) -> Result<String>) -> Result<()> {
    let mut conn = connection(lib, true)?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    crate::schema::require_current(&tx)?;
    let value = change(&tx)?;
    tx.execute("INSERT OR REPLACE INTO prefs(key,value) VALUES (?1,?2)", [key, &value])?;
    tx.commit()?;
    Ok(())
}
#[cfg(test)]
fn set_startup(lib: &Library, changes: &Value) -> Result<()> {
    set_startup_json(lib, &serde_json::to_string(changes).unwrap())
}
fn path_text(path: &Path) -> Result<String> {
    path.to_str().map(str::to_owned).ok_or_else(|| Error::Invalid("图库路径不是 UTF-8".into()))
}
fn bytes(path: &Path) -> Result<u64> {
    let mut wal = path.as_os_str().to_owned();
    wal.push("-wal");
    let mut total = 0u64;
    for path in [path, Path::new(&wal)] {
        match fs::metadata(path) {
            Ok(m) => total = total.checked_add(m.len()).ok_or_else(|| Error::Invalid("数据库尺寸溢出".into()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(total)
}
pub(crate) fn info(lib: &Library, forced_off: &[String]) -> Result<Json> {
    let mut thumb_bytes = 0u64;
    let mut thumb_files = 0u64;
    match fs::read_dir(lib.thumbnails()) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry?;
                if entry.path().extension().is_some_and(|s| s == "jpg") {
                    thumb_bytes += entry.metadata()?.len();
                    thumb_files += 1;
                }
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    let mut backups = Vec::new();
    match fs::read_dir(lib.backups()) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry?;
                let name = entry.file_name();
                let name = name.to_str().ok_or_else(|| Error::Invalid("备份名称不是 UTF-8".into()))?;
                if name.starts_with("library-") && name.ends_with(".sqlite") {
                    backups.push((name.to_owned(), entry.path()));
                }
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    backups.sort_by(|a, b| b.0.cmp(&a.0));
    let backup_count = backups.len();
    let mut listed = Vec::new();
    for (name, path) in backups.into_iter().take(20) {
        let meta = fs::metadata(path)?;
        let modified = meta.modified()?;
        let time = match modified.duration_since(UNIX_EPOCH) {
            Ok(d) => d.as_secs_f64(),
            Err(e) => -e.duration().as_secs_f64(),
        };
        listed.push(Json::object([
            ("name", name.into()),
            ("bytes", Value::from(meta.len()).into()),
            ("time", Json::Float(time)),
        ]));
    }
    let mut forced_off = forced_off.to_vec();
    forced_off.sort();
    forced_off.dedup();
    Ok(Json::object([
        ("package", lib.info()?),
        (
            "source",
            Json::object([
                ("path", path_text(&lib.source())?.into()),
                ("bytes", Value::from(bytes(&lib.source())?).into()),
            ]),
        ),
        (
            "library",
            Json::object([("path", path_text(&lib.db())?.into()), ("bytes", Value::from(bytes(&lib.db())?).into())]),
        ),
        (
            "thumbnails",
            Json::object([
                ("path", path_text(&lib.thumbnails())?.into()),
                ("bytes", Value::from(thumb_bytes).into()),
                ("files", Value::from(thumb_files).into()),
            ]),
        ),
        ("backups", Json::Array(listed)),
        ("backup_count", Value::from(backup_count).into()),
        ("startup", startup(lib)?),
        ("forced_off", Json::Array(forced_off.into_iter().map(Into::into).collect())),
        ("tasks", Json::object([("similar", Json::object([("status", "off".into())]))])),
    ]))
}

/// 只备份 library.sqlite，包括已提交的 WAL 数据，通过 SQLite 的一致读快照进行。
/// 这个显式操作可以创建 backups/；设置的 GET 从不创建。保留的名字不会覆盖已有备份。
pub fn backup(lib: &Library) -> Result<PathBuf> {
    let source = connection(lib, false)?;
    let stamp: String = source.query_row("SELECT strftime('%Y%m%d-%H%M%S','now','localtime')", [], |r| r.get(0))?;
    fs::create_dir_all(lib.backups())?;
    let dest = loop {
        let mut random = [0u8; 3];
        getrandom::fill(&mut random).map_err(|e| Error::Invalid(format!("无法生成备份名称：{e}")))?;
        let dest =
            lib.backups().join(format!("library-{stamp}-{:02x}{:02x}{:02x}.sqlite", random[0], random[1], random[2]));
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        crate::platform_filesystem::private_file_options(&mut options);
        match options.open(&dest) {
            Ok(file) => {
                drop(file);
                break dest;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    };
    // 清理只限于本次调用成功占下的那个文件。
    let copied = (|| -> Result<()> {
        let mut target = crate::sqlite::open_with_flags(&dest, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
        target.busy_timeout(Duration::from_secs(20))?;
        {
            let copy = rusqlite::backup::Backup::new(&source, &mut target)?;
            use rusqlite::backup::StepResult;
            loop {
                match copy.step(100)? {
                    StepResult::Done => break,
                    StepResult::More => continue,
                    StepResult::Busy => {
                        return Err(rusqlite::Error::SqliteFailure(
                            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
                            None,
                        )
                        .into())
                    }
                    StepResult::Locked => {
                        return Err(rusqlite::Error::SqliteFailure(
                            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_LOCKED),
                            None,
                        )
                        .into())
                    }
                    _ => return Err(Error::Invalid("备份未完成".into())),
                }
            }
        }
        drop(target);
        let check = crate::sqlite::open_with_flags(&dest, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let result: String = check.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
        if result != "ok" {
            return Err(Error::Invalid("备份完整性检查失败，已停止迁移".into()));
        }
        Ok(())
    })();
    if let Err(error) = copied {
        let _ = fs::remove_file(&dest);
        return Err(error);
    }
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Fixture {
        root: PathBuf,
        lib: Library,
    }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "illustash-settings-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let path = root.join("synthetic.pilib");
            fs::create_dir_all(&path).unwrap();
            fs::write(path.join("library.json"), r#"{"format":1,"id":"synthetic","name":"合成"}"#).unwrap();
            let db = crate::sqlite::open(path.join("library.sqlite")).unwrap();
            db.execute_batch(&format!("CREATE TABLE meta(key TEXT PRIMARY KEY,value TEXT); INSERT INTO meta VALUES ('schema_version','36'),('terms_schema','{v}'),('terms_artists','core'),('terms_labels','core'),('terms_characters','core'),('terms_names','core'); CREATE TABLE prefs(key TEXT PRIMARY KEY,value TEXT NOT NULL); CREATE TABLE actions(id INTEGER PRIMARY KEY,label TEXT); CREATE TABLE evidence(value TEXT);", v = crate::SCHEMA_VERSION)).unwrap();
            drop(db);
            let source = crate::sqlite::open(path.join("source.sqlite")).unwrap();
            source.execute_batch("CREATE TABLE assets(uuid TEXT,filename TEXT,src_path TEXT,width INTEGER,height INTEGER,added_at TEXT,src_kind TEXT,in_scope INTEGER,trashed INTEGER);CREATE TABLE labels(uuid TEXT,dim TEXT,value TEXT,conf REAL);").unwrap();
            drop(source);
            let lib = Library::open(&path).unwrap();
            Self { root, lib }
        }
        fn stored(&self) -> Option<String> {
            connection(&self.lib, false)
                .unwrap()
                .query_row("SELECT value FROM prefs WHERE key='startup'", [], |r| r.get(0))
                .optional()
                .unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
    #[test]
    fn ordinary_settings_get_has_no_side_effects_and_preserves_contract() {
        let f = Fixture::new();
        let before = fs::read(f.lib.db()).unwrap();
        let output = info(&f.lib, &["sync".into(), "names".into()]).unwrap().dump();
        let parsed: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(parsed["forced_off"], json!(["names", "sync"]));
        assert_eq!(parsed["tasks"], json!({"similar":{"status":"off"}}));
        assert_eq!(
            startup(&f.lib).unwrap().dump(),
            r#"{"sync": true, "similar": true, "index_check": true, "names": true, "provenance": true}"#
        );
        let mut last = 0;
        for key in
            ["package", "source", "library", "thumbnails", "backups", "backup_count", "startup", "forced_off", "tasks"]
        {
            let pos = output.find(&format!("\"{key}\":")).unwrap();
            assert!(pos >= last);
            last = pos;
        }
        assert!(!f.lib.thumbnails().exists());
        assert!(!f.lib.backups().exists());
        assert_eq!(before, fs::read(f.lib.db()).unwrap());
        assert_eq!(f.stored(), None);
    }
    #[test]
    fn startup_validation_matches_existing_errors_without_writes() {
        let f = Fixture::new();
        for (input, message) in [
            (json!({}), "未知设置"),
            (json!(null), "未知设置"),
            (json!([]), "未知设置"),
            (json!({"other":true}), "未知设置"),
            (json!({"sync":1}), "开关取值不正确"),
            (json!({"provenance":null}), "开关取值不正确"),
            (json!({"local_limit":3000}), "未知设置"),
        ] {
            assert!(matches!(set_startup(&f.lib,&input),Err(Error::Invalid(s)) if s==message));
            assert_eq!(f.stored(), None);
        }
    }
    #[test]
    fn raw_body_preserves_first_invalid_key_error_precedence() {
        let f = Fixture::new();
        assert!(
            matches!(set_startup_json(&f.lib, r#"{"sync":1,"names":null}"#), Err(Error::Invalid(v)) if v == "开关取值不正确")
        );
        assert!(
            matches!(set_startup_json(&f.lib, r#"{"sync":1,"other":true}"#), Err(Error::Invalid(v)) if v == "未知设置")
        );
        assert_eq!(f.stored(), None);
    }
    #[test]
    fn startup_updates_are_ordered_atomic_and_do_not_authorize_network() {
        let f = Fixture::new();
        set_startup(&f.lib, &json!({"sync":false,"similar":false})).unwrap();
        assert_eq!(
            f.stored().unwrap(),
            r#"{"sync": false, "similar": false, "index_check": true, "names": true, "provenance": true}"#
        );
        let db = connection(&f.lib, true).unwrap();
        db.execute_batch(
            "CREATE TRIGGER fail_settings BEFORE INSERT ON prefs BEGIN SELECT RAISE(ABORT,'injected'); END;",
        )
        .unwrap();
        drop(db);
        let before = f.stored();
        assert!(set_startup(&f.lib, &json!({"provenance":false})).is_err());
        assert_eq!(f.stored(), before);
        assert_eq!(
            connection(&f.lib, false)
                .unwrap()
                .query_row("SELECT count(*) FROM actions", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    #[test]
    fn stored_nested_order_unicode_and_float_format_are_preserved() {
        let f = Fixture::new();
        let db = connection(&f.lib, true).unwrap();
        db.execute("INSERT INTO prefs VALUES ('startup',?)", [r#"{"names":false,"legacy":{"z":"角色🙂","a":1e-7}}"#])
            .unwrap();
        drop(db);
        set_startup(&f.lib, &json!({"similar":false})).unwrap();
        assert!(f.stored().unwrap().ends_with(r#", "legacy": {"z": "\u89d2\u8272\ud83d\ude42", "a": 1e-07}}"#));
        assert!(startup(&f.lib).unwrap().dump().contains(r#""legacy": {"z": "角色🙂", "a": 1e-07}"#));
    }
    #[test]
    fn removed_local_limit_is_ignored_and_dropped_on_save() {
        let f = Fixture::new();
        let db = connection(&f.lib, true).unwrap();
        db.execute("INSERT INTO prefs VALUES ('startup',?)", [r#"{"sync":false,"local_limit":100}"#]).unwrap();
        drop(db);
        assert_eq!(
            startup(&f.lib).unwrap().dump(),
            r#"{"sync": false, "similar": true, "index_check": true, "names": true, "provenance": true}"#
        );
        set_startup(&f.lib, &json!({"names":false})).unwrap();
        assert!(!f.stored().unwrap().contains("local_limit"));
    }
    #[test]
    fn thumbnail_and_backup_listing_matches_sizes_filter_order_and_limit() {
        let f = Fixture::new();
        fs::create_dir(f.lib.thumbnails()).unwrap();
        fs::write(f.lib.thumbnails().join("a.jpg"), b"123").unwrap();
        fs::write(f.lib.thumbnails().join("ignore.JPG"), b"ignored").unwrap();
        fs::create_dir(f.lib.backups()).unwrap();
        for n in 0..23 {
            fs::write(f.lib.backups().join(format!("library-{n:02}.sqlite")), b"copy").unwrap();
        }
        fs::write(f.lib.backups().join("terms-ignored.sqlite"), b"ignored").unwrap();
        let output: Value = serde_json::from_str(&info(&f.lib, &[]).unwrap().dump()).unwrap();
        assert_eq!(output["thumbnails"]["bytes"], 3);
        assert_eq!(output["thumbnails"]["files"], 1);
        assert_eq!(output["backup_count"], 23);
        assert_eq!(output["backups"].as_array().unwrap().len(), 20);
        assert_eq!(output["backups"][0]["name"], "library-22.sqlite");
    }
    #[test]
    fn explicit_backup_uses_readonly_source_and_includes_live_committed_wal() {
        let f = Fixture::new();
        let writer = connection(&f.lib, true).unwrap();
        writer
            .execute_batch("PRAGMA journal_mode=WAL; INSERT INTO evidence(rowid,value) VALUES (42,'committed-wal');")
            .unwrap();
        assert!(Path::new(&format!("{}-wal", f.lib.db().display())).exists());
        let before = fs::read(f.lib.db()).unwrap();
        let dest = backup(&f.lib).unwrap();
        let name = dest.file_name().unwrap().to_str().unwrap();
        assert_eq!(name.len(), 37);
        assert!(name.starts_with("library-"));
        assert_eq!(&name[16..17], "-");
        assert_eq!(&name[23..24], "-");
        assert!(name[24..30].chars().all(|c| c.is_ascii_hexdigit()));
        let check = crate::sqlite::open_with_flags(dest, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        assert_eq!(
            check.query_row("SELECT value FROM evidence", [], |r| r.get::<_, String>(0)).unwrap(),
            "committed-wal"
        );
        assert_eq!(check.query_row("SELECT rowid FROM evidence", [], |r| r.get::<_, i64>(0)).unwrap(), 42);
        assert_eq!(check.query_row("PRAGMA quick_check", [], |r| r.get::<_, String>(0)).unwrap(), "ok");
        assert_eq!(before, fs::read(f.lib.db()).unwrap());
    }
    #[test]
    fn backup_failure_cleans_own_file_and_preserves_existing_backup() {
        let f = Fixture::new();
        fs::create_dir(f.lib.backups()).unwrap();
        let existing = f.lib.backups().join("library-existing.sqlite");
        fs::write(&existing, b"existing").unwrap();
        let db = connection(&f.lib, false).unwrap();
        let page: usize =
            db.query_row("SELECT rootpage FROM sqlite_master WHERE name='evidence'", [], |r| r.get(0)).unwrap();
        let page_size: usize = db.query_row("PRAGMA page_size", [], |r| r.get(0)).unwrap();
        drop(db);
        let mut bytes = fs::read(f.lib.db()).unwrap();
        bytes[(page - 1) * page_size] = 0; // 坏数据页：打开和 strftime 都能成功；复制出的数据库必须过不了 quick_check。
        fs::write(f.lib.db(), bytes).unwrap();
        assert!(backup(&f.lib).is_err());
        assert_eq!(fs::read(existing).unwrap(), b"existing");
        assert_eq!(fs::read_dir(f.lib.backups()).unwrap().count(), 1);
    }
    #[test]
    fn missing_database_is_not_recreated_by_get_set_or_backup() {
        let f = Fixture::new();
        fs::remove_file(f.lib.db()).unwrap();
        assert!(info(&f.lib, &[]).is_err());
        assert!(set_startup(&f.lib, &json!({"sync":false})).is_err());
        assert!(backup(&f.lib).is_err());
        assert!(!f.lib.db().exists());
        assert!(!f.lib.backups().exists());
    }
}
