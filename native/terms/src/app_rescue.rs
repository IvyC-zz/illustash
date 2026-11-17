//! 在 App 里抢救损坏的 source.sqlite（#228，用户 2026-10-19 决定）。
//!
//! source.sqlite 里的识别结果可以重新生成；library.sqlite 里人工记录指向的图片 ID 不能：文件夹图片的 ID 是第一次
//! 扫描时随机生成的。因此抢救时
//!
//! 1. 把 SQLite 还能读出的每一行复制到一个新文件里，新文件用损坏文件自己的 schema，保留 ID 和 rowid，跳过读不出的
//!    页（索引、触发器和视图在写完行之后再建）；
//! 2. 把各文件夹来源扫描进新文件，让没能读回的图片拿回原来的 ID：先按路径和内容，再按唯一内容，依据是读出的行和
//!    library.sqlite（`assets.src_path` 以及 `folder_hash:<uuid>` 记录，library.sqlite 的每份备份里也都有）。
//!    Photos 的 ID 是 Photos 自己的，下一次同步会自己重新找到；
//! 3. 结果没能读回的图片留给识别。
//!
//! 新文件完整建好并检查通过之前，图库包里什么都不变：新文件建在损坏文件旁边；等两个文件都没有被打开时，损坏文件
//! 移到 backups/（保留，这里从不删除），新文件接替它的名字。
use crate::app_folder_identity::Earlier;
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

/// 正在建的新文件，在图库包里 source.sqlite 的旁边。
const BUILDING: &str = "source.rescue.sqlite";
/// 新文件的文件夹扫描留给下一次刷新 library.sqlite 的内容（`app_folder` 的日志）。
const BUILDING_JOURNAL: &str = "source.rescue.scan-pending.json";

/// 在损坏文件大小之外，新文件和 SQLite 自己的临时文件可能还需要的空间。
const SPACE_MARGIN: u64 = 256 << 20;
/// 运行期间告诉用户的内容：所处阶段，以及占整个抢救的比例。
pub(crate) type Step<'a> = &'a dyn Fn(&str, f64);
const READING: &str = "正在抢救识别结果数据库：读出还能读的数据";
const SCANNING: &str = "正在抢救识别结果数据库：重新扫描来源文件夹";
const FINISHING: &str = "正在抢救识别结果数据库：检查新数据库";

