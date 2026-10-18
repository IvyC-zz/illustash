//! Rescuing a damaged source.sqlite inside the app (#228, user decision 2026-10-19).
//!
//! The recognition results in source.sqlite can be made again; the picture IDs the curation records in library.sqlite
//! point at cannot: a folder picture's ID was made at random when it was first scanned. A rescue therefore
//!
//! 1. copies every row SQLite can still read into a new file with the damaged file's own schema, keeping IDs and
//!    rowids, stepping over pages it cannot read (indices, triggers and views are made after the rows);
//! 2. scans the folder sources into the new file, giving each picture it could not read back the ID it had: by full
//!    path, then by content, from the rows it did read and from library.sqlite (`assets.src_path` and the
//!    `folder_hash:<uuid>` records, which every backup of library.sqlite carries as well). Photos IDs are Photos'
//!    own, so the next sync finds those again by itself;
//! 3. leaves to recognition the pictures whose results it could not read.
//!
//! Nothing in the package changes until the new file is complete and checked: it is built beside the damaged one, which
//! then moves to backups/ (kept, never deleted here) as the new one takes its name, when nothing has either open.
use crate::app_folder::Earlier;
use crate::app_integrity::Health;
use crate::app_library::Library;
use crate::http_json::Json;
use crate::{Error, Result};
use rusqlite::{types::Value as Sql, Connection, OpenFlags, OptionalExtension};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread::JoinHandle,
};

/// The new file while it is being built, beside source.sqlite in the package.
const BUILDING: &str = "source.rescue.sqlite";
/// What the folder scans of the new file leave for the next refresh of library.sqlite (`app_folder`'s journal).
const BUILDING_JOURNAL: &str = "source.rescue.scan-pending.json";

/// What the person is told while it runs: the stage and the fraction of the whole rescue.
pub(crate) type Step<'a> = &'a dyn Fn(&str, f64);
const READING: &str = "正在抢救识别结果数据库：读出还能读的数据";
const SCANNING: &str = "正在抢救识别结果数据库：重新扫描来源文件夹";
const FINISHING: &str = "正在抢救识别结果数据库：检查新数据库";

/// What a rescue did, for the page and the log.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Report {
    /// Tables in which some rows could not be read.
    pub(crate) damaged: Vec<String>,
    /// Not even the list of tables could be read: the new file started from an empty source.
    pub(crate) schema_lost: bool,
    /// Pictures read back from the damaged file.
    pub(crate) salvaged: i64,
    /// Pictures the scans gave back their earlier ID.
    pub(crate) kept: i64,
    /// Pictures the scans found under a new ID: they had none on record.
    pub(crate) new: i64,
    /// Pictures waiting for recognition in the new file.
    pub(crate) pending: i64,
    /// Where the damaged file went (after `install`).
    pub(crate) damaged_copy: Option<PathBuf>,
}
impl Report {
    pub(crate) fn json(&self) -> Json {
        Json::object([
            ("damaged_tables", Json::Array(self.damaged.iter().map(|t| t.as_str().into()).collect())),
            ("schema_lost", self.schema_lost.into()),
            ("salvaged", serde_json::Value::from(self.salvaged).into()),
            ("kept", serde_json::Value::from(self.kept).into()),
            ("new", serde_json::Value::from(self.new).into()),
            ("pending", serde_json::Value::from(self.pending).into()),
            (
                "damaged_copy",
                match &self.damaged_copy {
                    Some(path) => crate::app_upgrade::shown(path).as_str().into(),
                    None => serde_json::Value::Null.into(),
                },
            ),
        ])
    }
}

/// A complete, checked new source.sqlite waiting beside the damaged one.
#[derive(Clone, Debug)]
pub(crate) struct Rebuilt {
    package: PathBuf,
    pub(crate) report: Report,
}

/// Removes what an unfinished rescue left in the package.
fn discard(package: &Path) {
    for name in [BUILDING.to_owned(), format!("{BUILDING}-journal"), BUILDING_JOURNAL.to_owned()] {
        let _ = fs::remove_file(package.join(name));
    }
}

