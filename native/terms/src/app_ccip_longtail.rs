//! 加载冻结的 CCIP 长尾参考包，扫描已保存的向量，并发布独立的候选批次。
use crate::app_imports::Observer;
use crate::app_library::Library;
use crate::app_packs::{Base, Pack};
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
    collections::{HashMap, HashSet},
    fs,
    io::{Cursor, Read, Write},
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::Instant,
};

const PACKAGE_SHA256: &str = "d36eeab8768e30cc36e2ba5b2788f00a5f608ecad0df264a2830a44c14407fb9";
const PACKAGE_BYTES: u64 = 1_273_692;
pub(crate) const PACKAGE_NAME: &str = "A-longtail-p75.zip";
pub(crate) const PACKAGE_VERSION: &str = "ccip-longtail-v1";
const PACKAGE_URL: &str =
    "https://github.com/shio-vinyl/illustash/releases/download/ccip-longtail-v1/A-longtail-p75.zip";
const CCIP_SHA256: &str = "4ea118d16496274f4f6e08d3afc768cc592389e8f7f32f8732ce2215c228ac5f";
const DIMENSION: usize = 768;
const DELIVERY_ERROR: &str = "ccip_longtail_delivery_error";
const INDEX_GENERATION: &str = "ccip_longtail_index_generation";
const LAST_SCAN: &str = "ccip_longtail_last_scan";
const ROLES: usize = 54;
type AutoHistory = HashMap<(PathBuf, String), Value>;
static AUTO_HISTORY: OnceLock<Mutex<AutoHistory>> = OnceLock::new();

/// 参考包的身份：冻结的发布版本，或走查和测试用 `ILLUSTASH_TEST_CCIP_LONGTAIL` 指向的清单换成的本机合成包（#559，
/// 同 WD 扩展包的 `ILLUSTASH_TEST_WD_EXTENSION`）。合成包的文件名、存放目录和发布版一致，角色数和大小不超过发布版，
/// 只能从 127.0.0.1 的 HTTP 端口下载；它记的 CCIP 模型摘要换掉冻结值，好让合成识别结果的向量算兼容。
pub(crate) struct Release {
    pub(crate) sha256: String,
    pub(crate) bytes: u64,
    pub(crate) ccip_sha256: String,
    pub(crate) roles: usize,
    pub(crate) url: String,
}

pub(crate) fn release() -> Result<&'static Release> {
    #[derive(Deserialize)]
    struct Fixture {
        sha256: String,
        size: u64,
        ccip_sha256: String,
        roles: usize,
        url: String,
    }
    static FROZEN: OnceLock<Release> = OnceLock::new();
    static FIXTURE: OnceLock<std::result::Result<Release, String>> = OnceLock::new();
    let frozen = FROZEN.get_or_init(|| Release {
        sha256: PACKAGE_SHA256.into(),
        bytes: PACKAGE_BYTES,
        ccip_sha256: CCIP_SHA256.into(),
        roles: ROLES,
        url: PACKAGE_URL.into(),
    });
    let Some(path) = std::env::var_os("ILLUSTASH_TEST_CCIP_LONGTAIL") else { return Ok(frozen) };
    FIXTURE
        .get_or_init(|| {
            let text = fs::read_to_string(path).map_err(|e| format!("读不到长尾参考包测试清单：{e}"))?;
            let f: Fixture = serde_json::from_str(&text).map_err(|e| format!("长尾参考包测试清单无效：{e}"))?;
            let local = url::Url::parse(&f.url).is_ok_and(|u| {
                u.scheme() == "http"
                    && u.host_str() == Some("127.0.0.1")
                    && u.username().is_empty()
                    && u.password().is_none()
                    && u.port().is_some()
            });
            let hex = |s: &str| s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
            if !local
                || !(1..=ROLES).contains(&f.roles)
                || !(1..=PACKAGE_BYTES).contains(&f.size)
                || !hex(&f.sha256)
                || !hex(&f.ccip_sha256)
            {
                return Err("长尾参考包测试清单无效，下载地址须为 127.0.0.1 的 HTTP 端口".into());
            }
            Ok(Release { sha256: f.sha256, bytes: f.size, ccip_sha256: f.ccip_sha256, roles: f.roles, url: f.url })
        })
        .as_ref()
        .map_err(|message| Error::invalid(message.clone()))
}

