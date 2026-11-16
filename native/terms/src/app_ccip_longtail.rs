//! 加载冻结的 CCIP 长尾参考包，扫描已保存的向量，并发布独立的候选批次。
use crate::app_imports::Observer;
use crate::app_library::Library;
use crate::recognition_compute::{
    anchor_snapshot::{Fingerprint, LongtailSnapshot, Reference, Snapshot},
    anchors::Metadata,
};
use crate::{Error, Result};
use illustash_core::ccip_longtail::{self, Hit};
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::HashSet,
    fs,
    io::{Cursor, Read, Write},
    path::{Path, PathBuf},
    time::Instant,
};

pub(crate) const PACKAGE_SHA256: &str = "d36eeab8768e30cc36e2ba5b2788f00a5f608ecad0df264a2830a44c14407fb9";
pub(crate) const PACKAGE_BYTES: u64 = 1_273_692;
pub(crate) const PACKAGE_NAME: &str = "A-longtail-p75.zip";
const CCIP_SHA256: &str = "4ea118d16496274f4f6e08d3afc768cc592389e8f7f32f8732ce2215c228ac5f";
const DIMENSION: usize = 768;
const DELIVERY_ERROR: &str = "ccip_longtail_delivery_error";
const INDEX_GENERATION: &str = "ccip_longtail_index_generation";
const ROLES: usize = 54;

struct Package {
    bytes: Vec<u8>,
    references: Vec<Reference>,
}

#[derive(Deserialize)]
struct Entry {
    id: String,
    tag: String,
    skin: bool,
    post_count: i64,
    gender_conf: f64,
    p50: f32,
    p75: f32,
    p90: f32,
}

fn bad() -> Error {
    Error::invalid("CCIP 长尾参考包与支持的冻结版本不符")
}

fn floats(bytes: &[u8]) -> Option<Vec<f32>> {
    if bytes.len() != DIMENSION * size_of::<f32>() {
        return None;
    }
    let vector = bytes
        .chunks_exact(size_of::<f32>())
        .map(|value| f32::from_le_bytes(value.try_into().expect("每个浮点数为四字节")))
        .collect::<Vec<_>>();
    (vector.iter().all(|v| v.is_finite()) && vector.iter().any(|v| *v != 0.)).then_some(vector)
}