/// The damaged file's layout as SQLite lists it, read on a connection of its own.
struct Schema {
    page_size: i64,
    auto_vacuum: i64,
    user_version: i64,
    application_id: i64,
    /// (type, name, sql) in the file's own order.
    entries: Vec<(String, String, String)>,
}
fn read_schema(path: &Path) -> Result<Schema> {
    let c = crate::app_library::read_connection(path)?;
    let read = |name: &str| c.query_row(&format!("PRAGMA {name}"), [], |r| r.get::<_, i64>(0));
    let schema = Schema {
        page_size: read("page_size")?,
        auto_vacuum: read("auto_vacuum")?,
        user_version: read("user_version")?,
        application_id: read("application_id")?,
        entries: c
            .prepare("SELECT type,name,sql FROM sqlite_master WHERE sql IS NOT NULL ORDER BY rowid")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?,
    };
    Ok(schema)
}

/// Builds the new file beside the damaged source.sqlite of `library`; nothing else in the package changes. Every
/// folder source must be readable: a picture of one that is not would be scanned later, under a new ID.
pub(crate) fn rebuild(library: &Library, reference: Option<&Path>, stop: &AtomicBool, step: Step) -> Result<Rebuilt> {
    let package = library.instance_path().to_owned();
    discard(&package);
    let result = build(library, reference, stop, step);
    match result {
        Ok(report) => Ok(Rebuilt { package, report }),
        Err(error) => {
            discard(&package);
            Err(error)
        }
    }
}

fn folders(library: &Library) -> Vec<Value> {
    library.config()["sources"].as_array().into_iter().flatten().filter(|s| s["kind"] == "folder").cloned().collect()
}