/// 抢救做了什么，给页面和日志用。
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Report {
    /// 有些行读不出的表。
    pub(crate) damaged: Vec<String>,
    /// 连表的清单都读不出：新文件从空的源库开始。
    pub(crate) schema_lost: bool,
    /// 从损坏文件里读回的图片数。
    pub(crate) salvaged: i64,
    /// 扫描时拿回原来 ID 的图片数。
    pub(crate) kept: i64,
    /// 扫描时以新 ID 找到的图片数：记录里没有它们的 ID。
    pub(crate) new: i64,
    /// 新文件里等待识别的图片数。
    pub(crate) pending: i64,
    /// 损坏文件去了哪里（`install` 之后）。
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

/// 一个完整、检查过的新 source.sqlite，在损坏文件旁边等着。
#[derive(Clone, Debug)]
pub(crate) struct Rebuilt {
    package: PathBuf,
    pub(crate) report: Report,
}

/// 删掉没完成的抢救留在图库包里的东西。
fn discard(package: &Path) {
    for name in [BUILDING.to_owned(), format!("{BUILDING}-journal"), BUILDING_JOURNAL.to_owned()] {
        let _ = fs::remove_file(package.join(name));
    }
}

/// SQLite 列出的损坏文件的结构，用单独的连接读取。
struct Schema {
    page_size: i64,
    auto_vacuum: i64,
    user_version: i64,
    application_id: i64,
    /// (type, name, sql)，按文件自己的顺序。
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

/// 在 `library` 损坏的 source.sqlite 旁边建新文件；图库包里其他东西都不变。每个文件夹来源都必须可读：
/// 不可读的来源里的图片会在之后以新 ID 被扫描进来。
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
    // 新文件建在损坏文件旁边：大小差不多，含索引。在读任何东西之前就说明，而不是中途让 SQLite 报「disk is full」。
    let needed = fs::metadata(library.source())?.len().saturating_add(SPACE_MARGIN);
    let available = crate::platform_filesystem::available_space(library.instance_path())?;
    if needed > available {
        return Err(Error::Invalid(format!(
            "磁盘空间不足：抢救需要约 {needed} MiB，可用 {available} MiB。请先腾出空间再抢救",
            needed = needed.div_ceil(1 << 20),
            available = available >> 20
        )));
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
        // 角色表来自参考图库：读不出的部分从那里取回。
        let lost = report.damaged.iter().filter(|t| ["characters", "character_aliases"].contains(&t.as_str()));
        let lost = lost.cloned().collect::<Vec<_>>();
        if !lost.is_empty() {
            refill(&target, reference, &lost)?;
        }
    }
    crate::app_sync::check(stop)?;
    step(SCANNING, 0.6);
    let earlier = earlier(&target, library, &folders)?;
    // 在所有来源里一起排除重复内容。把已经读过的文件交给扫描，避免重新读取原图。
    let mut batches = Vec::new();
    let mut fingerprints = Vec::new();
    for source in &folders {
        let root = Path::new(source["path"].as_str().ok_or_else(|| Error::invalid("来源缺少路径"))?);
        let (mut files, mut unreadable) = (Vec::new(), Vec::new());
        let check = || crate::app_sync::check(stop);
        crate::platform_folder::walk(root, root, &mut files, &mut unreadable, &check)?;
        let mut checked = Vec::new();
        for mut file in files {
            match crate::platform_folder::hash(&file.path, &check) {
                Ok(hash) => file.hash = hash,
                Err(Error::Io(_)) => {
                    unreadable.push(file.rel);
                    continue;
                }
                Err(e) => return Err(e),
            }
            fingerprints.push((file.path.to_string_lossy().into_owned(), file.hash.clone()));
            checked.push(file);
        }
        batches.push((checked, unreadable));
    }
    let complete = batches.iter().all(|(_, unreadable)| unreadable.is_empty());
    let mut matches = rescue_matches(&target, &folders, &fingerprints, &earlier, complete)?;
    for (i, (source, (files, unreadable))) in folders.iter().zip(batches).enumerate() {
        let share = 0.35 / folders.len() as f64;
        let from = 0.6 + share * i as f64;
        let progress = |done: usize, total: usize| step(SCANNING, from + share * done as f64 / total.max(1) as f64);
        let ids = files
            .iter()
            .filter_map(|f| {
                let path = f.path.to_string_lossy().into_owned();
                Some((path.clone(), matches.remove(&path)?))
            })
            .collect();
        let scan = crate::app_folder::scan_with(
            source,
            &target,
            &|| crate::app_sync::check(stop),
            &progress,
            Some(crate::app_folder::RescueInput { files, ids, unreadable }),
        )?;
        crate::app_relocation::rescued(&target, source)?;
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

/// 损坏文件仍列出的每张表的行，然后是它的索引、触发器和视图。
fn salvage(
    c: &mut Connection,
    damaged: &Path,
    schema: &Schema,
    stop: &AtomicBool,
    step: Step,
    report: &mut Report,
) -> Result<()> {
    c.execute_batch(&format!("PRAGMA page_size={}; PRAGMA auto_vacuum={};", schema.page_size, schema.auto_vacuum))?;
    // 损坏文件用单独的连接：SQLite 在那里遇到的问题不会混进新文件的写入。
    let old = crate::app_library::read_connection(damaged)?;
    let tables = schema
        .entries
        .iter()
        .filter(|(kind, name, _)| kind == "table" && !name.starts_with("sqlite_"))
        .collect::<Vec<_>>();
    let tx = c.transaction()?;
    for (_, name, sql) in &tables {
        // 虚拟表会自己建影子表，而清单里随后又会列出这些影子表。
        if let Err(error) = tx.execute_batch(sql) {
            eprintln!("抢救时没能建表 {name}：{error}");
        }
    }
    // 每张表在读取进度中的份额是它的 rowid 跨度：它可能有的行数。
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

/// 损坏文件里的一张表，以及怎样读回它的行。
struct Table {
    name: String,
    columns: Vec<String>,
    /// 普通表按 rowid 顺序读，这样读取可以越过读不出的页继续。
    rowid: bool,
    /// rowid 是单独的一列（没有 INTEGER PRIMARY KEY 指代它），也一并复制。
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
    /// 把 `old` 里可读的行复制到 `c` 的同名表；有读不出的行时返回 true。
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
            // 没有可以接着读的顺序：只取第一行读不出的行之前的那些。
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
        // 续读指向的那一行：如果它也读不出，就跳过它。
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

/// 抢救出的一行是否写进了新文件。新文件是完好的，所以写入时 SQLite 报损坏，说明这一行自己的内容（只在写入时才从
/// 损坏文件里读取）读不出。
fn written(result: rusqlite::Result<usize>) -> Result<bool> {
    match result {
        Ok(_) => Ok(true),
        Err(e) if crate::app_library::corruption(&e).is_some() => Ok(false),
        Err(e) => Err(e.into()),
    }
}

/// 在 rowid `after` 之后读取失败后：seek 能到达的大于它的最小 rowid（如果有）。seek 先以越来越大的距离试探，
/// 直到落在损坏之后，再逐步收窄到可读的行重新开始的位置。
fn resume(seek: &mut rusqlite::Statement, after: i64) -> Result<Option<i64>> {
    let mut probe =
        |from: i64| -> Option<Option<i64>> { seek.query_row([from], |r| r.get::<_, i64>(0)).optional().ok() };
    // 距离和中点用 i128 计算：rowid 的范围覆盖整个 i64。
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

/// 抢救没能读出的地方，从参考图库取 `tables` 的行，只取匹配的列。
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

/// 新文件和 library.sqlite 对原有 ID 知道些什么。图片行已丢失的文件夹记账行不能留下：它们离开新文件，
/// 对应的图片经扫描以同一个 ID 回来。
fn earlier(target: &Path, library: &Library, folders: &[Value]) -> Result<Earlier> {
    let mut earlier = Earlier::default();
    let roots: HashMap<_, _> = folders
        .iter()
        .filter_map(|s| Some((s["id"].as_str()?.to_owned(), PathBuf::from(s["path"].as_str()?))))
        .collect();
    let has_photos = ["sources", "removed_sources"]
        .iter()
        .any(|key| library.config()[*key].as_array().into_iter().flatten().any(|s| s["kind"] == "photos"));
    // 先读 library.sqlite：它的人工记录指向这些 ID。
    let lib = crate::app_library::read_connection(&library.db())?;
    let hashes = lib
        .prepare("SELECT substr(key,13),value FROM meta WHERE key LIKE 'folder\\_hash:%' ESCAPE '\\'")?
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<HashMap<_, _>>>()?;
    let paths = lib
        .prepare("SELECT uuid,src_path FROM assets WHERE uuid IS NOT NULL ORDER BY uuid")?
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut folder_ids = HashSet::new();
    for (uuid, path) in paths {
        if !has_photos || path.as_deref().is_some_and(|p| roots.values().any(|root| Path::new(p).starts_with(root))) {
            earlier.record(&uuid, path.as_deref(), None);
            folder_ids.insert(uuid);
        }
    }
    for (uuid, hash) in hashes {
        if !has_photos || folder_ids.contains(&uuid) {
            earlier.record(&uuid, None, Some(&hash));
        }
    }
    let c = crate::sqlite::open_with_flags(target, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    let rows = c
        .prepare("SELECT uuid,src_path,stable_hash,source_id FROM assets WHERE uuid IS NOT NULL ORDER BY rowid")?
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (uuid, path, hash, sid) in rows {
        if sid.as_ref().is_some_and(|id| roots.contains_key(id)) {
            earlier.record(&uuid, path.as_deref(), hash.as_deref());
        }
    }
    let records = c
        .prepare("SELECT source_id,path,md5,asset FROM files")?
        .query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (sid, path, hash, uuid) in &records {
        let Some(root) = roots.get(sid) else { continue };
        let full = root.join(Path::new(path).components().collect::<PathBuf>());
        earlier.record(uuid, full.to_str(), Some(hash));
    }
    Ok(earlier)
}

/// 让本次读到的文件按全批匹配重新绑定。保住确已消失、且这次没有认回的旧文件账目，方便以后接回。
fn rescue_matches(
    target: &Path,
    folders: &[Value],
    files: &[(String, String)],
    earlier: &Earlier,
    complete: bool,
) -> Result<HashMap<String, String>> {
    let roots: HashMap<_, _> =
        folders.iter().filter_map(|s| Some((s["id"].as_str()?, Path::new(s["path"].as_str()?)))).collect();
    let mut c = crate::sqlite::open(target)?;
    let tx = c.transaction()?;
    let records = tx
        .prepare("SELECT f.source_id,f.path,f.asset,a.uuid IS NULL FROM files f LEFT JOIN assets a ON a.uuid=f.asset")?
        .query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, bool>(3)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut reserved: HashSet<_> =
        records.iter().filter(|(s, _, _, _)| !roots.contains_key(s.as_str())).map(|(_, _, id, _)| id.clone()).collect();
    reserved.extend(
        tx.prepare("SELECT asset FROM asset_links WHERE system='photos'")?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?,
    );
    let matches = earlier.matches(files, &reserved, complete);
    let assigned: HashSet<_> = matches.values().collect();
    let present: HashSet<_> = files.iter().map(|(path, _)| Path::new(path)).collect();
    for (sid, path, id, orphan) in records {
        let Some(root) = roots.get(sid.as_str()) else { continue };
        if orphan || assigned.contains(&id) || present.contains(root.join(&path).as_path()) {
            tx.execute("DELETE FROM files WHERE source_id=? AND path=?", rusqlite::params![sid, path])?;
            tx.execute(
                "DELETE FROM asset_links WHERE asset=? AND source_id=? AND system IN ('file','md5')",
                rusqlite::params![id, sid],
            )?;
        }
    }
    tx.commit()?;
    Ok(matches)
}

/// 扫描之后：没有被任何扫描找到的文件夹图片按扫描的做法标为已消失；没有任何记录认识的图片的结果丢弃；
/// 没有结果的图片重新识别，所以不能有台账条目或包记录说它们已识别过。
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
    // 识别台账：没有结果的图片若有台账条目，它的新结果会看起来已经导入过；覆盖了这类图片的包记录，会让同一个包再次
    // 识别（同一模型、同样的图片、同样的摘要）时被当作重复，什么也不导入。
    let table = |name: &str| -> Result<bool> {
        Ok(tx.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?)", [name], |r| {
            r.get(0)
        })?)
    };
    if table("recognition_assets")? {
        tx.execute_batch(
            "DELETE FROM recognition_assets WHERE NOT EXISTS(SELECT 1 FROM labels l WHERE l.uuid=recognition_assets.asset AND l.dim='rating')",
        )?;
    }
    if table("recognition_imports")? {
        tx.execute_batch(
            "DELETE FROM recognition_imports WHERE CASE WHEN json_valid(assets) THEN EXISTS(SELECT 1 FROM json_each(recognition_imports.assets) j WHERE NOT EXISTS(SELECT 1 FROM labels l WHERE l.uuid=j.value AND l.dim='rating')) ELSE 1 END",
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

/// 把重建好的文件放到损坏的 source.sqlite 的位置，损坏文件移到 backups/。两个文件都不能被打开（调用方持有图库包，
/// 并已关闭它的连接）。之后记录里的损坏标记被清除。
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
        // 损坏文件放回去：图库包恢复原样。
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

/// 抢救的文件夹扫描把改变的图片留给下一次刷新 library.sqlite，记在新文件的名字下。
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

/// 对调用方持有的图库包 `library` 做一次从头到尾的抢救：先建好，再放到位。
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

/// 会话在后台进行的抢救（#228）：这期间会话保持停止，什么都不写；页面再次打开图库时，新文件放到位。
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
    /// 开始建新文件；已有抢救在运行或已有建好的结果时返回 false。
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
    /// 建好的重建结果等着放到位。
    pub(crate) fn ready(&self) -> bool {
        !self.running() && self.state.lock().expect("rescue state lock poisoned").ready.is_some()
    }
    /// `package` 建好的重建结果，用来放到位。
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
    /// 停止正在运行的抢救，并丢弃从未放到位的已完成结果。
    pub(crate) fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        if let Some(rebuilt) = self.state.lock().expect("rescue state lock poisoned").ready.take() {
            discard(&rebuilt.package);
        }
    }
    /// 给 `GET /api/integrity` 用：`{running: {stage, done} | null, ready, error}`。
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
