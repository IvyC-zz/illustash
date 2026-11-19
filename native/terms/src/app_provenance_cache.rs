//! 机器缓存和证据交付。源库、缓存、应用三处的提交彼此分开，都可重试。
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
    c.execute_batch("PRAGMA user_version=5; CREATE TABLE IF NOT EXISTS native_delivery(task_id TEXT PRIMARY KEY, package TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'pending', error TEXT NOT NULL DEFAULT '', parts INTEGER NOT NULL DEFAULT 1 CHECK(parts>=1));")?;
    if !has_column(&c, "native_delivery", "error")? {
        c.execute_batch("ALTER TABLE native_delivery ADD COLUMN error TEXT NOT NULL DEFAULT ''")?;
    }
    if !has_column(&c, "native_delivery", "parts")? {
        c.execute_batch("ALTER TABLE native_delivery ADD COLUMN parts INTEGER NOT NULL DEFAULT 1 CHECK(parts>=1)")?;
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
/// 像素结果描述的是计算时用的那份字节。每张图片的内容哈希都会记下；同一 UUID 下字节变了时，丢弃它的查询、
/// 匹配到的帖子、归属和 OCR 标记，让下一次运行重新计算；那次运行之后交付的快照替换按旧字节算出的证据。
/// 第一次见到的图片只记下哈希。这些都是机器结果；裁决在核心里。
pub(super) fn forget_changed_content(c: &Connection, library: &Library) -> Result<bool> {
    let src = crate::app_library::read_connection(&library.source())?;
    // 打开时不要求有这一列；这样的源库没有可比较的内容版本。
    if src.prepare("SELECT stable_hash FROM assets LIMIT 0").is_err() {
        return Ok(false);
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
    let mut changed = false;
    for (asset, hash) in current {
        match seen.get(&asset) {
            Some(old) if *old == hash => continue,
            Some(_) => {
                tx.execute("DELETE FROM lookups WHERE target=?", [&asset])?;
                changed |= tx.execute("DELETE FROM image_sources WHERE asset=?", [&asset])? != 0;
                changed |= tx.execute("DELETE FROM image_artists WHERE asset=?", [&asset])? != 0;
                changed |= tx.execute("DELETE FROM marks WHERE asset=?", [&asset])? != 0;
            }
            None => {}
        }
        tx.execute(
            "INSERT INTO content_versions(asset,hash) VALUES (?1,?2) ON CONFLICT(asset) DO UPDATE SET hash=excluded.hash",
            [&asset, &hash],
        )?;
    }
    tx.commit()?;
    Ok(changed)
}
pub(super) fn backup(from: &Path, to: &Path) -> Result<()> {
    let c = crate::app_library::read_connection(from)?;
    c.backup(rusqlite::MAIN_DB, to, rusqlite::backup::NO_PROGRESS)?;
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
/// 清单覆盖完整快照，查无结果的图也要让上次的证据失效。每份清单都遵守公共证据格式的限额。
pub(super) fn package(library: &Library, cache: &Path, output: &Path, task: &str) -> Result<usize> {
    seal_parts(library, cache, output, task, crate::package::MAX_ASSETS)
}

fn part_path(output: &Path, parts: usize, index: usize) -> std::path::PathBuf {
    if parts == 1 {
        output.to_owned()
    } else {
        output.join(format!("part-{}", index + 1))
    }
}

fn part_id(task: &str, parts: usize, index: usize) -> String {
    format!("{task}/{}/{parts}", index + 1)
}

fn seal_parts(library: &Library, cache: &Path, output: &Path, task: &str, limit: usize) -> Result<usize> {
    let app = crate::app_library::read_connection(&library.db())?;
    let present = app
        .prepare("SELECT uuid FROM assets ORDER BY uuid")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    let c = crate::app_library::read_connection(cache)?;
    let parts = present.len().div_ceil(limit).max(1);
    // 大库缩回单包时也可能再次遇到拆包前的旧指纹；同一份已封包的重试仍按指纹去重。
    let needs_task_id = parts > 1
        || c.query_row("SELECT EXISTS(SELECT 1 FROM native_delivery WHERE parts>1)", [], |r| r.get::<_, bool>(0))?;
    for index in 0..parts {
        let assets = &present[index * limit..present.len().min((index + 1) * limit)];
        let mut evidence = image_evidence(&c, assets)?;
        let terms = if index == 0 { artist_evidence(&c, &mut evidence)? } else { String::new() };
        // 同一次交付的所有分包一并重放。新快照即使某一分包恰好回到更早的内容，也要重新发布该范围。
        let id = needs_task_id.then(|| part_id(task, parts, index));
        write_part(library.id(), &part_path(output, parts, index), assets, id.as_deref(), evidence, terms)?;
    }
    Ok(parts)
}

fn image_evidence(c: &Connection, assets: &[String]) -> Result<String> {
    let mut evidence = String::new();
    let (Some(first), Some(last)) = (assets.first(), assets.last()) else { return Ok(evidence) };
    let mut q = c.prepare(
        "SELECT asset,artist_id,method,score,evidence,status,updated_at FROM image_artists
        WHERE asset BETWEEN ? AND ? ORDER BY asset,artist_id",
    )?;
    let mut rows = q.query([first, last])?;
    while let Some(row) = rows.next()? {
        let asset: String = row.get(0)?;
        if assets.binary_search(&asset).is_err() {
            continue;
        }
        let artist: String = row.get(1)?;
        let method: String = row.get(2)?;
        let score: Option<f64> = row.get(3)?;
        let raw: String = row.get(4)?;
        let status: String = row.get(5)?;
        let updated: String = row.get(6)?;
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
    Ok(evidence)
}

fn artist_evidence(c: &Connection, evidence: &mut String) -> Result<String> {
    for row in c
        .prepare("SELECT id,merged_into FROM artists WHERE merged_into IS NOT NULL ORDER BY id")?
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
    {
        let (a, b) = row?;
        *evidence += &crate::strict_json::canonical(
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
    Ok(terms)
}

fn write_part(
    library_id: &str,
    output: &Path,
    assets: &[String],
    task: Option<&str>,
    evidence: String,
    terms: String,
) -> Result<()> {
    let mut outcomes: Vec<_> = assets
        .iter()
        .map(|id| json!({"asset":{"namespace":"library","id":id},"status":"completed","error":null}))
        .collect();
    // 首行带这次交付的分包标识，重放时可以发现目录互换或同一个分包放了两次。
    if let (Some(first), Some(id)) = (outcomes.first_mut(), task) {
        first["task_id"] = id.into();
    }
    let manifest = json!({"format":"illustash-evidence/1","library_id":library_id,"producer":{"name":"provenance","version":"db-v5"},"created_at":timestamp(""),"assets":outcomes,"files":{"evidence.jsonl":{"bytes":evidence.len(),"sha256":crate::strict_json::sha256_hex(evidence.as_bytes())},"terms.jsonl":{"bytes":terms.len(),"sha256":crate::strict_json::sha256_hex(terms.as_bytes())}}});
    let text = crate::strict_json::canonical(&manifest);
    crate::package::Package::from_texts(&text, &evidence, Some(&terms))?;
    std::fs::create_dir_all(output)?;
    std::fs::write(output.join("evidence.jsonl"), evidence)?;
    std::fs::write(output.join("terms.jsonl"), terms)?;
    std::fs::write(output.join("manifest.json"), format!("{text}\n"))?;
    Ok(())
}
/// 按顺序交付完整快照。核心拒绝内容或分包缺失时把该次交付及原因标为 stale，继续处理后面的快照。
/// 最新结果为 stale 时按当前缓存重封，只列出仍在图库里的图片。数据库忙等暂时失败保持队列原样，等下次重试。
pub(super) fn deliver(library: &Library, cache: &Path) -> Result<Json> {
    if library.frozen() {
        return Ok(Json::Scalar(Value::Null));
    }
    let c = connect(cache)?;
    let rows = c
        .prepare("SELECT task_id,package,parts FROM native_delivery WHERE status='pending' ORDER BY rowid")?
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, usize>(2)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut last = Json::Scalar(Value::Null);
    let mut stale = false;
    for (task, path, parts) in rows {
        (last, stale) = settle(library, &c, &task, &path, parts)?;
    }
    if stale {
        let task = format!("resealed-{}", crate::store::new_key()?);
        let path = format!(".provenance/{task}/evidence");
        let parts = package(library, cache, &library.instance_path().join(&path), &task)?;
        c.execute(
            "INSERT INTO native_delivery(task_id,package,parts) VALUES (?,?,?)",
            rusqlite::params![task, path, parts],
        )?;
        // 重新封好的包再次被拒仍为 stale；下一次研究运行会封一个新包。
        (last, _) = settle(library, &c, &task, &path, parts)?;
    }
    // 目录只做回收；删除失败从不改变交付结果。
    let _ = collect(library.instance_path(), &c, false);
    Ok(last)
}
/// 运行目录 `.provenance/<task>` 的保留规则（#31）。保留待交付的快照及其全部分包，另保留最近一次成功交付的
/// 快照供检查。stale 的快照永远不会导入，原因留在
/// `native_delivery.error`，所以删掉。带 `orphans` 时，没有交付记录的目录也删：这些是失败、被停止或在接收门
/// 之前退出的运行，恢复从不导入它们。调用方持有溯源租约，所以这期间本包没有运行在写目录。返回删了多少个。
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
/// 保存为 `.provenance/<task>/evidence`（相对路径，或包内的绝对路径）的包对应的 `<task>`。
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
/// 交付一个待处理的包：结果是完成、带原因的 stale，或者出错并保持待处理。
fn settle(library: &Library, c: &Connection, task: &str, path: &str, parts: usize) -> Result<(Json, bool)> {
    match import(library, task, path, parts) {
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
fn import(library: &Library, task: &str, path: &str, parts: usize) -> Result<Json> {
    let saved = Path::new(path);
    let path = if saved.is_absolute() { saved.to_owned() } else { library.instance_path().join(saved) };
    let open = |index| -> Result<crate::package::Package> {
        let pkg = crate::package::Package::open(&part_path(&path, parts, index))?;
        if pkg.manifest.producer != "provenance" {
            return crate::error::invalid("不是溯源证据包");
        }
        if parts > 1
            && pkg.manifest.assets.first().and_then(|outcome| outcome.task_id.as_ref())
                != Some(&part_id(task, parts, index))
        {
            return crate::error::invalid("溯源分包与本次交付记录不符");
        }
        Ok(pkg)
    };
    let first = open(0)?;
    let mut app = crate::sqlite::open_with_flags(library.db(), rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    app.busy_timeout(Duration::from_secs(2))?;
    let tx = app.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let mut report = crate::store::import_package_in_transaction(&tx, &first, library.id(), false)?;
    drop(first);
    for index in 1..parts {
        let pkg = open(index)?;
        let part = crate::store::import_package_in_transaction(&tx, &pkg, library.id(), false)?;
        report.added += part.added;
        report.duplicate += part.duplicate;
        report.skipped += part.skipped;
        report.created_terms += part.created_terms;
        report.already &= part.already;
    }
    tx.commit()?;
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
/// 包本身永远导入不了：内容被拒（图片后来被移除、属于另一个图库、文件损坏），或者包已不存在。
/// 用同样的字节重试也会同样失败。
fn refused(e: &Error) -> bool {
    match e {
        Error::Invalid(_) | Error::Conflict(_) => true,
        Error::Io(e) | Error::IoAt { source: e, .. } => e.kind() == std::io::ErrorKind::NotFound,
        _ => false,
    }
}
/// 保持一个只读连接，这样任何原生或外部写入方提交后，SQLite 的 data_version 会让缓存的历史失效；
/// 轮询不会重新打开或解析没变的行。
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
    fn split_delivery_publishes_and_recovers_as_one_snapshot() {
        let root = std::env::temp_dir().join(format!("illustash-split-{}.pilib", crate::store::new_key().unwrap()));
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("library.json"), r#"{"format":1,"id":"synthetic","name":"合成","sources":[]}"#)
            .unwrap();
        let library = Library::locate(&root).unwrap();
        let mut app = crate::open(&library.db()).unwrap();
        crate::migrate(&mut app).unwrap();
        app.execute_batch("CREATE TABLE assets(uuid TEXT PRIMARY KEY); INSERT INTO assets VALUES ('a'),('b'),('c')")
            .unwrap();
        let cache = root.join("provenance.sqlite");
        let c = connect(&cache).unwrap();
        c.execute_batch("INSERT INTO artists(id) VALUES ('synthetic');
            INSERT INTO image_artists(asset,artist_id,method,status) VALUES ('a','synthetic','ocr','auto'),('c','synthetic','ocr','auto');").unwrap();
        let path = ".provenance/first/evidence";
        let output = root.join(path);
        let parts = seal_parts(&library, &cache, &output, "first", 2).unwrap();
        assert_eq!(parts, 2);
        let last = part_path(&output, parts, 1).join("manifest.json");
        let bytes = std::fs::read(&last).unwrap();
        std::fs::remove_file(&last).unwrap();
        assert!(import(&library, "first", path, parts).is_err());
        let count = |table: &str| {
            app.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get::<_, usize>(0)).unwrap()
        };
        assert_eq!(count("term_packages"), 0);
        assert_eq!(count("term_evidence"), 0);
        std::fs::write(last, bytes).unwrap();
        // 把完整、可单独导入的后一包换到首包的位置。每个包的文件校验都会通过。
        let first = part_path(&output, parts, 0);
        let second = part_path(&output, parts, 1);
        let saved = output.join("saved");
        std::fs::rename(&first, &saved).unwrap();
        std::fs::rename(&second, &first).unwrap();
        assert!(
            matches!(import(&library, "first", path, parts), Err(Error::Invalid(reason)) if reason.contains("交付记录"))
        );
        std::fs::rename(&first, second).unwrap();
        std::fs::rename(saved, first).unwrap();
        c.execute(
            "INSERT INTO native_delivery(task_id,package,parts) VALUES ('first',?,?)",
            rusqlite::params![path, parts],
        )
        .unwrap();
        let report = deliver(&library, &cache).unwrap();
        assert_eq!(report.get("added").unwrap().dump(), "2");
        assert_eq!(count("term_current_evidence"), 2);
        c.execute("UPDATE native_delivery SET status='pending'", []).unwrap();
        assert_eq!(deliver(&library, &cache).unwrap().get("already").unwrap().dump(), "true");
        assert_eq!(count("term_packages"), 2);
        c.execute("DELETE FROM image_artists WHERE asset='a'", []).unwrap();
        let path = ".provenance/second/evidence";
        let parts = seal_parts(&library, &cache, &root.join(path), "second", 2).unwrap();
        c.execute(
            "INSERT INTO native_delivery(task_id,package,parts) VALUES ('second',?,?)",
            rusqlite::params![path, parts],
        )
        .unwrap();
        deliver(&library, &cache).unwrap();
        let remaining: String = app
            .query_row("SELECT a.key FROM term_current_evidence e JOIN term_assets a ON a.id=e.asset_id", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(remaining, "c");
        assert_eq!(count("term_evidence"), 2);
        drop((c, app));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn compact_machine_evidence_keeps_v4_core_evidence_and_undo() {
        let root = std::env::temp_dir()
            .join(format!("illustash-evidence-encoding-{}.pilib", crate::store::new_key().unwrap()));
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("library.json"), r#"{"format":1,"id":"synthetic","name":"合成","sources":[]}"#)
            .unwrap();
        let library = Library::locate(&root).unwrap();
        let mut app = crate::open(&library.db()).unwrap();
        crate::migrate(&mut app).unwrap();
        app.execute_batch(
            "CREATE TABLE assets(uuid TEXT PRIMARY KEY); INSERT INTO assets VALUES ('group'),('gallery');",
        )
        .unwrap();
        let cache = root.join("provenance.sqlite");
        let c = connect(&cache).unwrap();
        c.execute("INSERT INTO artists(id) VALUES ('synthetic-artist')", []).unwrap();
        let proofs = [
            (
                "group",
                "group_oc",
                0.9,
                r#"{"group": "oc", "count": 2, "from": ["\u96ea\ud83d\ude42", "a"]}"#,
                json!({"group":"oc","count":2,"from":["雪🙂","a"]}).to_string(),
            ),
            (
                "gallery",
                "gallery_match",
                0.95,
                r#"{"pixiv": "\u96ea\ud83d\ude42\"\\"}"#,
                json!({"pixiv":"雪🙂\"\\"}).to_string(),
            ),
        ];
        for (asset, method, score, old, _) in &proofs {
            c.execute("INSERT INTO image_artists(asset,artist_id,method,score,evidence,status,updated_at) VALUES (?,'synthetic-artist',?,?,?,'candidate','2026-01-01 00:00:00')", rusqlite::params![asset,method,score,old]).unwrap();
        }
        package(&library, &cache, &root.join("old"), "old").unwrap();
        let old_package = crate::package::Package::open(&root.join("old")).unwrap();
        crate::store::import_package(&mut app, &old_package, library.id()).unwrap();
        let evidence = |app: &Connection| -> Vec<Vec<rusqlite::types::Value>> {
            let mut query = app.prepare("SELECT * FROM term_evidence ORDER BY id").unwrap();
            let columns = query.column_count();
            query
                .query_map([], |r| (0..columns).map(|i| r.get(i)).collect())
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap()
        };
        let before = evidence(&app);
        assert_eq!(before.len(), 2);
        assert!(before.iter().all(|row| matches!(&row[6], rusqlite::types::Value::Text(v) if v.starts_with("v4/"))));
        let (asset, term) = app
            .query_row("SELECT asset_id,term_id FROM term_evidence ORDER BY id LIMIT 1", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        let action = crate::store::judge(
            &mut app,
            &[crate::store::VerdictTarget::Image { asset, term }],
            crate::store::Verdict::No,
            "user",
            "",
            "合成裁决",
        )
        .unwrap();
        for (asset, _, _, old, new) in &proofs {
            assert_eq!(serde_json::from_str::<Value>(old).unwrap(), serde_json::from_str::<Value>(new).unwrap());
            c.execute("UPDATE image_artists SET evidence=? WHERE asset=?", [new.as_str(), asset]).unwrap();
        }
        package(&library, &cache, &root.join("new"), "new").unwrap();
        let new_package = crate::package::Package::open(&root.join("new")).unwrap();
        assert_eq!(
            std::fs::read(root.join("old/evidence.jsonl")).unwrap(),
            std::fs::read(root.join("new/evidence.jsonl")).unwrap()
        );
        crate::store::import_package(&mut app, &new_package, library.id()).unwrap();
        assert_eq!(evidence(&app), before);
        assert_eq!(
            app.query_row(
                "SELECT verdict FROM term_effective_verdicts WHERE asset_id=? AND term_id=?",
                [asset, term],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "no"
        );
        crate::store::undo(&mut app, Some(action)).unwrap();
        assert_eq!(
            app.query_row("SELECT count(*) FROM term_effective_verdicts", [], |r| r.get::<_, i64>(0)).unwrap(),
            0
        );
        assert_eq!(evidence(&app), before);
        drop(c);
        drop(app);
        std::fs::remove_dir_all(root).unwrap();
    }

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
        // 保留，只在比较需要颜色检查时才重新计算签名。
        let row: (String, Option<Vec<u8>>) =
            c.query_row("SELECT post_id, colour FROM ref_sigs", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(row, ("1".into(), None));
        drop(c);
        std::fs::remove_dir_all(&root).unwrap();
    }
    #[test]
    fn earlier_delivery_queue_keeps_its_single_packages() {
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
        let row: (String, String, String, usize) = c
            .query_row("SELECT package,status,error,parts FROM native_delivery", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })
            .unwrap();
        assert_eq!(row, ("p".into(), "pending".into(), String::new(), 1));
        drop(c);
        std::fs::remove_dir_all(&root).unwrap();
    }
    #[test]
    fn run_directories_keep_pending_and_the_newest_delivered_package() {
        // #31：只用合成的运行目录和交付记录。
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
        // 一次在接收门之前失败的运行，以及一个用绝对路径保存的包。
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
        // 交付只回收已有定论的包。
        assert_eq!(collect(&root, &c, false).unwrap(), 2);
        assert_eq!(names(), ["absolute", "failed-run", "later-pending", "newest-done", "pending"]);
        // 打开或开始运行时，也回收没有交付记录指向的目录。
        assert_eq!(collect(&root, &c, true).unwrap(), 1);
        assert_eq!(names(), ["absolute", "later-pending", "newest-done", "pending"]);
        // 交付后，待处理的包成为最新的已交付包，前一个被删掉。
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
    /// #31 测量（合成数据，release）：`cargo test --release provenance_delta_bench -- --ignored --nocapture`。
    /// 接收一份暂存的机器缓存（十张表）的耗时，以及一个封好的包的大小。三次运行取中位数。
    /// ILLUSTASH_RESEARCH_BENCH_SIZES 覆盖默认的 1000,10000,50000。
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
            // 一个有 n 张图片的合成图库，以及把每张都归属好的缓存。
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
            package(&library, &base, &sealed, "bench").unwrap();
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