fn auto_key(library: &Library) -> (PathBuf, String) {
    (library.instance_path().to_owned(), library.id().into())
}

pub(crate) fn auto_task(library: &Library) -> Option<Value> {
    crate::app_tasks::lock_ignoring_poison(AUTO_HISTORY.get_or_init(Mutex::default)).get(&auto_key(library)).cloned()
}

pub(crate) fn clear_auto_task(library: &Library) {
    crate::app_tasks::lock_ignoring_poison(AUTO_HISTORY.get_or_init(Mutex::default)).remove(&auto_key(library));
}

pub(crate) fn note_auto_task(library: &Library, task: Option<Value>) {
    if let Some(task) = task {
        crate::app_tasks::lock_ignoring_poison(AUTO_HISTORY.get_or_init(Mutex::default))
            .insert(auto_key(library), task);
    }
}

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
    /// 读一个版本的参考包（内置的或目录里的，#602）：大小、SHA-256、CCIP 模型和角色数都对上这个版本。
    fn load(path: &Path, pack: &Pack) -> Result<Self> {
        let Base::Ccip { model } = &pack.base else { return Err(bad()) };
        let (size, roles) = (pack.file.size, pack.heads);
        let bytes = crate::app_recognition::file(path, size)?;
        if bytes.len() as u64 != size || crate::strict_json::sha256_hex(&bytes) != pack.file.sha256 {
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
        if provenance["format"] != crate::app_packs::CCIP_FORMAT || provenance["ccip_sha256"] != model.as_str() {
            return Err(bad());
        }
        // 元数据每个角色约两百字节；内置包 54 个角色的上限照旧是 128 KB。
        let metadata = member("A-metadata.json", (128 * 1024).max(roles as u64 * 2048))?;
        let packed = member("A-anchors.f32", (roles * DIMENSION * size_of::<f32>()) as u64)?;
        for (name, value) in [("A-metadata.json", &metadata), ("A-anchors.f32", &packed)] {
            if provenance["files"][name] != crate::strict_json::sha256_hex(value) {
                return Err(bad());
            }
        }
        let entries: Vec<Entry> = serde_json::from_slice(&metadata).map_err(|_| bad())?;
        if entries.len() != roles || packed.len() != roles * DIMENSION * size_of::<f32>() {
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

    fn remember(&self, destination: &Path) -> Result<()> {
        if matches!(crate::app_recognition::file(destination, self.bytes.len() as u64), Ok(previous) if previous == self.bytes)
        {
            return Ok(());
        }
        fs::create_dir_all(destination.parent().expect("参考包有存放目录"))?;
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

/// 某个版本的参考包在共享数据里的位置：`extensions/<版本>/<文件名>`。
pub(crate) fn package_path(shared: &Path, pack: &Pack) -> PathBuf {
    shared.join("extensions").join(&pack.version).join(&pack.file.name)
}

/// 卡片上的版本（`app_packs::current`）：本图库启用的那个，否则本机已有的最新的，都没有就是最新的。
pub(crate) fn card_pack(root: &Path, library: &Library) -> Result<Pack> {
    let app = crate::app_library::read_connection(&library.db())?;
    let enabled = ccip_longtail::enabled_package(&app)?;
    let shared = crate::app_resources::shared_path(root)?;
    let packs = crate::app_packs::known(root, crate::app_packs::Kind::Ccip)?;
    Ok(crate::app_packs::current(&packs, enabled.as_deref(), |pack| {
        package_path(&shared, pack).try_exists().unwrap_or(false)
    }))
}

/// 本图库启用的版本（在可用列表里的话）。
fn enabled_pack(root: &Path, app: &Connection) -> Result<Option<Pack>> {
    match ccip_longtail::enabled_package(app)? {
        Some(sha) => crate::app_packs::by_sha(root, crate::app_packs::Kind::Ccip, &sha),
        None => Ok(None),
    }
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
    let Ok(release) = release() else { return false };
    model["version"] == "ccip-model_feat"
        && model["dimension"] == DIMENSION
        && model["sha256"] == release.ccip_sha256.as_str()
}

/// 命令行（`ccip-longtail --package`）：按文件的 SHA-256 找它是哪个可用版本。
pub(crate) fn apply_file(
    library: &Library,
    root: &Path,
    path: &Path,
    check: &dyn Fn() -> Result<()>,
    observer: &dyn Observer,
) -> Result<Value> {
    let sha = crate::strict_json::sha256_hex(&crate::app_recognition::file(path, 512 << 20)?);
    let pack = crate::app_packs::by_sha(root, crate::app_packs::Kind::Ccip, &sha)?.ok_or_else(bad)?;
    apply(library, root, &pack, path, check, observer)
}

/// 扫描阶段使用只读事务，写入前在来源库租约下复核输入版本。用别的版本启用过的图库，这一次换成 `pack`（#602）。
pub(crate) fn apply(
    library: &Library,
    root: &Path,
    pack: &Pack,
    path: &Path,
    check: &dyn Fn() -> Result<()>,
    observer: &dyn Observer,
) -> Result<Value> {
    writable(library)?;
    observer.stage("models");
    let package = Package::load(path, pack)?;
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
    package.remember(&package_path(&shared, pack))?;
    let tx = app.transaction_with_behavior(TransactionBehavior::Immediate)?;
    crate::schema::begin_writing(&tx)?;
    let changed = ccip_longtail::apply(&tx, &pack.file.sha256, &hits)?;
    tx.execute("INSERT OR REPLACE INTO meta(key,value) VALUES (?1,?2)", [INDEX_GENERATION, &generation.to_string()])?;
    tx.execute(
        "INSERT INTO meta(key,value) VALUES (?1,json_object('enabled_at',CURRENT_TIMESTAMP,'checked_at',CURRENT_TIMESTAMP,'scored',?2))
         ON CONFLICT(key) DO UPDATE SET value=json_set(meta.value,'$.checked_at',CURRENT_TIMESTAMP,'$.scored',?2)",
        rusqlite::params![LAST_SCAN, count],
    )?;
    tx.execute("DELETE FROM meta WHERE key=?1", [DELIVERY_ERROR])?;
    check()?;
    let mut tx = Some(tx);
    observer.publish(&mut || crate::schema::commit_writing(tx.take().expect("角色参考包只提交一次")))?;
    Ok(json!({"package_sha256":pack.file.sha256,"roles":pack.heads,"scored":count,"hits":hits.len(),"changed":changed,
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
    tx.execute("DELETE FROM meta WHERE key IN (?1,?2,?3)", [DELIVERY_ERROR, INDEX_GENERATION, LAST_SCAN])?;
    let mut tx = Some(tx);
    observer.publish(&mut || crate::schema::commit_writing(tx.take().expect("角色参考包只提交一次")))?;
    Ok(json!({"changed":changed}))
}

pub(crate) fn stored_vectors(library: &Library) -> Result<i64> {
    let source = crate::app_library::read_connection(&library.source())?;
    Ok(source.query_row(
        "SELECT count(*) FROM embeddings v JOIN assets a ON a.uuid=v.uuid
         WHERE v.model='ccip' AND a.in_scope=1 AND coalesce(a.trashed,0)=0",
        [],
        |r| r.get(0),
    )?)
}

pub(crate) fn status(library: &Library, root: &Path) -> Result<Value> {
    let app = crate::app_library::read_connection(&library.db())?;
    let enabled = ccip_longtail::enabled_package(&app)?;
    let shared = crate::app_resources::shared_path(root)?;
    let pack = card_pack(root, library)?;
    let available = package_path(&shared, &pack).try_exists()?;
    let candidates: i64 =
        app.query_row("SELECT count(DISTINCT uuid) FROM candidates WHERE source='CCIP_LONGTAIL'", [], |r| r.get(0))?;
    let memberships: i64 =
        app.query_row("SELECT count(*) FROM memberships WHERE sources='CCIP_LONGTAIL'", [], |r| r.get(0))?;
    let error: Option<String> =
        app.query_row("SELECT value FROM meta WHERE key=?1", [DELIVERY_ERROR], |r| r.get(0)).optional()?;
    let (enabled_at, checked_at, scored): (Option<String>, Option<String>, i64) = app
        .query_row(
            "SELECT json_extract(value,'$.enabled_at'),json_extract(value,'$.checked_at'),json_extract(value,'$.scored') FROM meta WHERE key=?1",
            [LAST_SCAN],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?
        .unwrap_or_default();
    let vectors = stored_vectors(library)?;
    let needs_scan = if enabled.is_some() {
        let prior: Option<String> =
            app.query_row("SELECT value FROM meta WHERE key=?1", [INDEX_GENERATION], |r| r.get(0)).optional()?;
        let generation = crate::index_generation::current(&shared)?;
        prior.as_deref() != Some(generation.to_string().as_str())
    } else {
        false
    };
    Ok(json!({"roles":pack.heads,"package_sha256":pack.file.sha256,"package_bytes":pack.file.size,"enabled":enabled,
        "needs_scan":needs_scan,"available":available,"candidates":candidates,"assigned":memberships,"error":error,
        "writable":!library.frozen(),"vectors":vectors,"enabled_at":enabled_at,"checked_at":checked_at,"scored":scored}))
}

fn delivery_snapshot(
    root: &Path,
    shared: &Path,
    app: &Connection,
    source: &Connection,
    check: &dyn Fn() -> Result<()>,
) -> Result<Option<LongtailSnapshot>> {
    let Some(pack) = enabled_pack(root, app)? else { return Ok(None) };
    let path = package_path(shared, &pack);
    if !path.try_exists()? {
        return Ok(None);
    }
    let package = Package::load(&path, &pack)?;
    LongtailSnapshot::load(shared, source, &package.references, check).map(Some)
}

/// 正常识别任务在接收任何批次之前构建一次长尾快照，避免在回执锁里做平方级计算。
pub(crate) fn prepare_delivery(
    library: &Library,
    root: &Path,
    shared: &Path,
    expected: &Fingerprint,
    check: &dyn Fn() -> Result<()>,
) -> Result<Option<LongtailSnapshot>> {
    let app = crate::app_library::read_connection(&library.db())?;
    let source = crate::app_library::read_connection(&library.source())?;
    let snapshot = delivery_snapshot(root, shared, &app, &source, check)?;
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
    if model["version"] != "ccip-model_feat"
        || model["dimension"] != DIMENSION
        || model["sha256"] != release()?.ccip_sha256.as_str()
    {
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
    let Some(snapshot) = delivery_snapshot(root, &shared, app, source, check)? else {
        return Ok(None);
    };
    let rows = read_rows(source, Some(ids), check)?;
    Ok(Some(snapshot.scan(&rows.compatible, check, &|_| ())?))
}

/// 可选包损坏、缺少基础索引时仍提交原识别结果，在长尾包的状态中记下具体失败。
pub(crate) fn publish_delivery(
    app: &Connection,
    digest: &str,
    ids: &[String],
    result: Result<Option<Vec<Hit>>>,
) -> Result<Option<Value>> {
    let Some(enabled) = ccip_longtail::enabled_package(app)? else {
        return Ok(None);
    };
    let task = match result {
        Ok(Some(hits)) => {
            let changed = ccip_longtail::refresh(app, &enabled, ids, &hits)?;
            app.execute("DELETE FROM meta WHERE key=?1", [DELIVERY_ERROR])?;
            json!({"id":digest,"kind":"auto","phase":"write","status":"done","result":{"changed":changed}})
        }
        Ok(None) => return Ok(None),
        Err(error) => {
            let message = error.reason().chars().take(300).collect::<String>();
            app.execute("INSERT OR REPLACE INTO meta(key,value) VALUES (?1,?2)", [DELIVERY_ERROR, &message])?;
            let diagnostic = crate::diagnostics::failure(
                crate::diagnostics::Operation::Recognition,
                crate::diagnostics::stage("scoring"),
                &error,
                crate::diagnostics::Publication::NotCommitted,
            );
            json!({"id":digest,"kind":"auto","phase":"scan","status":"failed","message":message,"diagnostic":diagnostic.value()})
        }
    };
    Ok(Some(task))
}
