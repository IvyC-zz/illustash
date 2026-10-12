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
    // Directories are only reclaimed; a failure to remove one never changes the delivery outcome.
    let _ = collect(library.instance_path(), &c, false);
    Ok(last)
}
/// Retention of the run directories `.provenance/<task>` (#31). A pending package is kept: delivery and
/// recovery import it. Of the delivered packages only the newest is kept, the snapshot the core now holds
/// (every package replaces the previous one), for inspection. Stale packages never import and their reason
/// stays in `native_delivery.error`, so they go. With `orphans`, directories without a delivery row go too:
/// runs that failed, were stopped or died before the receive gate, which recovery never imports. Callers hold
/// the provenance lease, so no run of this package is writing a directory meanwhile. Returns how many went.
pub(super) fn collect(instance: &Path, c: &Connection, orphans: bool) -> Result<usize> {
    let rows = c
        .prepare("SELECT package,status FROM native_delivery ORDER BY rowid")?
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let (mut keep, mut known, mut delivered) = (BTreeSet::new(), BTreeSet::new(), None);
    for (package, status) in rows {
        let Some(name) = run_directory(instance, &package) else {
            continue;
        };
        known.insert(name.clone());
        match status.as_str() {
            "pending" => {
                keep.insert(name);
            }
            "done" => delivered = Some(name),
            _ => {}
        }
    }
    keep.extend(delivered);
    let Ok(entries) = std::fs::read_dir(instance.join(".provenance")) else {
        return Ok(0);
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if keep.contains(&name) || !orphans && !known.contains(&name) {
            continue;
        }
        let path = entry.path();
        let gone = match std::fs::symlink_metadata(&path) {
            Ok(m) if m.is_dir() => std::fs::remove_dir_all(&path),
            Ok(_) => std::fs::remove_file(&path),
            Err(e) => Err(e),
        };
        removed += usize::from(gone.is_ok());
    }
    Ok(removed)
}
/// `<task>` of a package saved as `.provenance/<task>/evidence` (relative, or absolute inside the package).
fn run_directory(instance: &Path, package: &str) -> Option<String> {
    use std::path::Component;
    let path = Path::new(package);
    let path = if path.is_absolute() { path.strip_prefix(instance).ok()? } else { path };
    match path.components().collect::<Vec<_>>().as_slice() {
        [Component::Normal(root), Component::Normal(task), Component::Normal(evidence)]
            if *root == ".provenance" && *evidence == "evidence" =>
        {
            task.to_str().map(str::to_owned)
        }
        _ => None,
    }
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
    fn run_directories_keep_pending_and_the_newest_delivered_package() {
        // #31: synthetic run directories and delivery rows only.
        let root =
            std::env::temp_dir().join(format!("illustash-provenance-retention-{}", crate::store::new_key().unwrap()));
        let runs = root.join(".provenance");
        std::fs::create_dir_all(&runs).unwrap();
        let c = connect(&root.join("provenance.sqlite")).unwrap();
        let rows = [
            ("old-done", "done"),
            ("stale", "stale"),
            ("pending", "pending"),
            ("newest-done", "done"),
            ("later-pending", "pending"),
        ];
        for (task, status) in rows {
            std::fs::create_dir_all(runs.join(task).join("evidence")).unwrap();
            std::fs::write(runs.join(task).join("evidence/manifest.json"), "{}").unwrap();
            c.execute(
                "INSERT INTO native_delivery(task_id,package,status) VALUES (?,?,?)",
                [task, &format!(".provenance/{task}/evidence"), status],
            )
            .unwrap();
        }
        // A run that failed before its receive gate, and a package saved with an absolute path.
        std::fs::create_dir_all(runs.join("failed-run/groups")).unwrap();
        std::fs::write(runs.join("failed-run/provenance.sqlite"), "staged").unwrap();
        let absolute = root.join(".provenance/absolute/evidence");
        std::fs::create_dir_all(&absolute).unwrap();
        c.execute(
            "INSERT INTO native_delivery(task_id,package,status) VALUES ('absolute',?,'pending')",
            [absolute.to_str().unwrap()],
        )
        .unwrap();
        let names = || {
            let mut v: Vec<String> =
                std::fs::read_dir(&runs).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
            v.sort();
            v
        };
        // Delivery reclaims settled packages only.
        assert_eq!(collect(&root, &c, false).unwrap(), 2);
        assert_eq!(names(), ["absolute", "failed-run", "later-pending", "newest-done", "pending"]);
        // Opening or starting a run also reclaims directories no delivery row names.
        assert_eq!(collect(&root, &c, true).unwrap(), 1);
        assert_eq!(names(), ["absolute", "later-pending", "newest-done", "pending"]);
        // Once delivered, a pending package becomes the newest delivered one and the previous goes.
        c.execute(
            "UPDATE native_delivery SET status='done' WHERE task_id IN ('pending','later-pending','absolute')",
            [],
        )
        .unwrap();
        assert_eq!(collect(&root, &c, false).unwrap(), 3);
        assert_eq!(names(), ["absolute"]);
        assert_eq!(collect(&root, &c, true).unwrap(), 0);
        drop(c);
        std::fs::remove_dir_all(&root).unwrap();
    }
    /// #31 measurement (synthetic, release): `cargo test --release provenance_delta_bench -- --ignored --nocapture`.
    /// Accepting a staged machine cache (ten tables) and the size of one sealed package. Three runs, median.
    /// ILLUSTASH_RESEARCH_BENCH_SIZES overrides the default 1000,10000,50000.
    #[test]
    #[ignore]
    fn provenance_delta_bench() {
        use std::time::Instant;
        const TABLES: [&str; 10] = [
            "artists",
            "artist_links",
            "artist_names",
            "image_sources",
            "image_artists",
            "seeds",
            "lookups",
            "marks",
            "ref_sigs",
            "ext_cache",
        ];
        let sizes = std::env::var("ILLUSTASH_RESEARCH_BENCH_SIZES").unwrap_or_else(|_| "1000,10000,50000".into());
        let median = |mut v: Vec<f64>| {
            v.sort_by(f64::total_cmp);
            v[v.len() / 2]
        };
        let root =
            std::env::temp_dir().join(format!("illustash-provenance-bench-{}", crate::store::new_key().unwrap()));
        for n in sizes.split(',').map(|s| s.trim().parse::<usize>().unwrap()) {
            let dir = root.join(n.to_string());
            std::fs::create_dir_all(&dir).unwrap();
            // A synthetic library with n pictures and a cache attributing each of them.
            let reference = dir.join("reference.sqlite");
            crate::open(&reference).unwrap().execute_batch(include_str!("app_source_empty.sql")).unwrap();
            let dest = dir.join("synthetic.pilib");
            crate::app_create::create_folder(&dest, &dir, Some("synthetic"), Some("other"), Some(&reference)).unwrap();
            let library = Library::open(&dest).unwrap();
            let base = dir.join("provenance.sqlite");
            {
                let app = crate::open(&library.db()).unwrap();
                app.execute_batch("BEGIN").unwrap();
                for i in 0..n {
                    app.execute("INSERT INTO assets(uuid,src_path) VALUES (?,'')", [format!("asset-{i:06}")]).unwrap();
                }
                app.execute_batch("COMMIT").unwrap();
                let c = connect(&base).unwrap();
                c.execute_batch("BEGIN").unwrap();
                let artists = (n / 10).max(1);
                for k in 0..artists {
                    let id = format!("artist-{k:05}");
                    c.execute("INSERT INTO artists(id) VALUES (?)", [&id]).unwrap();
                    c.execute(
                        "INSERT INTO artist_links(system,key,artist_id,source) VALUES ('danbooru',?,?,'danbooru_post')",
                        [format!("tag_{k}"), id.clone()],
                    )
                    .unwrap();
                    c.execute(
                        "INSERT INTO artist_names(artist_id,source,name) VALUES (?,'danbooru',?)",
                        [id.clone(), format!("name {k}")],
                    )
                    .unwrap();
                    c.execute("INSERT INTO seeds(artist_id,source) VALUES (?,'pixel_hash')", [&id]).unwrap();
                }
                for i in 0..n {
                    let asset = format!("asset-{i:06}");
                    let artist = format!("artist-{:05}", i % artists);
                    c.execute("INSERT INTO image_sources(asset,site,post_id,url,artist_id,method) VALUES (?,'danbooru',?,?,?,'pixel_hash')", [asset.clone(), i.to_string(), format!("https://example.invalid/posts/{i}"), artist.clone()]).unwrap();
                    c.execute("INSERT INTO image_artists(asset,artist_id,method,score,evidence,status,updated_at) VALUES (?,?,'pixel_hash',1.0,?,'auto','2026-10-12 00:00:00')", [asset.clone(), artist.clone(), format!("{{\"post\":{i}}}")]).unwrap();
                    c.execute("INSERT INTO lookups(target,backend,attempts,last_at,last_result) VALUES (?,'pixel_hash',1,'2026-10-12 00:00:00','hit')", [&asset]).unwrap();
                    c.execute(
                        "INSERT INTO ref_sigs(site,post_id,page,artist_id,ratio,grid) VALUES ('danbooru',?,0,?,1.25,?)",
                        rusqlite::params![i.to_string(), artist, vec![(i % 256) as u8; 256]],
                    )
                    .unwrap();
                }
                for i in 0..n / 100 {
                    c.execute(
                        "INSERT INTO ext_cache(key,body,fetched_at) VALUES (?,?,'2026-10-12 00:00:00')",
                        rusqlite::params![format!("synthetic-{i}"), vec![b'x'; 4096]],
                    )
                    .unwrap();
                }
                c.execute_batch("COMMIT; PRAGMA wal_checkpoint(TRUNCATE);").unwrap();
            }
            let cache_bytes = std::fs::metadata(&base).unwrap().len();
            let started = Instant::now();
            let sealed = dir.join("evidence");
            package(&library, &base, &sealed).unwrap();
            let seal_ms = started.elapsed().as_secs_f64() * 1000.0;
            let package_bytes: u64 =
                std::fs::read_dir(&sealed).unwrap().map(|e| e.unwrap().metadata().unwrap().len()).sum();
            println!(
                "{}",
                json!({"n": n, "cache_bytes": cache_bytes, "package_bytes": package_bytes, "seal_ms": seal_ms})
            );
            let mid = format!("asset-{:06}", n / 2);
            let modes: [(&str, String); 3] = [
                ("nochange", String::new()),
                ("single", format!("UPDATE lookups SET attempts=attempts+1 WHERE target='{mid}'; INSERT OR REPLACE INTO image_artists(asset,artist_id,method,score,evidence,status,updated_at) SELECT asset,artist_id,method,0.9,evidence,status,'2026-10-13 00:00:00' FROM image_artists WHERE asset='{mid}';")),
                ("full", "UPDATE lookups SET attempts=attempts+1,last_at='2026-10-13 00:00:00';".into()),
            ];
            for (mode, sql) in modes {
                let stage = dir.join(format!("stage-{mode}.sqlite"));
                std::fs::copy(&base, &stage).unwrap();
                connect(&stage).unwrap().execute_batch(&sql).unwrap();
                crate::app_library::read_connection(&stage)
                    .unwrap()
                    .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
                    .ok();
                for code in ["main", "#31"] {
                    let (mut publish, mut wal, mut written) = (vec![], vec![], 0usize);
                    for run in 0..3 {
                        let path = dir.join(format!("run-{run}.sqlite"));
                        std::fs::copy(&base, &path).unwrap();
                        let mut c = connect(&path).unwrap();
                        c.execute_batch("PRAGMA wal_autocheckpoint=0").unwrap();
                        c.execute("ATTACH DATABASE ? AS stage", [stage.to_str().unwrap()]).unwrap();
                        c.execute_batch("PRAGMA defer_foreign_keys=ON").unwrap();
                        let started = Instant::now();
                        let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).unwrap();
                        written = 0;
                        for table in TABLES {
                            if code == "main" {
                                written += tx.execute(&format!("DELETE FROM {table}"), []).unwrap();
                                written += tx
                                    .execute(&format!("INSERT INTO {table} SELECT * FROM stage.{table}"), [])
                                    .unwrap();
                            } else {
                                written += crate::table_delta::mirror(&tx, table, "stage").unwrap().rows();
                            }
                        }
                        tx.commit().unwrap();
                        publish.push(started.elapsed().as_secs_f64() * 1000.0);
                        wal.push(
                            std::fs::metadata(dir.join(format!("run-{run}.sqlite-wal"))).map(|m| m.len()).unwrap_or(0)
                                as f64,
                        );
                        drop(c);
                        for suffix in ["", "-wal", "-shm"] {
                            let _ = std::fs::remove_file(dir.join(format!("run-{run}.sqlite{suffix}")));
                        }
                    }
                    println!(
                        "{}",
                        json!({"n": n, "mode": mode, "code": code, "publish_ms": median(publish), "rows_written": written, "wal_bytes": median(wal)})
                    );
                }
            }
        }
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
