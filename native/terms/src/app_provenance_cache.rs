//! Machine cache and evidence delivery. Source/cache/application commits are separate and retryable.
use crate::{app_library::Library, http_json::Json, Error, Result};
use rusqlite::{Connection, OptionalExtension};
use serde_json::{json, Value};
use std::{collections::BTreeSet, path::Path, time::Duration};

pub(crate) fn connect(path: &Path) -> Result<Connection> {
    let c = crate::sqlite::open(path)?;
    c.busy_timeout(Duration::from_secs(2))?;
    c.execute_batch("PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL;")?;
    let version: u32 = c.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version > 5 {
        return Err(Error::Invalid("溯源缓存版本比程序新".into()));
    }
    if version < 5 {
        c.execute_batch(include_str!("app_migration_sql/provenance.sql"))?;
    }
    if !has_column(&c, "ref_sigs", "colour")? {
        c.execute_batch("ALTER TABLE ref_sigs ADD COLUMN colour BLOB")?;
    }
    c.execute_batch("PRAGMA user_version=5; CREATE TABLE IF NOT EXISTS native_delivery(task_id TEXT PRIMARY KEY, package TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'pending', error TEXT NOT NULL DEFAULT '');")?;
    if !has_column(&c, "native_delivery", "error")? {
        c.execute_batch("ALTER TABLE native_delivery ADD COLUMN error TEXT NOT NULL DEFAULT ''")?;
    }
    c.execute_batch("CREATE TABLE IF NOT EXISTS native_meta(key TEXT PRIMARY KEY,value TEXT NOT NULL)")?;
    Ok(c)
}
fn has_column(c: &Connection, table: &str, column: &str) -> Result<bool> {
    Ok(c.prepare(&format!("PRAGMA table_info({table})"))?
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .iter()
        .any(|s| s == column))
}
/// Pixel results describe the bytes they were computed from. Each picture's content hash is remembered, and
/// when its bytes change under the same UUID its lookups, matched posts, attributions and OCR marks are
/// dropped so the next run computes them again; the snapshot delivered after that run replaces the evidence
/// computed from the old bytes. A picture seen for the first time is only remembered. All of these are
/// machine results; verdicts live in the core.
pub(super) fn forget_changed_content(c: &Connection, library: &Library) -> Result<()> {
    let src = crate::app_library::read_connection(&library.source())?;
    // Opening does not require the column; such a source has no content version to compare.
    if src.prepare("SELECT stable_hash FROM assets LIMIT 0").is_err() {
        return Ok(());
    }
    c.execute_batch("CREATE TABLE IF NOT EXISTS content_versions(asset TEXT PRIMARY KEY, hash TEXT NOT NULL)")?;
    let current = src
        .prepare("SELECT uuid,stable_hash FROM assets WHERE coalesce(stable_hash,'')<>''")?
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let seen = c
        .prepare("SELECT asset,hash FROM content_versions")?
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<std::collections::HashMap<_, _>>>()?;
    let tx = c.unchecked_transaction()?;
    for (asset, hash) in current {
        match seen.get(&asset) {
            Some(old) if *old == hash => continue,
            Some(_) => {
                tx.execute("DELETE FROM lookups WHERE target=?", [&asset])?;
                tx.execute("DELETE FROM image_sources WHERE asset=?", [&asset])?;
                tx.execute("DELETE FROM image_artists WHERE asset=?", [&asset])?;
                tx.execute("DELETE FROM marks WHERE asset=?", [&asset])?;
            }
            None => {}
        }
        tx.execute(
            "INSERT INTO content_versions(asset,hash) VALUES (?1,?2) ON CONFLICT(asset) DO UPDATE SET hash=excluded.hash",
            [&asset, &hash],
        )?;
    }
    tx.commit()?;
    Ok(())
}
pub(super) fn backup(from: &Path, to: &Path) -> Result<()> {
    let c = crate::app_library::read_connection(from)?;
    c.backup(rusqlite::DatabaseName::Main, to, None)?;
    Ok(())
}
fn own(id: &str) -> Value {
    json!({"kind":"artist","link":{"system":"provenance","key":id}})
}
fn timestamp(value: &str) -> String {
    chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S")
        .map(|v| v.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_else(|_| chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string())
}
pub(super) fn package(library: &Library, cache: &Path, output: &Path) -> Result<crate::package::Package> {
    let app = crate::app_library::read_connection(&library.db())?;
    let present: BTreeSet<String> = app
        .prepare("SELECT uuid FROM assets ORDER BY uuid")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let c = crate::app_library::read_connection(cache)?;
    let mut evidence = String::new();
    type Row = (String, String, String, Option<f64>, String, String, String);
    let rows = c.prepare("SELECT asset,artist_id,method,score,evidence,status,updated_at FROM image_artists ORDER BY asset,artist_id")?.query_map([],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?)))?.collect::<rusqlite::Result<Vec<Row>>>()?;
    for (asset, artist, method, score, raw, status, updated) in rows {
        if !present.contains(&asset) {
            continue;
        }
        if !["auto", "candidate"].contains(&status.as_str()) {
            return Err(Error::Invalid("溯源结果不能表达人工裁决".into()));
        }
        let mut data =
            serde_json::from_str::<Value>(&raw).ok().filter(Value::is_object).unwrap_or_else(|| json!({"raw":raw}));
        data["status"] = status.into();
        let score = score.filter(|v| v.is_finite());
        let version = format!(
            "v4/{}",
            &crate::strict_json::sha256_hex(
                crate::strict_json::canonical(&json!({"score":score,"data":data})).as_bytes()
            )[..12]
        );
        let metric = match method.as_str() {
            "pixel_hash" => "exact_match".into(),
            "gallery_match" => "gallery_similarity".into(),
            "ocr" => "ocr_confidence".into(),
            _ => format!("{}_score", method.to_lowercase()),
        };
        let row = json!({"id":format!("{asset}:{artist}"),"asset":{"namespace":"library","id":asset},"term":own(&artist),"source":"provenance","method":method,"version":version,"score":score.map(|value|json!({"value":value,"metric":metric})),"observed_at":timestamp(&updated),"data":data});
        evidence += &crate::strict_json::canonical(&row);
        evidence.push('\n');
    }
    for row in c
        .prepare("SELECT id,merged_into FROM artists WHERE merged_into IS NOT NULL ORDER BY id")?
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
    {
        let (a, b) = row?;
        evidence += &crate::strict_json::canonical(
            &json!({"id":format!("merge:{a}"),"relation":{"kind":"same_as","a":own(&a),"b":own(&b)},"source":"provenance","method":"merge","version":"v4","score":null,"observed_at":timestamp(""),"data":{}}),
        );
        evidence.push('\n');
    }
    let mut terms = String::new();
    let ids = c
        .prepare("SELECT id FROM artists ORDER BY id")?
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for id in ids {
        let links = c
            .prepare("SELECT system,key,url FROM artist_links WHERE artist_id=? ORDER BY system,key")?
            .query_map([&id], |r| {
                let url: String = r.get(2)?;
                let mut v = json!({"system":r.get::<_,String>(0)?,"key":r.get::<_,String>(1)?});
                if !url.is_empty() {
                    v["url"] = url.into();
                }
                Ok(v)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let names = c
            .prepare("SELECT source,name FROM artist_names WHERE artist_id=? ORDER BY source")?
            .query_map([&id], |r| Ok(json!({"source":r.get::<_,String>(0)?,"value":r.get::<_,String>(1)?})))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        terms += &crate::strict_json::canonical(&json!({"term":own(&id),"links":links,"names":names}));
        terms.push('\n');
    }
    let manifest = json!({"format":"illustash-evidence/1","library_id":library.id(),"producer":{"name":"provenance","version":"db-v5"},"created_at":timestamp(""),"assets":present.iter().map(|u|json!({"asset":{"namespace":"library","id":u},"status":"completed","error":null})).collect::<Vec<_>>(),"files":{"evidence.jsonl":{"bytes":evidence.len(),"sha256":crate::strict_json::sha256_hex(evidence.as_bytes())},"terms.jsonl":{"bytes":terms.len(),"sha256":crate::strict_json::sha256_hex(terms.as_bytes())}}});
    let text = crate::strict_json::canonical(&manifest);
    let pkg = crate::package::Package::from_texts(&text, &evidence, Some(&terms))?;
    std::fs::create_dir_all(output)?;
    std::fs::write(output.join("evidence.jsonl"), evidence)?;
    std::fs::write(output.join("terms.jsonl"), terms)?;
    std::fs::write(output.join("manifest.json"), format!("{text}\n"))?;
    Ok(pkg)
}
/// Import pending packages in order. A package the core refuses for its content, or one that is gone,
/// will never import: it is marked stale with the reason instead of blocking every later delivery. Each
/// package is a snapshot of the whole cache and importing one replaces the previous snapshot, so when the
/// newest outcome is stale the cache as it stands now is sealed again; it names only assets still in the
/// library. Other failures (a busy or unreadable database) leave the queue as it is for the next retry.
pub(super) fn deliver(library: &Library, cache: &Path) -> Result<Json> {
    if library.frozen() {
        return Ok(Json::Scalar(Value::Null));
    }
    let c = connect(cache)?;
    let rows = c
        .prepare("SELECT task_id,package FROM native_delivery WHERE status='pending' ORDER BY rowid")?
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut last = Json::Scalar(Value::Null);
    let mut stale = false;
    for (task, path) in rows {
        (last, stale) = settle(library, &c, &task, &path)?;
    }
    if stale {
        let task = format!("resealed-{}", crate::store::new_key()?);
        let path = format!(".provenance/{task}/evidence");
        package(library, cache, &library.instance_path().join(&path))?;
        c.execute("INSERT INTO native_delivery(task_id,package) VALUES (?,?)", [&task, &path])?;
        // A resealed package that is refused again stays stale; the next research run seals a new one.
        (last, _) = settle(library, &c, &task, &path)?;
    }
    Ok(last)
}
/// Deliver one pending package: done, stale with its reason, or an error that leaves it pending.
fn settle(library: &Library, c: &Connection, task: &str, path: &str) -> Result<(Json, bool)> {
    match import(library, path) {
        Ok(report) => {
            c.execute("UPDATE native_delivery SET status='done',error='' WHERE task_id=?", [task])?;
            Ok((report, false))
        }
        Err(e) if refused(&e) => {
            let reason = e.to_string();
            c.execute("UPDATE native_delivery SET status='stale',error=? WHERE task_id=?", [&reason, task])?;
            Ok((Json::object([("status", "stale".into()), ("error", reason.into())]), true))
        }
        Err(e) => Err(e),
    }
}
fn import(library: &Library, path: &str) -> Result<Json> {
    let saved = Path::new(path);
    let path = if saved.is_absolute() { saved.to_owned() } else { library.instance_path().join(saved) };
    let pkg = crate::package::Package::open(&path)?;
    if pkg.manifest.producer != "provenance" {
        return Err(Error::Invalid("不是溯源证据包".into()));
    }
    let mut app = crate::sqlite::open_with_flags(library.db(), rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    app.busy_timeout(Duration::from_secs(2))?;
    let report = crate::store::import_package(&mut app, &pkg, library.id())?;
    Ok(Json::object([
        ("status", "done".into()),
        ("package", report.package.into()),
        ("added", (report.added as i64).into()),
        ("duplicate", (report.duplicate as i64).into()),
        ("skipped", (report.skipped as i64).into()),
        ("created_terms", (report.created_terms as i64).into()),
        ("already", report.already.into()),
    ]))
}
/// The package itself can never import: its content is refused (an asset since removed, another library,
/// a damaged file) or it no longer exists. Retrying the same bytes would fail the same way.
fn refused(e: &Error) -> bool {
    match e {
        Error::Invalid(_) | Error::Conflict(_) => true,
        Error::Io(e) | Error::IoAt { source: e, .. } => e.kind() == std::io::ErrorKind::NotFound,
        _ => false,
    }
}
/// Keep a read-only connection so SQLite data_version invalidates cached history after
/// any native or external writer commits; polling does not reopen/parse unchanged rows.
pub(super) struct History {
    connection: Connection,
    version: i64,
    value: (Json, Json),
}
impl History {
    pub fn new(path: &Path) -> Result<Self> {
        Ok(Self {
            connection: crate::app_library::read_connection(path)?,
            version: -1,
            value: (Json::Scalar(Value::Null), Json::Scalar(Value::Null)),
        })
    }
    pub fn read(&mut self) -> Result<(Json, Json)> {
        let version = self.connection.query_row("PRAGMA data_version", [], |r| r.get::<_, i64>(0))?;
        if version != self.version {
            self.value = summary(&self.connection)?;
            self.version = version;
        }
        Ok(self.value.clone())
    }
}
fn summary(c: &Connection) -> Result<(Json, Json)> {
    let mut runs = Vec::new();
    let mut coverage = Json::Scalar(Value::Null);
    let mut stmt=c.prepare("SELECT id,kind,steps,started_at,finished_at,status,reason,found,traffic_mb,report FROM runs ORDER BY id DESC LIMIT 5")?;
    let mut q = stmt.query([])?;
    while let Some(row) = q.next()? {
        let report: Option<String> = row.get(9)?;
        let parsed = report.and_then(|s| crate::http_json::parse_request(&s).ok()).unwrap_or(Json::Scalar(Value::Null));
        if matches!(coverage, Json::Scalar(Value::Null)) && !matches!(parsed, Json::Scalar(Value::Null)) {
            coverage = parsed.get("numbers").cloned().unwrap_or(Json::Scalar(Value::Null));
        }
        runs.push(Json::object([
            ("id", row.get::<_, i64>(0)?.into()),
            ("kind", row.get::<_, String>(1)?.into()),
            ("steps", row.get::<_, String>(2)?.into()),
            ("started_at", row.get::<_, String>(3)?.into()),
            ("finished_at", row.get::<_, Option<String>>(4)?.into()),
            ("status", row.get::<_, String>(5)?.into()),
            ("reason", row.get::<_, String>(6)?.into()),
            ("found", row.get::<_, i64>(7)?.into()),
            ("traffic_mb", Json::Float(row.get::<_, f64>(8)?)),
            ("report", parsed),
        ]));
    }
    Ok((Json::Array(runs), coverage))
}
pub(super) fn latest(cache: &Path) -> Result<Json> {
    let c = crate::app_library::read_connection(cache)?;
    let row: Option<(String, String, String, Option<String>)> = c
        .query_row("SELECT status,kind,reason,finished_at FROM runs ORDER BY id DESC LIMIT 1", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })
        .optional()?;
    Ok(row
        .map(|(s, k, r, f)| {
            Json::object([("status", s.into()), ("kind", k.into()), ("reason", r.into()), ("finished_at", f.into())])
        })
        .unwrap_or_else(|| Json::object([("status", "idle".into())])))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_4_cache_keeps_its_signatures_and_gains_the_colour_column() {
        let root = std::env::temp_dir().join(format!(
            "illustash-provenance-v4-{}-{}",
            std::process::id(),
            crate::store::new_key().unwrap()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("provenance.sqlite");
        Connection::open(&path)
            .unwrap()
            .execute_batch(
                "CREATE TABLE ref_sigs(site TEXT NOT NULL, post_id TEXT NOT NULL, page INTEGER NOT NULL DEFAULT 0,
                 artist_id TEXT, ratio REAL NOT NULL, grid BLOB NOT NULL, url TEXT, PRIMARY KEY(site, post_id, page));
                 INSERT INTO ref_sigs(site,post_id,ratio,grid) VALUES ('danbooru','1',1.0,x'00'); PRAGMA user_version=4;",
            )
            .unwrap();
        let c = connect(&path).unwrap();
        let version: u32 = c.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(version, 5);
        // Kept, and re-signed only when a comparison needs the colour check.
        let row: (String, Option<Vec<u8>>) =
            c.query_row("SELECT post_id, colour FROM ref_sigs", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(row, ("1".into(), None));
        drop(c);
        std::fs::remove_dir_all(&root).unwrap();
    }
    #[test]
    fn earlier_delivery_queue_gains_the_error_column_and_keeps_its_rows() {
        let root = std::env::temp_dir().join(format!(
            "illustash-provenance-delivery-{}-{}",
            std::process::id(),
            crate::store::new_key().unwrap()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("provenance.sqlite");
        drop(connect(&path).unwrap());
        Connection::open(&path)
            .unwrap()
            .execute_batch(
                "DROP TABLE native_delivery; CREATE TABLE native_delivery(task_id TEXT PRIMARY KEY, package TEXT NOT NULL,
                 status TEXT NOT NULL DEFAULT 'pending'); INSERT INTO native_delivery(task_id,package) VALUES ('t','p');",
            )
            .unwrap();
        let c = connect(&path).unwrap();
        let row: (String, String, String) = c
            .query_row("SELECT package,status,error FROM native_delivery", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .unwrap();
        assert_eq!(row, ("p".into(), "pending".into(), String::new()));
        drop(c);
        std::fs::remove_dir_all(&root).unwrap();
    }
    #[test]
    fn only_packages_that_can_never_import_are_settled_as_stale() {
        assert!(refused(&Error::Invalid("图已移除".into())));
        assert!(refused(&Error::Conflict("链接冲突".into())));
        assert!(refused(&Error::Io(std::io::ErrorKind::NotFound.into())));
        let busy = rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY), None);
        assert!(!refused(&Error::Sql(busy)));
        assert!(!refused(&Error::Io(std::io::ErrorKind::PermissionDenied.into())));
        assert!(!refused(&Error::Schema("图库需要升级".into())));
    }
}