impl Package {
    fn load(path: &Path) -> Result<Self> {
        let bytes = crate::app_recognition::file(path, PACKAGE_BYTES)?;
        if bytes.len() as u64 != PACKAGE_BYTES || crate::strict_json::sha256_hex(&bytes) != PACKAGE_SHA256 {
            return Err(bad());
        }
        let mut archive = zip::ZipArchive::new(Cursor::new(&bytes)).map_err(|_| bad())?;
        let mut member = |name: &str, maximum: u64| -> Result<Vec<u8>> {
            let entry = archive.by_name(name).map_err(|_| bad())?;
            if entry.size() > maximum {
                return Err(bad());
            }
            let mut contents = Vec::new();
            entry.take(maximum + 1).read_to_end(&mut contents)?;
            if contents.len() as u64 > maximum {
                return Err(bad());
            }
            Ok(contents)
        };
        let provenance: Value = serde_json::from_slice(&member("A-provenance.json", 16 * 1024)?).map_err(|_| bad())?;
        if provenance["format"] != "illustash-557-ccip-offline/1" || provenance["ccip_sha256"] != CCIP_SHA256 {
            return Err(bad());
        }
        let metadata = member("A-metadata.json", 128 * 1024)?;
        let packed = member("A-anchors.f32", (ROLES * DIMENSION * size_of::<f32>()) as u64)?;
        for (name, value) in [("A-metadata.json", &metadata), ("A-anchors.f32", &packed)] {
            if provenance["files"][name] != crate::strict_json::sha256_hex(value) {
                return Err(bad());
            }
        }
        let entries: Vec<Entry> = serde_json::from_slice(&metadata).map_err(|_| bad())?;
        if entries.len() != ROLES || packed.len() != ROLES * DIMENSION * size_of::<f32>() {
            return Err(bad());
        }
        let mut ids = HashSet::new();
        let mut tags = HashSet::new();
        let references = entries
            .into_iter()
            .zip(packed.chunks_exact(DIMENSION * size_of::<f32>()))
            .map(|(entry, vector)| {
                if !ids.insert(entry.id.clone())
                    || !tags.insert(entry.tag.clone())
                    || entry.id.is_empty()
                    || entry.tag.is_empty()
                    || !(1..50).contains(&entry.post_count)
                    || !(0.0..=1.0).contains(&entry.gender_conf)
                    || !(0.0..=entry.p75).contains(&entry.p50)
                    || !(entry.p50..=entry.p90).contains(&entry.p75)
                    || !(entry.p75..=1.0).contains(&entry.p90)
                    || entry.p75 <= 0.
                {
                    return Err(bad());
                }
                Ok(Reference {
                    id: entry.id,
                    tag: entry.tag,
                    vector: floats(vector).ok_or_else(bad)?,
                    metadata: Metadata {
                        skin: entry.skin,
                        post_count: entry.post_count,
                        gender_conf: entry.gender_conf,
                        p50: entry.p50,
                        p75: entry.p75,
                        p90: entry.p90,
                    },
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Self { bytes, references })
    }

    fn remember(&self, library: &Library) -> Result<()> {
        let destination = local_package(library);
        if matches!(crate::app_recognition::file(&destination, PACKAGE_BYTES), Ok(previous) if previous == self.bytes) {
            return Ok(());
        }
        let tmp = destination.with_extension(format!("{}.tmp", crate::store::new_key()?));
        let result = (|| {
            let mut file = fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
            file.write_all(&self.bytes)?;
            file.sync_all()?;
            drop(file);
            fs::rename(&tmp, destination)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(tmp);
        }
        result
    }
}

pub(crate) fn local_package(library: &Library) -> PathBuf {
    library.instance_path().join(PACKAGE_NAME)
}

fn writable(library: &Library) -> Result<()> {
    if library.frozen() {
        return Err(Error::Conflict("图库只读".into()));
    }
    Ok(())
}

fn writer(path: &Path) -> Result<Connection> {
    let c = crate::sqlite::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    crate::configure(&c)?;
    c.busy_timeout(crate::app_library::TASK_WRITE_WAIT)?;
    Ok(c)
}

struct ScanRows {
    compatible: Vec<(String, Vec<f32>)>,
    missing: usize,
    incompatible: usize,
}

fn read_rows(source: &Connection, scope: Option<&[String]>, check: &dyn Fn() -> Result<()>) -> Result<ScanRows> {
    let ledger: bool = source.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='recognition_assets')",
        [],
        |r| r.get(0),
    )?;
    let (columns, joins) = if ledger {
        (
            "r.package,i.manifest,i.status",
            "LEFT JOIN recognition_assets r ON a.uuid=r.asset LEFT JOIN recognition_imports i ON r.package=i.digest",
        )
    } else {
        ("NULL,NULL,NULL", "")
    };
    let mut query = source.prepare(&format!(
        "SELECT a.uuid,v.vec,{columns} FROM assets a LEFT JOIN embeddings v ON v.uuid=a.uuid AND v.model='ccip' {joins}
         WHERE a.in_scope=1 AND coalesce(a.trashed,0)=0 AND (?1 IS NULL OR a.uuid IN (SELECT value FROM json_each(?1)))
         ORDER BY a.uuid"
    ))?;
    let scope = scope.map(|ids| json!(ids).to_string());
    let mut rows = query.query([scope.as_deref()])?;
    let mut out = ScanRows { compatible: vec![], missing: 0, incompatible: 0 };
    let mut verified = std::collections::HashMap::<String, bool>::new();
    while let Some(row) = rows.next()? {
        check()?;
        let Some(data) = row.get::<_, Option<Vec<u8>>>(1)? else {
            out.missing += 1;
            continue;
        };
        if let Some(receipt) = row.get::<_, Option<String>>(2)? {
            // 当前批次的源向量可已提交而尚未投影，只有重扫全库时需要等该批次发布。
            let status = row.get::<_, Option<String>>(4)?;
            let ready =
                matches!(status.as_deref(), Some("done")) || scope.is_some() && status.as_deref() == Some("pending");
            let compatible = match verified.get(&receipt) {
                Some(value) => *value,
                None => {
                    let manifest = row.get::<_, Option<String>>(3)?.unwrap_or_default();
                    let value = ready && compatible_model(&manifest);
                    verified.insert(receipt, value);
                    value
                }
            };
            if !compatible {
                out.incompatible += 1;
                continue;
            }
        }
        // 旧图库无逐图账本时，以实际保存的 CCIP 向量为准。
        let Some(vector) = floats(&data) else {
            out.incompatible += 1;
            continue;
        };
        out.compatible.push((row.get(0)?, vector));
    }
    Ok(out)
}

fn compatible_model(raw: &str) -> bool {
    let Ok(manifest) = serde_json::from_str::<Value>(raw) else { return false };
    let model = if manifest.get("recognition").is_some() {
        &manifest["recognition"]["models"]["ccip"]
    } else {
        &manifest["models"]["ccip"]
    };
    model["version"] == "ccip-model_feat" && model["dimension"] == DIMENSION && model["sha256"] == CCIP_SHA256
}

/// 扫描阶段使用只读事务，写入前在来源库租约下复核输入版本。
pub(crate) fn apply(
    library: &Library,
    root: &Path,
    path: &Path,
    check: &dyn Fn() -> Result<()>,
    observer: &dyn Observer,
) -> Result<Value> {
    writable(library)?;
    observer.stage("models");
    let package = Package::load(path)?;
    let shared = crate::app_resources::shared_path(root)?;
    let mut source = writer(&library.source())?;
    let read = source.transaction()?;
    let version: i64 = read.query_row("PRAGMA data_version", [], |r| r.get(0))?;
    let mut app = writer(&library.db())?;
    crate::schema::require_current(&app)?;
    crate::app_recognition::require_published_inputs(&app, &read)?;
    let started = Instant::now();
    let snapshot = LongtailSnapshot::load(&shared, &read, &package.references, check)?;
    let rows = read_rows(&read, None, check)?;
    let count = rows.compatible.len();
    observer.stage("scoring");
    observer.count(0, count as u64);
    let hits = snapshot.scan(&rows.compatible, check, &|done| observer.count(done as u64, count as u64))?;
    let scan_ms = started.elapsed().as_millis();
    read.commit()?;

    observer.stage("committing");
    let publish = Instant::now();
    let source = source.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let generation = crate::index_generation::current(&shared)?;
    if version != source.query_row::<i64, _, _>("PRAGMA data_version", [], |r| r.get(0))?
        || snapshot.anchors.fingerprint != Snapshot::fingerprint(&shared, &source, check)?
        || generation != crate::index_generation::current(&shared)?
    {
        return Err(Error::Conflict("重扫期间图库向量或角色索引已更新，请重新检查".into()));
    }
    crate::app_recognition::require_published_inputs(&app, &source)?;
    check()?;
    package.remember(library)?;
    let tx = app.transaction_with_behavior(TransactionBehavior::Immediate)?;
    crate::schema::begin_writing(&tx)?;
    let changed = ccip_longtail::apply(&tx, PACKAGE_SHA256, &hits)?;
    tx.execute("INSERT OR REPLACE INTO meta(key,value) VALUES (?1,?2)", [INDEX_GENERATION, &generation.to_string()])?;
    tx.execute("DELETE FROM meta WHERE key=?1", [DELIVERY_ERROR])?;
    check()?;
    let mut tx = Some(tx);
    observer.publish(&mut || crate::schema::commit_writing(tx.take().expect("角色参考包只提交一次")))?;
    Ok(json!({"package_sha256":PACKAGE_SHA256,"roles":ROLES,"scored":count,"hits":hits.len(),"changed":changed,
        "missing":rows.missing,"incompatible":rows.incompatible,"scan_ms":scan_ms,"publish_ms":publish.elapsed().as_millis()}))
}

pub(crate) fn withdraw(library: &Library, observer: &dyn Observer) -> Result<Value> {
    observer.stage("committing");
    writable(library)?;
    let mut source = writer(&library.source())?;
    let _source = source.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut app = writer(&library.db())?;
    crate::schema::require_current(&app)?;
    let tx = app.transaction_with_behavior(TransactionBehavior::Immediate)?;
    crate::schema::begin_writing(&tx)?;
    let changed = ccip_longtail::withdraw(&tx)?;
    tx.execute("DELETE FROM meta WHERE key IN (?1,?2)", [DELIVERY_ERROR, INDEX_GENERATION])?;
    let mut tx = Some(tx);
    observer.publish(&mut || crate::schema::commit_writing(tx.take().expect("角色参考包只提交一次")))?;
    Ok(json!({"changed":changed}))
}

pub(crate) fn status(library: &Library, root: &Path) -> Result<Value> {
    let app = crate::app_library::read_connection(&library.db())?;
    let enabled = ccip_longtail::enabled_package(&app)?;
    let available = local_package(library).try_exists()?;
    let candidates: i64 =
        app.query_row("SELECT count(DISTINCT uuid) FROM candidates WHERE source='CCIP_LONGTAIL'", [], |r| r.get(0))?;
    let memberships: i64 =
        app.query_row("SELECT count(*) FROM memberships WHERE sources='CCIP_LONGTAIL'", [], |r| r.get(0))?;
    let error: Option<String> =
        app.query_row("SELECT value FROM meta WHERE key=?1", [DELIVERY_ERROR], |r| r.get(0)).optional()?;
    let needs_scan = if enabled.is_some() {
        let prior: Option<String> =
            app.query_row("SELECT value FROM meta WHERE key=?1", [INDEX_GENERATION], |r| r.get(0)).optional()?;
        let shared = crate::app_resources::shared_path(root)?;
        let generation = crate::index_generation::current(&shared)?;
        prior.as_deref() != Some(generation.to_string().as_str())
    } else {
        false
    };
    Ok(
        json!({"roles":ROLES,"package_sha256":PACKAGE_SHA256,"enabled":enabled,"needs_scan":needs_scan,"available":available,
        "candidates":candidates,"assigned":memberships,"error":error,"writable":!library.frozen()}),
    )
}

fn delivery_snapshot(
    library: &Library,
    shared: &Path,
    app: &Connection,
    source: &Connection,
    check: &dyn Fn() -> Result<()>,
) -> Result<Option<LongtailSnapshot>> {
    if ccip_longtail::enabled_package(app)?.as_deref() != Some(PACKAGE_SHA256)
        || !local_package(library).try_exists()?
    {
        return Ok(None);
    }
    let package = Package::load(&local_package(library))?;
    LongtailSnapshot::load(shared, source, &package.references, check).map(Some)
}

/// 正常识别任务在接收任何批次之前构建一次长尾快照，避免在回执锁里做平方级计算。
pub(crate) fn prepare_delivery(
    library: &Library,
    shared: &Path,
    expected: &Fingerprint,
    check: &dyn Fn() -> Result<()>,
) -> Result<Option<LongtailSnapshot>> {
    let app = crate::app_library::read_connection(&library.db())?;
    let source = crate::app_library::read_connection(&library.source())?;
    let snapshot = delivery_snapshot(library, shared, &app, &source, check)?;
    if snapshot.as_ref().is_some_and(|snap| &snap.anchors.fingerprint != expected) {
        return Err(Error::Conflict("准备识别时基础角色索引已更新".into()));
    }
    Ok(snapshot)
}

pub(crate) fn score_prepared(
    snapshot: &LongtailSnapshot,
    ids: &[String],
    vectors: &[u8],
    models: &Value,
    check: &dyn Fn() -> Result<()>,
) -> Result<Vec<Hit>> {
    let model = &models["ccip"];
    if model["version"] != "ccip-model_feat" || model["dimension"] != DIMENSION || model["sha256"] != CCIP_SHA256 {
        return Err(Error::Conflict("本次识别使用的 CCIP 模型与参考包不兼容".into()));
    }
    if vectors.len() != ids.len() * DIMENSION * size_of::<f32>() {
        return Err(Error::invalid("CCIP 识别向量和图片数量不一致"));
    }
    let rows = ids
        .iter()
        .zip(vectors.chunks_exact(DIMENSION * size_of::<f32>()))
        .filter_map(|(id, bytes)| floats(bytes).map(|v| (id.clone(), v)))
        .collect::<Vec<_>>();
    snapshot.scan(&rows, check, &|_| ())
}

/// 独立结果包或中断后恢复，从当前仍有效的来源向量重建评分。
pub(crate) fn score_delivery(
    library: &Library,
    root: &Path,
    app: &Connection,
    source: &Connection,
    ids: &[String],
    check: &dyn Fn() -> Result<()>,
) -> Result<Option<Vec<Hit>>> {
    if ccip_longtail::enabled_package(app)?.is_none() {
        return Ok(None);
    }
    let shared = crate::app_resources::shared_path(root)?;
    let Some(snapshot) = delivery_snapshot(library, &shared, app, source, check)? else {
        return Ok(None);
    };
    let rows = read_rows(source, Some(ids), check)?;
    Ok(Some(snapshot.scan(&rows.compatible, check, &|_| ())?))
}

/// 可选包损坏、缺少基础索引时仍提交原识别结果，在长尾包的状态中记下具体失败。
pub(crate) fn publish_delivery(app: &Connection, ids: &[String], result: Result<Option<Vec<Hit>>>) -> Result<()> {
    match result {
        Ok(Some(hits)) => {
            ccip_longtail::refresh(app, PACKAGE_SHA256, ids, &hits)?;
        }
        Ok(None) => (),
        Err(error) => {
            let message = error.reason().chars().take(300).collect::<String>();
            app.execute("INSERT OR REPLACE INTO meta(key,value) VALUES (?1,?2)", [DELIVERY_ERROR, &message])?;
        }
    }
    Ok(())
}