fn build(library: &Library, reference: Option<&Path>, stop: &AtomicBool, step: Step) -> Result<Report> {
    let folders = folders(library);
    for source in &folders {
        let path = Path::new(source["path"].as_str().unwrap_or(""));
        if !path.is_dir() {
            return Err(Error::Invalid(format!(
                "来源文件夹现在读不到：{path}。为了让每张图找回原来的 ID，请先恢复对它的访问（例如接上所在的磁盘），再抢救",
                path = path.display()
            )));
        }
    }
    let target = library.instance_path().join(BUILDING);
    let mut report = Report::default();
    step(READING, 0.0);
    let schema = read_schema(&library.source());
    {
        let mut c = crate::sqlite::open_with_flags(
            &target,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE | OpenFlags::SQLITE_OPEN_URI,
        )?;
        match schema {
            Ok(schema) => salvage(&mut c, &library.source(), &schema, stop, step, &mut report)?,
            Err(error) => {
                eprintln!("识别结果数据库的表清单读不出，从空库重建：{error}");
                report.schema_lost = true;
            }
        }
    }
    if report.schema_lost {
        crate::app_create::initialize_source(&target, reference)?;
    } else if let Some(reference) = reference {
        // The character tables came from the reference library: what could not be read comes back from there.
        let lost = report.damaged.iter().filter(|t| ["characters", "character_aliases"].contains(&t.as_str()));
        let lost = lost.cloned().collect::<Vec<_>>();
        if !lost.is_empty() {
            refill(&target, reference, &lost)?;
        }
    }
    crate::app_sync::check(stop)?;
    step(SCANNING, 0.6);
    let mut earlier = earlier(&target, library, &folders)?;
    for (i, source) in folders.iter().enumerate() {
        let share = 0.35 / folders.len() as f64;
        let from = 0.6 + share * i as f64;
        let progress = |done: usize, total: usize| step(SCANNING, from + share * done as f64 / total.max(1) as f64);
        let scan = crate::app_folder::scan_with(
            source,
            &target,
            &|| crate::app_sync::check(stop),
            &progress,
            Some(&mut earlier),
        )?;
        report.kept += scan.counts["kept"].as_i64().unwrap_or(0);
        report.new += scan.counts["new"].as_i64().unwrap_or(0);
    }
    step(FINISHING, 0.95);
    tidy(&target, library, &folders, &mut report)?;
    crate::app_sync::check(stop)?;
    let c = crate::sqlite::open_with_flags(&target, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let check: String = c.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
    if check != "ok" {
        return Err(Error::Schema(format!("抢救出的识别结果数据库没有通过检查：{result}", result = check)));
    }
    step(FINISHING, 1.0);
    Ok(report)
}

/// Rows of every table the damaged file still lists, then its indices, triggers and views.
fn salvage(
    c: &mut Connection,
    damaged: &Path,
    schema: &Schema,
    stop: &AtomicBool,
    step: Step,
    report: &mut Report,
) -> Result<()> {
    c.execute_batch(&format!("PRAGMA page_size={}; PRAGMA auto_vacuum={};", schema.page_size, schema.auto_vacuum))?;
    // The damaged file has a connection of its own: what SQLite meets there stays out of the new file's writes.
    let old = crate::app_library::read_connection(damaged)?;
    let tables = schema
        .entries
        .iter()
        .filter(|(kind, name, _)| kind == "table" && !name.starts_with("sqlite_"))
        .collect::<Vec<_>>();
    let tx = c.transaction()?;
    for (_, name, sql) in &tables {
        // A virtual table makes its own shadow tables, which the list then names again.
        if let Err(error) = tx.execute_batch(sql) {
            eprintln!("抢救时没能建表 {name}：{error}");
        }
    }
    // Each table's share of the reading is its span of rowids: the rows it may hold.
    let spans = tables
        .iter()
        .map(|(_, name, sql)| Table::new(&old, name, sql).map(|t| t.span()))
        .collect::<Result<Vec<_>>>()?;
    let total = spans.iter().sum::<f64>().max(1.0);
    let mut done = 0.0;
    for ((_, name, sql), span) in tables.iter().zip(&spans) {
        let table = Table::new(&old, name, sql)?;
        let before = done;
        let lost =
            table.copy(&old, &tx, stop, &mut |fraction| step(READING, 0.6 * (before + span * fraction) / total))?;
        if lost {
            eprintln!("抢救时 {name} 有读不出的数据，已跳过");
            report.damaged.push((*name).clone());
        }
        done += span;
    }
    for (kind, name, sql) in &schema.entries {
        if ["index", "trigger", "view"].contains(&kind.as_str()) {
            tx.execute_batch(sql).map_err(|e| Error::Schema(format!("抢救时没能重建 {name}：{e}")))?;
        }
    }
    tx.execute_batch(&format!(
        "PRAGMA user_version={}; PRAGMA application_id={};",
        schema.user_version, schema.application_id
    ))?;
    crate::app_create::complete_source(&tx)?;
    report.salvaged = tx.query_row("SELECT count(*) FROM assets", [], |r| r.get(0))?;
    tx.commit()?;
    Ok(())
}

/// One table of the damaged file and how its rows are read back.
struct Table {
    name: String,
    columns: Vec<String>,
    /// Ordinary tables are read in rowid order, so a read can resume past an unreadable page.
    rowid: bool,
    /// The rowid is a column of its own (no INTEGER PRIMARY KEY names it), copied as well.
    hidden_rowid: bool,
    first: Option<i64>,
    last: Option<i64>,
}
impl Table {
    fn new(c: &Connection, name: &str, sql: &str) -> Result<Self> {
        let quoted = crate::app_create::quote_id(name);
        let info = c
            .prepare(&format!("PRAGMA table_info({quoted})"))?
            .query_map([], |r| Ok((r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, i64>(5)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let upper = sql.to_ascii_uppercase();
        let rowid = !upper.contains("WITHOUT ROWID") && !upper.starts_with("CREATE VIRTUAL");
        let keys = info.iter().filter(|(_, _, pk)| *pk > 0).collect::<Vec<_>>();
        let alias = keys.len() == 1 && keys[0].1.eq_ignore_ascii_case("INTEGER");
        let edge = |order: &str| -> Option<i64> {
            if !rowid {
                return None;
            }
            c.query_row(&format!("SELECT {order}(rowid) FROM {quoted}"), [], |r| r.get(0)).ok().flatten()
        };
        Ok(Self {
            name: name.to_owned(),
            columns: info.into_iter().map(|(n, _, _)| n).collect(),
            rowid,
            hidden_rowid: rowid && !alias,
            first: edge("min"),
            last: edge("max"),
        })
    }
    fn span(&self) -> f64 {
        match (self.first, self.last) {
            (Some(a), Some(b)) => (b as f64 - a as f64 + 1.0).max(1.0),
            _ => 1.0,
        }
    }
    /// Copies the readable rows of `old` into the same table of `c`; true when some could not be read.
    fn copy(&self, old: &Connection, c: &Connection, stop: &AtomicBool, progress: &mut dyn FnMut(f64)) -> Result<bool> {
        if self.columns.is_empty() {
            return Ok(false);
        }
        let quoted = crate::app_create::quote_id(&self.name);
        let mut names = self.columns.iter().map(|n| crate::app_create::quote_id(n)).collect::<Vec<_>>();
        if self.hidden_rowid {
            names.push("rowid".into());
        }
        let width = names.len();
        let mut insert = c.prepare(&format!(
            "INSERT OR IGNORE INTO {quoted}({}) VALUES ({})",
            names.join(","),
            vec!["?"; width].join(",")
        ))?;
        let columns = self.columns.iter().map(|n| crate::app_create::quote_id(n)).collect::<Vec<_>>().join(",");
        if !self.rowid {
            // No order to resume in: the rows before the first unreadable one.
            let mut select = old.prepare(&format!("SELECT {columns} FROM {quoted}"))?;
            let mut rows = select.query([])?;
            loop {
                match rows.next() {
                    Ok(Some(row)) => {
                        let values = (0..width).map(|i| row.get::<_, Sql>(i)).collect::<rusqlite::Result<Vec<_>>>();
                        let Ok(values) = values else { return Ok(true) };
                        if !written(insert.execute(rusqlite::params_from_iter(values)))? {
                            return Ok(true);
                        }
                    }
                    Ok(None) => return Ok(false),
                    Err(_) => {
                        crate::app_sync::check(stop)?;
                        return Ok(true);
                    }
                }
            }
        }
        let mut select = old.prepare(&format!("SELECT {columns},rowid FROM {quoted} WHERE rowid>?1 ORDER BY rowid"))?;
        let mut seek = old.prepare(&format!("SELECT rowid FROM {quoted} WHERE rowid>?1 ORDER BY rowid LIMIT 1"))?;
        let (first, last) = (self.first.unwrap_or(0) as f64, self.last.unwrap_or(0) as f64);
        let fraction =
            |at: i64| if last > first { ((at as f64 - first) / (last - first)).clamp(0.0, 1.0) } else { 0.0 };
        let mut after = i64::MIN;
        // The row a resume pointed at: when it cannot be read either, it is passed over.
        let mut resumed_at = None;
        let mut lost = false;
        let mut count = 0u64;
        loop {
            let mut read_any = false;
            let failed = {
                let mut rows = select.query([after])?;
                loop {
                    match rows.next() {
                        Ok(Some(row)) => {
                            let id = row.get::<_, i64>(self.columns.len());
                            let values = (0..width).map(|i| row.get::<_, Sql>(i)).collect::<rusqlite::Result<Vec<_>>>();
                            let (Ok(id), Ok(values)) = (id, values) else { break true };
                            if !written(insert.execute(rusqlite::params_from_iter(values)))? {
                                break true;
                            }
                            after = id;
                            read_any = true;
                            count += 1;
                            if count % 4096 == 0 {
                                crate::app_sync::check(stop)?;
                                progress(fraction(after));
                            }
                        }
                        Ok(None) => break false,
                        Err(_) => break true,
                    }
                }
            };
            if !failed {
                progress(1.0);
                return Ok(lost);
            }
            crate::app_sync::check(stop)?;
            lost = true;
            if let (false, Some(row)) = (read_any, resumed_at) {
                after = row;
            }
            match resume(&mut seek, after)? {
                Some(next) => {
                    after = next - 1;
                    resumed_at = Some(next);
                }
                None => {
                    progress(1.0);
                    return Ok(true);
                }
            }
        }
    }
}

/// Whether a salvaged row went into the new file. The new file is sound, so SQLite reporting damage while writing it
/// means the row's own content (read from the damaged file only as it is written) is unreadable.
fn written(result: rusqlite::Result<usize>) -> Result<bool> {
    match result {
        Ok(_) => Ok(true),
        Err(e) if crate::app_library::corruption(&e).is_some() => Ok(false),
        Err(e) => Err(e.into()),
    }
}

/// After a read failed past rowid `after`: the smallest rowid beyond it that a seek can reach, if any. Seeks start at
/// growing distances until one lands past the damage, then narrow down to where readable rows begin again.
fn resume(seek: &mut rusqlite::Statement, after: i64) -> Result<Option<i64>> {
    let mut probe =
        |from: i64| -> Option<Option<i64>> { seek.query_row([from], |r| r.get::<_, i64>(0)).optional().ok() };
    // Distances and midpoints in i128: the rowid range spans all of i64.
    let (mut low, mut high, mut found) = (after as i128, None, None);
    let mut distance: i128 = 1;
    loop {
        let from = (after as i128 + distance).min(i64::MAX as i128);
        match probe(from as i64) {
            Some(None) => return Ok(None),
            Some(Some(next)) => {
                high = Some(from);
                found = Some(next);
                break;
            }
            None => low = from,
        }
        if from == i64::MAX as i128 {
            break;
        }
        distance *= 2;
    }
    let Some(mut high) = high else { return Ok(None) };
    while high - low > 1 {
        let middle = low + (high - low) / 2;
        match probe(middle as i64) {
            Some(Some(next)) => {
                high = middle;
                found = Some(next);
            }
            Some(None) => break,
            None => low = middle,
        }
    }
    Ok(found)
}

/// Rows of `tables` from the reference library, matching columns only, where the rescue could not read them.
fn refill(target: &Path, reference: &Path, tables: &[String]) -> Result<()> {
    let c = crate::sqlite::open_with_flags(target, OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_URI)?;
    c.execute("ATTACH DATABASE ? AS ref", [crate::app_create::file_uri(reference)?])?;
    for table in tables {
        let columns = |db: &str| -> Result<HashSet<String>> {
            Ok(c.prepare(&format!("PRAGMA {db}.table_info({})", crate::app_create::quote_id(table)))?
                .query_map([], |r| r.get::<_, String>(1))?
                .collect::<rusqlite::Result<_>>()?)
        };
        let ours = columns("main")?;
        let mut shared = columns("ref")?.into_iter().filter(|n| ours.contains(n)).collect::<Vec<_>>();
        shared.sort();
        if shared.is_empty() {
            continue;
        }
        let list = shared.iter().map(|n| crate::app_create::quote_id(n)).collect::<Vec<_>>().join(",");
        let quoted = crate::app_create::quote_id(table);
        c.execute_batch(&format!("INSERT OR IGNORE INTO main.{quoted}({list}) SELECT {list} FROM ref.{quoted}"))?;
    }
    c.execute("DETACH DATABASE ref", [])?;
    Ok(())
}

/// What the new file and library.sqlite know about earlier IDs. Folder bookkeeping rows whose picture row was lost
/// cannot stand: they leave the new file, and their picture comes back through the scan under the same ID.
fn earlier(target: &Path, library: &Library, folders: &[Value]) -> Result<Earlier> {
    let mut earlier = Earlier::default();
    let add = |earlier: &mut Earlier, uuid: String, path: Option<String>, hash: Option<String>| {
        if let Some(path) = path.filter(|p| !p.is_empty()) {
            earlier.by_path.entry(path).or_insert_with(|| uuid.clone());
        }
        if let Some(hash) = hash.filter(|h| !h.is_empty()) {
            earlier.hash_of.entry(uuid.clone()).or_insert_with(|| hash.clone());
            earlier.by_hash.entry(hash).or_default().push(uuid);
        }
    };
    // library.sqlite first: its curation records point at these IDs.
    let lib = crate::app_library::read_connection(&library.db())?;
    let hashes = lib
        .prepare("SELECT substr(key,13),value FROM meta WHERE key LIKE 'folder\\_hash:%' ESCAPE '\\'")?
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<HashMap<_, _>>>()?;
    let paths = lib
        .prepare("SELECT uuid,src_path FROM assets WHERE uuid IS NOT NULL ORDER BY uuid")?
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut hashed = hashes.into_iter().collect::<Vec<_>>();
    hashed.sort();
    for (uuid, path) in paths {
        add(&mut earlier, uuid, path, None);
    }
    for (uuid, hash) in hashed {
        add(&mut earlier, uuid, None, Some(hash));
    }
    let c = crate::sqlite::open_with_flags(target, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    let rows = c
        .prepare("SELECT uuid,src_path,stable_hash FROM assets WHERE uuid IS NOT NULL ORDER BY rowid")?
        .query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, Option<String>>(2)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (uuid, path, hash) in rows {
        add(&mut earlier, uuid, path, hash);
    }
    let roots = folders
        .iter()
        .filter_map(|s| Some((s["id"].as_str()?.to_owned(), PathBuf::from(s["path"].as_str()?))))
        .collect::<HashMap<_, _>>();
    let orphans = c
        .prepare("SELECT source_id,path,md5,asset FROM files f WHERE NOT EXISTS(SELECT 1 FROM assets a WHERE a.uuid=f.asset)")?
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (sid, path, hash, uuid) in &orphans {
        let full = roots.get(sid).map(|root| root.join(path).to_string_lossy().into_owned());
        add(&mut earlier, uuid.clone(), full, Some(hash.clone()));
        c.execute("DELETE FROM files WHERE source_id=? AND path=?", [sid, path])?;
        c.execute("DELETE FROM asset_links WHERE system='file' AND key=? AND source_id=?", [path, sid])?;
    }
    for ids in earlier.by_hash.values_mut() {
        let mut seen = HashSet::new();
        ids.retain(|u| seen.insert(u.clone()));
    }
    Ok(earlier)
}

/// After the scans: folder pictures no scan found are gone, as a scan would mark them; results of pictures no record
/// knows are dropped; and pictures without results are recognized again, so no ledger entry may say they were.
fn tidy(target: &Path, library: &Library, folders: &[Value], report: &mut Report) -> Result<()> {
    let mut c = crate::sqlite::open_with_flags(target, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    let lib = crate::app_library::read_connection(&library.db())?;
    let mut known = lib
        .prepare("SELECT uuid FROM assets WHERE uuid IS NOT NULL")?
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    known.extend(
        lib.prepare("SELECT substr(key,13) FROM meta WHERE key LIKE 'folder\\_hash:%' ESCAPE '\\'")?
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?,
    );
    drop(lib);
    let tx = c.transaction()?;
    tx.execute_batch("CREATE TEMP TABLE rescue_known(uuid TEXT PRIMARY KEY)")?;
    {
        let mut put = tx.prepare("INSERT OR IGNORE INTO temp.rescue_known VALUES (?)")?;
        for uuid in &known {
            put.execute([uuid])?;
        }
    }
    for source in folders {
        if let Some(sid) = source["id"].as_str() {
            tx.execute(
                "UPDATE assets SET trashed=1 WHERE coalesce(trashed,0)=0 AND source_id=? AND NOT EXISTS(SELECT 1 FROM files f WHERE f.asset=assets.uuid)",
                [sid],
            )?;
        }
    }
    for table in ["labels", "embeddings"] {
        tx.execute_batch(&format!(
            "DELETE FROM {table} WHERE NOT EXISTS(SELECT 1 FROM assets a WHERE a.uuid={table}.uuid) AND NOT EXISTS(SELECT 1 FROM temp.rescue_known k WHERE k.uuid={table}.uuid)"
        ))?;
    }
    let ledger: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='recognition_assets')",
        [],
        |r| r.get(0),
    )?;
    if ledger {
        tx.execute_batch(
            "DELETE FROM recognition_assets WHERE NOT EXISTS(SELECT 1 FROM labels l WHERE l.uuid=recognition_assets.asset AND l.dim='rating')",
        )?;
    }
    report.pending = tx.query_row(
        "SELECT count(*) FROM assets a WHERE in_scope=1 AND coalesce(trashed,0)=0 AND NOT EXISTS(SELECT 1 FROM labels l WHERE l.uuid=a.uuid AND l.dim='rating')",
        [],
        |r| r.get(0),
    )?;
    tx.execute_batch("DROP TABLE temp.rescue_known")?;
    tx.commit()?;
    Ok(())
}

/// Puts the rebuilt file in place of the damaged source.sqlite, which moves to backups/. Nothing may have either file
/// open (the caller owns the package and has closed its connections). The record then has the damage cleared.
pub(crate) fn install(library: &Library, health: &Health, rebuilt: Rebuilt) -> Result<Report> {
    let package = library.instance_path();
    if rebuilt.package != package {
        return Err(Error::Invalid("抢救结果不属于这个图库".into()));
    }
    let building = package.join(BUILDING);
    if !building.is_file() {
        return Err(Error::Invalid("抢救结果已不在图库里，请重新抢救".into()));
    }
    let backups = library.backups();
    fs::create_dir_all(&backups)?;
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    let mut kept = backups.join(format!("source-damaged-{stamp}.sqlite"));
    let mut n = 1;
    while kept.exists() {
        n += 1;
        kept = backups.join(format!("source-damaged-{stamp}-{n}.sqlite"));
    }
    let source = library.source();
    fs::rename(&source, &kept)?;
    let journal = package.join("source.sqlite-journal");
    if journal.exists() {
        let mut moved = kept.clone().into_os_string();
        moved.push("-journal");
        if let Err(error) = fs::rename(&journal, PathBuf::from(moved)) {
            let _ = fs::rename(&kept, &source);
            return Err(error.into());
        }
    }
    if let Err(error) = fs::rename(&building, &source) {
        // The damaged file goes back: the package is as it was.
        let _ = fs::rename(&kept, &source);
        return Err(error.into());
    }
    merge_journal(package, &source)?;
    health.rescued()?;
    let mut report = rebuilt.report;
    report.damaged_copy = Some(kept);
    eprintln!(
        "识别结果数据库已抢救：读回 {} 张，按记录找回 ID {} 张，新 ID {} 张，待识别 {} 张；损坏的文件在 {}",
        report.salvaged,
        report.kept,
        report.new,
        report.pending,
        report.damaged_copy.as_deref().map(|p| p.display().to_string()).unwrap_or_default()
    );
    Ok(report)
}

/// The rescue's folder scans leave changed pictures for the next refresh of library.sqlite, under the new file's name.
fn merge_journal(package: &Path, source: &Path) -> Result<()> {
    let building = package.join(BUILDING_JOURNAL);
    if !building.exists() {
        return Ok(());
    }
    let target = source.with_extension("scan-pending.json");
    let read = |path: &Path| -> Result<serde_json::Map<String, Value>> {
        serde_json::from_str(&fs::read_to_string(path)?).map_err(|e| Error::Invalid(e.to_string()))
    };
    let mut merged = if target.exists() { read(&target)? } else { serde_json::Map::new() };
    merged.extend(read(&building)?);
    let temp = package.join("source.scan-pending.rescue.tmp");
    fs::write(&temp, serde_json::to_string(&merged).expect("internal JSON value is serializable"))?;
    fs::rename(&temp, &target)?;
    fs::remove_file(&building)?;
    Ok(())
}

/// A rescue of the package `library`, owned by the caller, from start to finish: built, then put in place.
pub(crate) fn rescue(
    library: &Library,
    health: &Health,
    reference: Option<&Path>,
    stop: &AtomicBool,
    step: Step,
) -> Result<Report> {
    let rebuilt = rebuild(library, reference, stop, step)?;
    install(library, health, rebuilt)
}

#[derive(Default)]
struct State {
    running: Option<(String, f64)>,
    ready: Option<Rebuilt>,
    error: Option<String>,
}

/// A session's rescue in the background (#228): the session stays stopped meanwhile, writing nothing; the new file is
/// put in place when the page opens the library again.
#[derive(Default)]
pub(crate) struct Rescuer {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    state: Arc<Mutex<State>>,
}
impl Rescuer {
    pub(crate) fn running(&self) -> bool {
        self.thread.as_ref().is_some_and(|thread| !thread.is_finished())
    }
    /// Starts building; false when a rescue is running or one is ready.
    pub(crate) fn start(&mut self, library: &Library, reference: Option<PathBuf>) -> bool {
        if self.running() || self.state.lock().expect("rescue state lock poisoned").ready.is_some() {
            return false;
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let stop = Arc::new(AtomicBool::new(false));
        self.stop = stop.clone();
        let state = self.state.clone();
        *state.lock().expect("rescue state lock poisoned") =
            State { running: Some((READING.to_owned(), 0.0)), ready: None, error: None };
        let library = library.clone();
        self.thread = Some(std::thread::spawn(move || {
            let shown = state.clone();
            let step = move |stage: &str, done: f64| {
                shown.lock().expect("rescue state lock poisoned").running = Some((stage.to_owned(), done));
            };
            let result = rebuild(&library, reference.as_deref(), &stop, &step);
            let mut state = state.lock().expect("rescue state lock poisoned");
            state.running = None;
            match result {
                Ok(rebuilt) if !stop.load(Ordering::SeqCst) => state.ready = Some(rebuilt),
                Ok(rebuilt) => discard(&rebuilt.package),
                Err(_) if stop.load(Ordering::SeqCst) => {}
                Err(error) => {
                    eprintln!("识别结果数据库抢救没有完成：{error}");
                    state.error = Some(error.to_string());
                }
            }
        }));
        true
    }
    /// A finished rebuild waits to be put in place.
    pub(crate) fn ready(&self) -> bool {
        !self.running() && self.state.lock().expect("rescue state lock poisoned").ready.is_some()
    }
    /// The finished rebuild of `package`, for putting it in place.
    pub(crate) fn take(&mut self, package: &Path) -> Option<Rebuilt> {
        if self.running() {
            return None;
        }
        let mut state = self.state.lock().expect("rescue state lock poisoned");
        if state.ready.as_ref().is_some_and(|r| r.package == package) {
            state.ready.take()
        } else {
            None
        }
    }
    /// Stops a running rescue and drops a finished one that was never put in place.
    pub(crate) fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        if let Some(rebuilt) = self.state.lock().expect("rescue state lock poisoned").ready.take() {
            discard(&rebuilt.package);
        }
    }
    /// For `GET /api/integrity`: `{running: {stage, done} | null, ready, error}`.
    pub(crate) fn info(&self) -> Json {
        let state = self.state.lock().expect("rescue state lock poisoned");
        let running = match state.running.as_ref().filter(|_| self.running()) {
            Some((stage, done)) => {
                Json::object([("stage", stage.as_str().into()), ("done", Json::Float(done.clamp(0.0, 1.0)))])
            }
            None => serde_json::Value::Null.into(),
        };
        Json::object([
            ("running", running),
            ("ready", state.ready.is_some().into()),
            (
                "error",
                match &state.error {
                    Some(error) => error.as_str().into(),
                    None => serde_json::Value::Null.into(),
                },
            ),
        ])
    }
}
impl Drop for Rescuer {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
#[path = "../tests/support/rescue.rs"]
mod tests;
