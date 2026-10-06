//! Explicit fresh-only folder/Photos package factory. No registry, session switch, scanning or existing-library upgrade.
use crate::{Error, Result};
use rusqlite::{types::ValueRef, Connection, OpenFlags, TransactionBehavior};
use serde_json::{json, Map, Value};
use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

const APPLICATION: &str = include_str!("app_create_schema.sql");
const SOURCE: &str = include_str!("app_source_schema.sql");

pub fn create_folder(
    dest: &Path,
    folder: &Path,
    name: Option<&str>,
    preset: Option<&str>,
    reference: Option<&Path>,
) -> Result<Value> {
    create(dest, folder, name, preset, reference, |_| Ok(()))
}
/// Describe the Photos source without opening, resolving or validating its platform-owned files.
pub fn create_photos(dest: &Path, photos: &Path, name: Option<&str>, reference: Option<&Path>) -> Result<Value> {
    let dest = absolute(dest)?;
    let name = name
        .filter(|v| !v.is_empty())
        .or_else(|| dest.file_stem().and_then(|v| v.to_str()))
        .ok_or_else(|| Error::Invalid("图库名称不是 UTF-8".into()))?;
    let sources = json!([{"id":"s1","kind":"photos","path":text(photos)?,
        "apps":crate::origin::photos_defaults()}]);
    create_package(&dest, name, sources, reference, |_| Ok(()))
}
fn absolute(path: &Path) -> Result<PathBuf> {
    Ok(if path.is_absolute() { path.to_owned() } else { std::env::current_dir()?.join(path) })
}
fn text(path: &Path) -> Result<&str> {
    path.to_str().ok_or_else(|| Error::Invalid("图库路径不是 UTF-8".into()))
}
fn create<F: FnOnce(&Connection) -> Result<()>>(
    dest: &Path,
    folder: &Path,
    name: Option<&str>,
    preset: Option<&str>,
    reference: Option<&Path>,
    after_birth: F,
) -> Result<Value> {
    let dest = absolute(dest)?;
    if dest.extension().and_then(|s| s.to_str()) != Some("pilib") {
        return Err(Error::Invalid("图库包的扩展名必须是 .pilib".into()));
    }
    let folder = absolute(folder)?;
    if !folder.is_dir() {
        return Err(Error::Invalid(format!("不是文件夹：{}", folder.display())));
    }
    let folder = crate::app_paths::canonical(&folder).map_err(|e| crate::error::io_at(e, &folder, None))?;
    // The origin is what the user says; without one it is 「其他」, never guessed from the folder name.
    let explicit_preset = preset.is_some();
    let preset = match preset {
        Some(value) if crate::origin::find(value).is_some() => value.to_owned(),
        Some(_) => return Err(Error::Invalid("来源预设只能是 x、pixiv、other".into())),
        None => "other".to_owned(),
    };
    let name = name
        .filter(|v| !v.is_empty())
        .or_else(|| folder.file_name().and_then(|v| v.to_str()))
        .filter(|v| !v.is_empty())
        .or_else(|| dest.file_stem().and_then(|v| v.to_str()))
        .ok_or_else(|| Error::Invalid("图库名称不是 UTF-8".into()))?;
    let sources = json!([{"id":"s1","kind":"folder","path":text(&folder)?,"preset":preset,
        "preset_origin":if explicit_preset {"user"} else {"auto"}}]);
    create_package(&dest, name, sources, reference, after_birth)
}
/// Only an absent optional reference selects the empty built-in schema. Broken links,
/// directories and permission failures remain errors rather than silently losing the index.
pub(crate) fn optional_reference(reference: &Path) -> Result<Option<&Path>> {
    match fs::symlink_metadata(reference) {
        Ok(_) => Ok(Some(reference)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(crate::error::io_at(error, reference, None)),
    }
}
/// One source of a library being created: a folder with the origin the user gave it, or Photos.
pub enum NewSource {
    Folder { path: PathBuf, preset: String },
    Photos { path: PathBuf },
}
/// A folder becomes a source only if it exists, has a known origin and neither contains nor sits inside
/// another folder source (the same file must not belong to two sources).
pub(crate) fn folder_source(id: &str, folder: &Path, preset: &str, others: &[PathBuf]) -> Result<(PathBuf, Value)> {
    let folder = absolute(folder)?;
    if !folder.is_dir() {
        return Err(Error::Invalid(format!("不是文件夹：{}", folder.display())));
    }
    let folder = crate::app_paths::canonical(&folder).map_err(|e| crate::error::io_at(e, &folder, None))?;
    if crate::origin::find(preset).is_none() {
        return Err(Error::Invalid("来源预设只能是 x、pixiv、other".into()));
    }
    if let Some(other) = others.iter().find(|other| crate::app_paths::overlaps(&folder, other)) {
        return Err(Error::Invalid(format!(
            "{} 与已添加的文件夹 {} 重叠：同一个文件夹不能添加两次，也不能和它的上级或下级文件夹同时添加",
            folder.display(),
            other.display()
        )));
    }
    let entry = json!({"id":id,"kind":"folder","path":text(&folder)?,"preset":preset,"preset_origin":"user"});
    Ok((folder, entry))
}
/// A library with one or more sources, ids s1, s2, … in the given order. At most one Photos source.
pub fn create_sources(dest: &Path, name: &str, sources: Vec<NewSource>, reference: Option<&Path>) -> Result<Value> {
    let dest = absolute(dest)?;
    if sources.is_empty() {
        return Err(Error::Invalid("请至少添加一个来源".into()));
    }
    let (mut folders, mut entries, mut photos) = (Vec::new(), Vec::new(), false);
    for (i, source) in sources.into_iter().enumerate() {
        let id = format!("s{}", i + 1);
        match source {
            NewSource::Folder { path, preset } => {
                let (folder, entry) = folder_source(&id, &path, &preset, &folders)?;
                folders.push(folder);
                entries.push(entry);
            }
            NewSource::Photos { path } => {
                if std::mem::replace(&mut photos, true) {
                    return Err(Error::Invalid("照片图库只能添加一次".into()));
                }
                entries
                    .push(json!({"id":id,"kind":"photos","path":text(&path)?,"apps":crate::origin::photos_defaults()}));
            }
        }
    }
    create_package(&dest, name, Value::Array(entries), reference, |_| Ok(()))
}
fn create_package<F: FnOnce(&Connection) -> Result<()>>(
    dest: &Path,
    name: &str,
    sources: Value,
    reference: Option<&Path>,
    after_birth: F,
) -> Result<Value> {
    if dest.extension().and_then(|s| s.to_str()) != Some("pilib") {
        return Err(Error::Invalid("图库包的扩展名必须是 .pilib".into()));
    }
    if let Some(reference) = reference {
        if !reference.is_file() {
            return Err(Error::Invalid(format!("缺少参考库 {}", reference.display())));
        }
    }
    // Exclusive acquisition: existing directories, files, and even dangling symlinks never enter cleanup.
    // Attempt the final operand first, matching pathlib.mkdir(parents=True)'s error path.
    // Recursively creating it directly would accept an already existing directory.
    match fs::create_dir(dest) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent).map_err(|e| crate::error::io_at(e, parent, None))?;
            }
            fs::create_dir(dest).map_err(|e| crate::error::io_at(e, dest, None))?;
        }
        Err(error) => return Err(crate::error::io_at(error, dest, None)),
    }
    let result = (|| {
        for sub in ["features", "thumbnails", "backups"] {
            let path = dest.join(sub);
            fs::create_dir(&path).map_err(|e| crate::error::io_at(e, &path, None))?;
        }
        initialize_source(&dest.join("source.sqlite"), reference)?;
        if sources.as_array().is_some_and(|items| items.iter().any(|item| item["kind"] == "photos")) {
            let c = crate::sqlite::open_with_flags(dest.join("source.sqlite"), OpenFlags::SQLITE_OPEN_READ_ONLY)?;
            c.prepare("SELECT z_pk FROM assets LIMIT 0")
                .map_err(|e| Error::Invalid(format!("参考库结构不支持照片同步：{e}")))?;
        }
        initialize_application(&dest.join("library.sqlite"), &dest.join("source.sqlite"), after_birth)?;
        let c = Connection::open_in_memory()?;
        let (created, offset): (String, i64) = c.query_row(
            "SELECT strftime('%Y-%m-%dT%H:%M:%S','now','localtime'),unixepoch('now','localtime')-unixepoch('now')",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let created = format!(
            "{created}{}{:02}:{:02}",
            if offset < 0 { '-' } else { '+' },
            offset.abs() / 3600,
            offset.abs() % 3600 / 60
        );
        let config = json!({"format":1,"id":crate::store::new_key()?.to_ascii_uppercase(),"name":name,"created_at":created,
            "sources":sources});
        // library.json is the discoverability marker. Publish only after both databases are complete and closed.
        let temp = dest.join("library.json.tmp");
        let sources = serde_json::to_string_pretty(&config["sources"])
            .map_err(|e| Error::Invalid(e.to_string()))?
            .replace('\n', "\n  ");
        // Keep the existing config field order without changing serde_json's global map representation.
        let bytes = format!("{{\n  \"format\": 1,\n  \"id\": {},\n  \"name\": {},\n  \"created_at\": {},\n  \"sources\": {sources}\n}}\n", config["id"], config["name"], config["created_at"]);
        fs::write(&temp, bytes).map_err(|e| crate::error::io_at(e, &temp, None))?;
        let config_path = dest.join("library.json");
        fs::rename(&temp, &config_path).map_err(|e| crate::error::io_at(e, &temp, Some(&config_path)))?;
        Ok(json!({"path":text(dest)?,"name":name}))
    })();
    if let Err(error) = result {
        // Every SQLite connection above is scoped to the failed closure and is closed before removing files.
        if let Err(cleanup) = fs::remove_dir_all(dest) {
            return Err(Error::Invalid(format!("新图库创建失败：{error}；清理本次新目录失败：{cleanup}")));
        }
        return Err(error);
    }
    result
}
fn quote_id(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}
fn file_uri(path: &Path) -> Result<String> {
    let mut uri = String::from("file:");
    for byte in text(&crate::sqlite::path(path))?.bytes() {
        if byte.is_ascii_alphanumeric() || b"/-._~".contains(&byte) {
            uri.push(byte as char);
        } else {
            uri.push_str(&format!("%{byte:02X}"));
        }
    }
    Ok(uri + "?mode=ro")
}
fn initialize_source(path: &Path, reference: Option<&Path>) -> Result<()> {
    let mut c = crate::sqlite::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE | OpenFlags::SQLITE_OPEN_URI,
    )?;
    c.busy_timeout(Duration::from_secs(20))?;
    if let Some(reference) = reference {
        let reference = crate::app_paths::canonical(reference).map_err(|e| crate::error::io_at(e, reference, None))?;
        c.execute("ATTACH DATABASE ? AS ref", [file_uri(&reference)?])?;
    }
    let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if reference.is_some() {
        let tables: Vec<(String, String)> = tx
            .prepare("SELECT name,sql FROM ref.sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        for (name, sql) in tables {
            tx.execute_batch(&sql)?;
            if ["characters", "character_aliases", "runs"].contains(&name.as_str()) {
                let name = quote_id(&name);
                tx.execute_batch(&format!("INSERT INTO main.{name} SELECT * FROM ref.{name}"))?;
            }
        }
        let indices: Vec<String> = tx
            .prepare("SELECT sql FROM ref.sqlite_master WHERE type='index' AND sql IS NOT NULL")?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        for sql in indices {
            tx.execute_batch(&sql.replacen("CREATE INDEX ", "CREATE INDEX IF NOT EXISTS ", 1))?;
        }
    } else {
        tx.execute_batch(include_str!("app_source_empty.sql"))?;
    }
    let columns: Vec<String> =
        tx.prepare("PRAGMA table_info(assets)")?.query_map([], |r| r.get(1))?.collect::<rusqlite::Result<_>>()?;
    for column in ["source_id", "preset"] {
        if !columns.iter().any(|v| v == column) {
            tx.execute_batch(&format!("ALTER TABLE assets ADD COLUMN {column} TEXT"))?;
        }
    }
    tx.execute_batch(SOURCE)?;
    // Fail before publication if the supplied reference cannot support current source reads/imports.
    for sql in [
        "SELECT uuid,filename,src_path,width,height,added_at,src_kind,in_scope,trashed,hidden,stable_hash,orig_size,source_id,preset FROM assets LIMIT 0",
        "SELECT uuid,dim,value,conf,source,model_ver,run_id FROM labels LIMIT 0",
        "SELECT uuid,dim,value,note FROM decisions LIMIT 0",
        "SELECT * FROM runs LIMIT 0",
        "SELECT uuid,model,vec FROM embeddings LIMIT 0",
        "SELECT char_id,wd_tag,name_zh,name_ja,name_en FROM characters LIMIT 0",
        "SELECT char_id,alias FROM character_aliases LIMIT 0",
        "SELECT asset,system,key,source_id,current FROM asset_links LIMIT 0",
        "SELECT source_id,path,size,mtime_ns,md5,asset FROM files LIMIT 0",
    ] {
        tx.prepare(sql).map_err(|e| Error::Invalid(format!("参考库结构不支持当前读取与同步：{e}")))?;
    }
    tx.commit()?;
    if reference.is_some() {
        c.execute("DETACH DATABASE ref", [])?;
    }
    check(&c)?;
    Ok(())
}
pub(crate) fn names_triggers(c: &Connection) -> Result<()> {
    c.execute("INSERT OR IGNORE INTO meta VALUES ('names_version','0')", [])?;
    for table in ["entities", "links", "names", "display_names"] {
        if !c.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?)", [table], |r| {
            r.get::<_, bool>(0)
        })? {
            continue;
        }
        for event in ["INSERT", "UPDATE", "DELETE"] {
            c.execute_batch(&format!("CREATE TRIGGER IF NOT EXISTS names_version_{table}_{} AFTER {event} ON {table}\n                        BEGIN UPDATE meta SET value=CAST(value AS INTEGER)+1 WHERE key='names_version'; END",event.to_ascii_lowercase()))?;
        }
    }
    Ok(())
}
fn source_index(c: &Connection) -> Result<Value> {
    let mut query = c.prepare("SELECT * FROM characters")?;
    let columns: Vec<String> = query.column_names().iter().map(|v| (*v).to_owned()).collect();
    let rows = query
        .query_map([], |r| {
            let mut obj = Map::new();
            for (i, key) in columns.iter().enumerate() {
                let value = match r.get_ref(i)? {
                    ValueRef::Null => Value::Null,
                    ValueRef::Integer(v) => v.into(),
                    ValueRef::Real(v) => v.into(),
                    ValueRef::Text(v) => std::str::from_utf8(v)
                        .map_err(|e| {
                            rusqlite::Error::FromSqlConversionFailure(i, rusqlite::types::Type::Text, Box::new(e))
                        })?
                        .into(),
                    ValueRef::Blob(_) => {
                        return Err(rusqlite::Error::InvalidColumnType(i, key.clone(), rusqlite::types::Type::Blob))
                    }
                };
                obj.insert(key.clone(), value);
            }
            Ok(Value::Object(obj))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let aliases = c
        .prepare("SELECT char_id,alias FROM character_aliases")?
        .query_map([], |r| Ok(json!([r.get::<_, Option<String>>(0)?, r.get::<_, Option<String>>(1)?])))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let tag = if columns.iter().any(|v| v == "danbooru_tag") {
        "coalesce(nullif(danbooru_tag,''),char_id)"
    } else {
        "char_id"
    };
    let tags = c
        .prepare(&format!("SELECT char_id,{tag} FROM characters"))?
        .query_map([], |r| Ok(json!([r.get::<_, Option<String>>(0)?, r.get::<_, Option<String>>(1)?])))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(json!({"characters":rows,"aliases":aliases,"tags":tags,"new_ids":{}}))
}
fn initialize_application<F: FnOnce(&Connection) -> Result<()>>(
    path: &Path,
    source: &Path,
    after_birth: F,
) -> Result<()> {
    let mut c = crate::open(path)?;
    c.pragma_update(None, "journal_mode", "WAL")?;
    let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute_batch(APPLICATION)?;
    tx.commit()?;
    crate::migrate(&mut c)?;
    crate::legacy_artists::cutover(&mut c, None)?;
    crate::legacy_labels::cutover(&mut c)?;
    crate::legacy_characters::cutover(&mut c)?;
    crate::legacy_names::cutover(&mut c)?;
    names_triggers(&c)?;
    after_birth(&c)?;
    drop(c);
    let src = crate::sqlite::open_with_flags(source, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let index = source_index(&src)?;
    drop(src);
    let clock = Connection::open_in_memory()?;
    let stamp: String = clock.query_row("SELECT strftime('%Y-%m-%d %H:%M:%S','now','localtime')", [], |r| r.get(0))?;
    crate::app_service::character_request(
        path,
        &json!({"op":"refresh","index":index,"assets":[],"labels":[],"decisions":[],"duplicates":[],"album_map":{},"indexed":stamp,"rating_indexed":stamp,"general_indexed":stamp,"realistic":[],"finalize_labels":true}),
    )?;
    let c = crate::sqlite::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    check(&c)?;
    Ok(())
}
fn check(c: &Connection) -> Result<()> {
    let result: String = c.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
    if result != "ok" {
        return Err(Error::Invalid("新图库数据库完整性检查失败".into()));
    }
    Ok(())
}

pub(crate) fn creation_error(error: Error, reference: &Path) -> Error {
    match error {
        Error::Invalid(message) if message == format!("缺少参考库 {}", reference.display()) => {
            Error::Invalid(format!("{message}（python -m web.library export-reference）"))
        }
        Error::IoAt { source, path, destination } => {
            Error::Invalid(crate::error::filesystem_message(&source, &path, destination.as_deref()))
        }
        Error::Io(source) => Error::Invalid(source.to_string()),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Fixture {
        root: PathBuf,
        folder: PathBuf,
        reference: PathBuf,
        dest: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "illustash-create-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&root).unwrap();
            let folder = root.join("pixiv-downloads");
            fs::create_dir(&folder).unwrap();
            // Windows 文件名不允许 `?`；那里仍保留引号与 `#` 两个 URI 敏感字符。
            #[cfg(unix)]
            let reference = root.join("reference '?#.sqlite");
            #[cfg(windows)]
            let reference = root.join("reference '#.sqlite");
            let c = crate::sqlite::open(&reference).unwrap();
            c.execute_batch("CREATE TABLE assets(uuid TEXT PRIMARY KEY,filename TEXT,src_path TEXT,width INTEGER,height INTEGER,added_at TEXT,src_kind TEXT,in_scope INTEGER DEFAULT 1,trashed INTEGER,hidden INTEGER,stable_hash TEXT,orig_size INTEGER,z_pk INTEGER); CREATE TABLE embeddings(uuid TEXT,model TEXT,vec BLOB,PRIMARY KEY(uuid,model)); CREATE TABLE labels(uuid TEXT,dim TEXT,value TEXT,conf REAL,source TEXT DEFAULT 'legacy',model_ver TEXT,run_id TEXT); CREATE TABLE decisions(uuid TEXT,dim TEXT,value TEXT,note TEXT); CREATE TABLE runs(run_id TEXT PRIMARY KEY,dim TEXT); CREATE TABLE characters(char_id TEXT PRIMARY KEY,danbooru_tag TEXT,wd_tag TEXT,copyright TEXT,base_tag TEXT,skin TEXT,name_zh TEXT,name_ja TEXT,name_en TEXT); CREATE TABLE character_aliases(char_id TEXT,alias TEXT,kind TEXT); CREATE INDEX reference_alias_idx ON character_aliases(alias); CREATE TABLE reference_extra(value TEXT); CREATE INDEX reference_extra_idx ON reference_extra(value);
             INSERT INTO assets VALUES ('not-imported','fixture.png','/synthetic-only',10,20,'2026-10-01','orig',1,0,0,NULL,NULL,NULL);
             INSERT INTO labels(uuid,dim,value,conf) VALUES ('not-imported','general','blue_hair',0.9);
             INSERT INTO decisions VALUES ('not-imported','character','alice','album:synthetic pk=1');
             INSERT INTO reference_extra VALUES ('not-copied'); INSERT INTO runs VALUES ('reference-run','character');
             INSERT INTO characters VALUES ('alice','alice_tag','alice_tag','Example','alice',NULL,'爱丽丝',NULL,'Alice'),('alice_swim','alice_swim_tag','alice_swim_tag','Example','alice','swimsuit',NULL,NULL,'Alice swim'),('doodle_sensei_(blue_archive)','doodle_sensei_(blue_archive)',NULL,'blue_archive',NULL,NULL,NULL,NULL,'Doodle Sensei');
             INSERT INTO character_aliases VALUES ('alice','alias alice','index');").unwrap();
            drop(c);
            let dest = root.join("synthetic.pilib");
            Self { root, folder, reference, dest }
        }
        fn create(&self) -> Result<Value> {
            create_folder(&self.dest, &self.folder, None, None, Some(&self.reference))
        }
        fn db(&self) -> Connection {
            crate::sqlite::open(self.dest.join("library.sqlite")).unwrap()
        }
        fn source(&self) -> Connection {
            crate::sqlite::open(self.dest.join("source.sqlite")).unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.root).unwrap();
        }
    }
    fn count(c: &Connection, sql: &str) -> i64 {
        c.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn built_in_source_is_empty_and_supports_folder_scan() {
        let f = Fixture::new();
        fs::remove_file(&f.reference).unwrap();
        create_folder(&f.dest, &f.folder, None, None, None).unwrap();
        let source = f.source();
        for table in ["assets", "labels", "decisions", "runs", "embeddings", "characters", "character_aliases"] {
            assert_eq!(count(&source, &format!("SELECT count(*) FROM {table}")), 0);
        }
        assert!(!f.reference.exists());
        // Factory stays scan-free; scanner accepts its schema without test-time alterations.
        fs::write(f.folder.join("synthetic.png"), b"synthetic invalid image").unwrap();
        let source_config = json!({"id":"s1","path":f.folder,"preset":"other"});
        let scan = crate::app_folder::scan(
            &source_config,
            &f.dest.join("source.sqlite"),
            &|| crate::app_sync::check(&std::sync::atomic::AtomicBool::new(false)),
            &|_, _| {},
        )
        .unwrap();
        assert_eq!(scan.counts["new"], 1);
    }

    #[test]
    fn photos_source_is_configuration_only_and_preserves_explicit_path() {
        let f = Fixture::new();
        // A dangling link proves this factory does not require/read/resolve a Photos source.
        let photos = f.root.join("Photos Library.photoslibrary");
        #[cfg(unix)]
        std::os::unix::fs::symlink(f.root.join("missing-photos"), &photos).unwrap();
        let before = fs::read(&f.reference).unwrap();
        assert_eq!(
            create_photos(&f.dest, &photos, None, Some(&f.reference)).unwrap(),
            json!({"path":f.dest,"name":"synthetic"})
        );
        let config: Value = serde_json::from_slice(&fs::read(f.dest.join("library.json")).unwrap()).unwrap();
        assert_eq!(
            config["sources"],
            json!([{"id":"s1","kind":"photos","path":photos,
            "apps":{"com.atebits.Tweetie2":"x","net.pxv.iphone":"pixiv"}}])
        );
        assert_eq!(count(&f.source(), "SELECT count(*) FROM assets"), 0);
        assert_eq!(count(&f.db(), "SELECT count(*) FROM assets"), 0);
        assert_eq!(count(&f.db(), "SELECT count(*) FROM meta WHERE key LIKE 'terms_%' AND value='core'"), 4);
        assert_eq!(before, fs::read(&f.reference).unwrap());
        assert!(!f.root.join("missing-photos").exists());
    }
    #[test]
    fn photos_failure_never_removes_preexisting_destination_or_source() {
        let f = Fixture::new();
        fs::create_dir(&f.dest).unwrap();
        fs::write(f.dest.join("keep"), b"existing package").unwrap();
        assert!(create_photos(&f.dest, &f.folder, None, Some(&f.reference)).is_err());
        assert_eq!(fs::read(f.dest.join("keep")).unwrap(), b"existing package");
        let dest = f.root.join("failed.pilib");
        let c = crate::sqlite::open(&f.reference).unwrap();
        c.execute_batch("DROP TABLE characters").unwrap();
        drop(c);
        assert!(create_photos(&dest, &f.folder, None, Some(&f.reference)).is_err());
        assert!(!dest.exists());
        assert!(f.dest.join("keep").is_file());
        assert!(f.folder.is_dir());
        assert!(f.reference.is_file());
    }

    #[test]
    fn fresh_package_is_complete_and_core_verified_without_importing_reference_assets() {
        let f = Fixture::new();
        let before = fs::read(&f.reference).unwrap();
        let result = f.create().unwrap();
        assert_eq!(result, json!({"path":f.dest,"name":"pixiv-downloads"}));
        let config: Value = serde_json::from_slice(&fs::read(f.dest.join("library.json")).unwrap()).unwrap();
        assert_eq!(config["format"], 1);
        assert_eq!(
            config["sources"][0],
            json!({"id":"s1","kind":"folder","path":crate::app_paths::canonical(&f.folder).unwrap(),"preset":"other","preset_origin":"auto"})
        );
        let id = config["id"].as_str().unwrap();
        assert_eq!(id.len(), 36);
        assert_eq!(id, id.to_ascii_uppercase());
        assert_eq!(id.as_bytes()[14], b'4');
        for sub in ["features", "thumbnails", "backups"] {
            assert!(f.dest.join(sub).is_dir());
        }
        assert!(!f.dest.join("library.json.tmp").exists());
        let mut c = f.db();
        assert_eq!(count(&c,"SELECT count(*) FROM meta WHERE key IN ('terms_artists','terms_labels','terms_characters','terms_names') AND value='core'"),4);
        assert_eq!(
            count(
                &c,
                "SELECT count(*) FROM meta WHERE key IN ('schema_version','terms_schema') AND value IN ('36','7')"
            ),
            2
        );
        assert_eq!(
            count(
                &c,
                "SELECT count(*) FROM meta WHERE key IN ('indexed','rating_indexed','general_indexed','names_version')"
            ),
            4
        );
        assert_eq!(count(&c, "SELECT count(*) FROM meta WHERE key='identity_policy'"), 0);
        assert_eq!(count(&c, "SELECT count(*) FROM actions"), 0);
        assert_eq!(count(&c, "SELECT count(*) FROM assets"), 0);
        assert_eq!(count(&c, "SELECT count(*) FROM prefs"), 0);
        assert_eq!(count(&c, "SELECT count(*) FROM backend_effects"), 0);
        assert_eq!(
            count(&c, "SELECT count(*) FROM display_names WHERE kind='work' AND lang='zh' AND source='builtin'"),
            22
        );
        assert_eq!(
            count(&c, "SELECT count(*) FROM sqlite_master WHERE type='trigger' AND name LIKE 'names_version_%'"),
            12
        );
        assert_eq!(count(&c, "SELECT count(*) FROM links WHERE system='ccip'"), 3);
        assert_eq!(count(&c, "SELECT count(*) FROM entities WHERE parent_id IS NOT NULL AND variant='swimsuit'"), 1);
        assert_eq!(count(&c, "SELECT count(*) FROM entities WHERE mascot=1 AND mascot_origin='default'"), 1);
        assert!(crate::legacy_characters::verify(&mut c).unwrap().committed);
        assert!(crate::legacy_names::verify(&mut c).unwrap().committed);
        let src = f.source();
        for table in ["assets", "labels", "decisions", "reference_extra"] {
            assert_eq!(count(&src, &format!("SELECT count(*) FROM {table}")), 0);
        }
        assert_eq!(count(&src, "SELECT count(*) FROM characters"), 3);
        assert_eq!(count(&src, "SELECT count(*) FROM runs"), 1);
        assert_eq!(count(&src,"SELECT count(*) FROM sqlite_master WHERE type='index' AND name IN ('reference_alias_idx','reference_extra_idx')"),2);
        src.prepare("SELECT source_id,preset FROM assets").unwrap();
        src.prepare("SELECT * FROM asset_links").unwrap();
        src.prepare("SELECT * FROM files").unwrap();
        assert_eq!(before, fs::read(&f.reference).unwrap());
    }
    #[test]
    fn all_27_native_reads_work_on_fresh_empty_package() {
        let f = Fixture::new();
        f.create().unwrap();
        let context = crate::http::LibraryContext {
            db: f.dest.join("library.sqlite"),
            generation: "synthetic".into(),
            library: Some(f.dest.clone()),
            source: Some(f.dest.join("source.sqlite")),
            tags: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tags/tags_v3.csv"),
            token: Some("synthetic-token".into()),
            read_only: None,
        };
        let id: String = f
            .db()
            .query_row("SELECT entity_id FROM links WHERE system='ccip' AND key='alice'", [], |r| r.get(0))
            .unwrap();
        for route in crate::http_read::CORE_ROUTES {
            let target = match *route {
                "/api/character/profile" => format!("{route}?entity={id}"),
                "/api/character/stats" => format!("{route}?id={id}"),
                _ => (*route).to_owned(),
            };
            let out = crate::http_read::read(&context, &target).unwrap_or_else(|e| panic!("{route}: {e}"));
            assert!(out.is_some(), "{route}");
            let expected = if *route == "/api/detail" { 400 } else { 200 };
            assert_eq!(out.unwrap().1, expected, "{route}");
        }
    }
    #[test]
    fn explicit_preset_and_name_are_retained() {
        let f = Fixture::new();
        create_folder(&f.dest, &f.folder, Some("合成新图库"), Some("x"), Some(&f.reference)).unwrap();
        let config: Value = serde_json::from_slice(&fs::read(f.dest.join("library.json")).unwrap()).unwrap();
        assert_eq!(config["name"], "合成新图库");
        assert_eq!(config["sources"][0]["preset"], "x");
        assert_eq!(config["sources"][0]["preset_origin"], "user");
    }
    #[test]
    fn unicode_preset_nullable_reference_and_nested_destination_match_existing_contract() {
        let mut f = Fixture::new();
        f.dest = f.root.join("new/parent/nested.pilib");
        let c = crate::sqlite::open(&f.reference).unwrap();
        c.execute("INSERT INTO character_aliases VALUES (NULL,NULL,NULL)", []).unwrap();
        c.execute("UPDATE characters SET danbooru_tag=NULL WHERE char_id='alice'", []).unwrap();
        drop(c);
        f.create().unwrap();
        let bytes = fs::read_to_string(f.dest.join("library.json")).unwrap();
        assert!(bytes.starts_with("{\n  \"format\": 1,\n  \"id\":"));
        assert!(f.dest.join("library.sqlite").is_file());
    }
    #[test]
    fn existing_directory_and_file_are_never_cleaned_or_overwritten() {
        for directory in [true, false] {
            let f = Fixture::new();
            if directory {
                fs::create_dir(&f.dest).unwrap();
                fs::write(f.dest.join("keep"), b"existing").unwrap();
            } else {
                fs::write(&f.dest, b"existing").unwrap();
            }
            assert!(f.create().is_err());
            assert_eq!(fs::read(if directory { f.dest.join("keep") } else { f.dest.clone() }).unwrap(), b"existing");
        }
    }
    #[cfg(unix)]
    #[test]
    fn dangling_or_existing_symlink_destination_is_never_touched() {
        for dangling in [true, false] {
            let f = Fixture::new();
            let target = f.root.join("symlink-target");
            if !dangling {
                fs::create_dir(&target).unwrap();
                fs::write(target.join("keep"), b"existing").unwrap();
            }
            std::os::unix::fs::symlink(&target, &f.dest).unwrap();
            assert!(f.create().is_err());
            assert_eq!(fs::read_link(&f.dest).unwrap(), target);
            if !dangling {
                assert_eq!(fs::read(target.join("keep")).unwrap(), b"existing");
            }
        }
    }
    #[test]
    fn bad_input_or_reference_failure_preserves_source_folder_and_removes_only_new_package() {
        let f = Fixture::new();
        fs::write(f.folder.join("keep"), b"source").unwrap();
        assert!(create_folder(&f.dest, &f.folder, None, Some("bad"), Some(&f.reference)).is_err());
        assert!(!f.dest.exists());
        assert!(create_folder(&f.dest, &f.folder, None, None, Some(&f.root.join("missing.sqlite"))).is_err());
        assert!(!f.dest.exists());
        let invalid = f.root.join("invalid.sqlite");
        fs::write(&invalid, b"not sqlite").unwrap();
        assert!(create_folder(&f.dest, &f.folder, None, None, Some(&invalid)).is_err());
        assert!(!f.dest.exists());
        assert_eq!(fs::read(invalid).unwrap(), b"not sqlite");
        let incomplete = f.root.join("incomplete.sqlite");
        let c = crate::sqlite::open(&incomplete).unwrap();
        c.execute_batch("CREATE TABLE assets(uuid TEXT);").unwrap();
        drop(c);
        let before = fs::read(&incomplete).unwrap();
        assert!(create_folder(&f.dest, &f.folder, None, None, Some(&incomplete)).is_err());
        assert!(!f.dest.exists());
        assert_eq!(fs::read(incomplete).unwrap(), before);
        assert_eq!(fs::read(f.folder.join("keep")).unwrap(), b"source");
    }
    #[test]
    fn failure_after_core_cutovers_does_not_publish_config_and_cleans_owned_databases_and_backups() {
        let f = Fixture::new();
        let before = fs::read(&f.reference).unwrap();
        let result = create(&f.dest, &f.folder, None, None, Some(&f.reference), |c| {
            assert!(!f.dest.join("library.json").exists());
            assert_eq!(count(c,"SELECT count(*) FROM meta WHERE key IN ('terms_artists','terms_labels','terms_characters','terms_names') AND value='core'"),4);
            assert!(f.dest.join("library-backups").is_dir());
            c.execute_batch(
                "CREATE TRIGGER injected BEFORE INSERT ON entities BEGIN SELECT RAISE(ABORT,'injected'); END;",
            )?;
            Ok(())
        });
        assert!(result.is_err());
        assert!(!f.dest.exists());
        assert_eq!(before, fs::read(&f.reference).unwrap());
        assert!(f.folder.is_dir());
    }
    #[test]
    fn active_wal_reference_is_read_without_altering_committed_input() {
        let f = Fixture::new();
        let c = crate::sqlite::open(&f.reference).unwrap();
        c.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; INSERT INTO character_aliases VALUES ('alice','wal alias','index');").unwrap();
        let wal = PathBuf::from(format!("{}-wal", f.reference.display()));
        let before = fs::read(&wal).unwrap();
        f.create().unwrap();
        assert_eq!(count(&f.source(), "SELECT count(*) FROM character_aliases"), 2);
        assert_eq!(before, fs::read(wal).unwrap());
    }
}
