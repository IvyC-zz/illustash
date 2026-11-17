//! 用冻结的角色扩展头检查已存的 WD 向量。包可以是内置的 `wd-extension-v1`，也可以是签名目录里列出的新版本（#602，
//! `app_packs`）；同一图库从一个版本换到另一个，沿用同一个 `WD_EXT` 来源的替换流程，图库里记下用的是哪个包。
use crate::app_library::Library;
use crate::app_packs::{Base, Pack, WD_FORMAT_V1, WD_FORMAT_V2};
use crate::{Error, Result};
use illustash_core::wd_extension::{self, Hit};
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior};
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

const DIM: usize = 1024;

/// 包文件第一行的清单。`illustash-wd-extension/1`：统一的 `threshold`，可以另带逐头的 `thresholds`（#551 的 257 头包），
/// 全部是一级头。`/2`：必须带逐头的 `thresholds` 和 `tiers`（1 可以补主归属，2 只出候选）。
#[derive(Deserialize)]
struct Manifest {
    format: String,
    wd_model_sha256: String,
    wd_tags_sha256: String,
    #[serde(default)]
    threshold: Option<f64>,
    labels: Vec<String>,
    #[serde(default)]
    thresholds: Option<Vec<f64>>,
    #[serde(default)]
    tiers: Option<Vec<u8>>,
}

struct Package {
    wd_model_sha256: String,
    wd_tags_sha256: String,
    labels: Vec<String>,
    thresholds: Vec<f64>,
    /// 每个头是不是二级（只出候选）。
    candidate_only: Vec<bool>,
    weights: Vec<f32>,
    bias: Vec<f32>,
}

fn float32(bytes: &[u8]) -> Vec<f32> {
    bytes.chunks_exact(4).map(|b| f32::from_le_bytes(b.try_into().expect("四字节分块"))).collect()
}

impl Package {
    /// 读一个版本的包：大小和 SHA-256 先对上这个版本，再按格式解开。
    fn load(path: &Path, pack: &Pack) -> Result<Self> {
        let input = crate::app_recognition::file(path, pack.file.size)?;
        if input.len() as u64 != pack.file.size || crate::strict_json::sha256_hex(&input) != pack.file.sha256 {
            return Err(Error::invalid("WD 扩展包与这个版本的冻结文件不符"));
        }
        Self::decode(&input, pack)
    }

    fn decode(input: &[u8], pack: &Pack) -> Result<Self> {
        let end = input.iter().position(|&byte| byte == b'\n').ok_or_else(|| Error::invalid("缺少 WD 扩展包清单"))?;
        let manifest: Manifest =
            serde_json::from_slice(&input[..end]).map_err(|e| Error::invalid(format!("WD 扩展包清单无效：{e}")))?;
        let coefficients = &input[end + 1..];
        let heads = pack.heads;
        // 目录里的包，模型和标签表都要是目录写的那两个。内置包只看模型（models.json 只写了它，和以前一样）；
        // 标签表和每次交付对不对得上，扫描时逐批核对（`compatible`）。
        let base_matches = matches!(&pack.base, Base::Wd { model, tags }
            if *model == manifest.wd_model_sha256 && (pack.builtin || *tags == manifest.wd_tags_sha256));
        if manifest.format != pack.format
            || !base_matches
            || manifest.labels.len() != heads
            || coefficients.len() != heads * (DIM + 1) * 4
        {
            return Err(Error::invalid("WD 扩展包格式不符"));
        }
        let (thresholds, tiers) = match manifest.format.as_str() {
            WD_FORMAT_V1 if manifest.tiers.is_none() => match (manifest.thresholds, manifest.threshold) {
                (Some(values), Some(_)) => (values, vec![1; heads]),
                (None, Some(value)) => (vec![value; heads], vec![1; heads]),
                _ => return Err(Error::invalid("WD 扩展包缺少阈值")),
            },
            WD_FORMAT_V2 => match (manifest.thresholds, manifest.tiers) {
                (Some(values), Some(tiers)) => (values, tiers),
                _ => return Err(Error::invalid("WD 扩展包缺少逐头阈值或级别")),
            },
            _ => return Err(Error::invalid("WD 扩展包格式不符")),
        };
        if thresholds.len() != heads || tiers.len() != heads {
            return Err(Error::invalid("WD 扩展包阈值或级别数目不符"));
        }
        if thresholds.iter().any(|value| !(0.0..=1.0).contains(value)) || tiers.iter().any(|t| !matches!(t, 1 | 2)) {
            return Err(Error::invalid("WD 扩展包阈值或级别不合法"));
        }
        let candidate_only = tiers.iter().map(|&t| t == 2).collect::<Vec<_>>();
        if candidate_only.iter().filter(|&&c| c).count() != pack.candidate_only {
            return Err(Error::invalid("WD 扩展包的二级头数和目录不符"));
        }
        let (weights, bias) = coefficients.split_at(heads * DIM * 4);
        Ok(Self {
            wd_model_sha256: manifest.wd_model_sha256,
            wd_tags_sha256: manifest.wd_tags_sha256,
            labels: manifest.labels,
            thresholds,
            candidate_only,
            weights: float32(weights),
            bias: float32(bias),
        })
    }

    fn compatible(&self, manifest: &str) -> Result<()> {
        let doc: Value =
            serde_json::from_str(manifest).map_err(|e| Error::invalid(format!("识别账本清单无效：{e}")))?;
        // 带角色认领的交付把公开清单包在 recognition 字段里，其他交付直接存公开清单。
        let manifest = if doc.get("format").is_some() { &doc } else { &doc["recognition"] };
        let wd = match manifest["format"].as_str() {
            Some("illustash-recognition/1" | "illustash-recognition/2") => &manifest["models"]["wd"],
            _ => &Value::Null,
        };
        if wd["version"] != "swinv2_448"
            || wd["dimension"].as_u64() != Some(DIM as u64)
            || wd["sha256"].as_str() != Some(&self.wd_model_sha256)
            || wd["tags_sha256"].as_str() != Some(&self.wd_tags_sha256)
        {
            return Err(Error::Conflict("WD 扩展包与图库向量的底座模型或标签表版本不匹配".into()));
        }
        Ok(())
    }

    fn hits(&self, uuid: &str, bytes: &[u8], out: &mut Vec<Hit>) -> Result<()> {
        if bytes.len() != DIM * 4 {
            return Err(Error::invalid("WD 特征向量维数不正确"));
        }
        let vector = float32(bytes);
        if vector.iter().any(|v| !v.is_finite()) {
            return Err(Error::invalid("WD 特征向量包含非有限值"));
        }
        let heads = self.labels.iter().zip(self.weights.chunks_exact(DIM)).zip(&self.bias);
        for ((((tag, weights), bias), &threshold), &candidate_only) in
            heads.zip(&self.thresholds).zip(&self.candidate_only)
        {
            // 累加与 sigmoid 用 f64，阈值与证据分数沿用冻结实验的 f32 概率精度。
            let sum: f64 = vector.iter().zip(weights).map(|(&x, &w)| f64::from(x) * f64::from(w)).sum();
            let probability = (1.0 / (1.0 + (-(sum + f64::from(*bias))).exp())) as f32;
            if f64::from(probability) >= threshold {
                let probability = f64::from(probability);
                out.push(Hit { uuid: uuid.to_owned(), tag: tag.clone(), probability, candidate_only });
            }
        }
        Ok(())
    }
}

fn writer(path: &Path) -> Result<Connection> {
    let conn = crate::sqlite::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    crate::configure(&conn)?;
    conn.busy_timeout(crate::app_library::TASK_WRITE_WAIT)?;
    Ok(conn)
}

const IN_SCOPE: &str = "v.model='wd' AND a.in_scope=1 AND coalesce(a.trashed,0)=0";
const VECTORS: &str = "FROM embeddings v JOIN assets a ON a.uuid=v.uuid";
const SCOPE: &str = "AND (?1 IS NULL OR v.uuid IN (SELECT value FROM json_each(?1)))";
const META_KEY: &str = "wd_extension";
/// 角色资料包默认启用以后（#602），本图库明确不用：提示里选了不用，或撤回过。启用时清掉。
const DECLINED_KEY: &str = "character_pack_declined";

/// 本图库是不是明确不用角色资料包（`app_packs::default_state`）。
pub(crate) fn declined(app: &Connection) -> Result<bool> {
    Ok(app.query_row("SELECT EXISTS(SELECT 1 FROM meta WHERE key=?1)", [DECLINED_KEY], |r| r.get(0))?)
}

/// 记下或清掉「本图库不用角色资料包」。
pub(crate) fn set_declined(library: &Library, declined: bool) -> Result<()> {
    ensure_writable(library)?;
    let mut app = writer(&library.db())?;
    crate::schema::require_current(&app)?;
    let tx = app.transaction_with_behavior(TransactionBehavior::Immediate)?;
    crate::schema::begin_writing(&tx)?;
    if declined {
        tx.execute("INSERT OR REPLACE INTO meta(key,value) VALUES (?1,CURRENT_TIMESTAMP)", [DECLINED_KEY])?;
    } else {
        tx.execute("DELETE FROM meta WHERE key=?1", [DECLINED_KEY])?;
    }
    crate::schema::commit_writing(tx)
}

fn has_table(src: &Connection, name: &str) -> Result<bool> {
    Ok(src
        .query_row("SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?)", [name], |r| r.get(0))?)
}

/// 本图库存着的 WD 向量张数（角色资料包启用时决定现在检查还是等识别结果，#602）。
pub(crate) fn stored_vectors(library: &Library) -> Result<usize> {
    vector_count(&crate::app_library::read_connection(&library.source())?, None)
}

fn vector_count(src: &Connection, scope: Option<&str>) -> Result<usize> {
    if !has_table(src, "embeddings")? {
        return Ok(0);
    }
    Ok(src.query_row(&format!("SELECT count(*) {VECTORS} WHERE {IN_SCOPE} {SCOPE}"), [scope], |r| r.get(0))?)
}

fn scan(
    src: &Connection,
    package: &Package,
    scope: Option<&str>,
    progress: &Progress<'_>,
) -> Result<(usize, Vec<Hit>)> {
    let total = vector_count(src, scope)?;
    progress.count(0, total)?;
    if total == 0 {
        return Ok((0, Vec::new()));
    }
    let has_ledger = has_table(src, "recognition_assets")?;
    if has_ledger != has_table(src, "recognition_imports")? {
        return Err(Error::Schema("WD 识别账本不完整".into()));
    }
    let sql = if has_ledger {
        format!(
            "SELECT v.uuid,v.vec,r.package,i.manifest,i.status {VECTORS}
                 LEFT JOIN recognition_assets r ON r.asset=a.uuid LEFT JOIN recognition_imports i ON i.digest=r.package
                 WHERE {IN_SCOPE} {SCOPE} ORDER BY v.uuid"
        )
    } else {
        format!("SELECT v.uuid,v.vec,NULL,NULL,NULL {VECTORS} WHERE {IN_SCOPE} {SCOPE} ORDER BY v.uuid")
    };
    let mut seen = std::collections::HashSet::new();
    let mut hits = Vec::new();
    let mut checked = 0;
    let mut stmt = src.prepare(&sql)?;
    let mut rows = stmt.query([scope])?;
    while let Some(row) = rows.next()? {
        progress.check()?;
        if let Some(delivery) = row.get::<_, Option<String>>(2)? {
            if row.get::<_, Option<String>>(4)?.as_deref() != Some("done") {
                return Err(Error::Conflict("WD 识别交付尚未完成，请先恢复这次交付".into()));
            }
            if seen.insert(delivery) {
                package.compatible(&row.get::<_, String>(3)?)?;
            }
        }
        // 旧图库无逐图交付账本时，使用已经存储的 WD 向量。
        package.hits(&row.get::<_, String>(0)?, &row.get::<_, Vec<u8>>(1)?, &mut hits)?;
        checked += 1;
        if checked % 32 == 0 || checked == total {
            progress.count(checked, total)?;
        }
    }
    Ok((checked, hits))
}

#[derive(serde::Serialize, Deserialize)]
struct Enabled {
    package_sha256: String,
    enabled_at: String,
    checked_at: String,
    scored: usize,
}

fn enabled(c: &Connection) -> Result<Option<Enabled>> {
    let raw: Option<String> = c.query_row("SELECT value FROM meta WHERE key=?", [META_KEY], |r| r.get(0)).optional()?;
    raw.map(|value| serde_json::from_str(&value).map_err(|e| Error::Schema(format!("WD 扩展状态无效：{e}"))))
        .transpose()
}

/// 命令行（`wd-extension --package`）：按文件的 SHA-256 找它是哪个可用版本，用它检查整个图库。
pub fn apply(library: &Library, path: &Path) -> Result<Value> {
    ensure_writable(library)?;
    let resources = crate::app_resources::default_root();
    let bytes = crate::app_recognition::file(path, 512 << 20)?;
    let sha = crate::strict_json::sha256_hex(&bytes);
    let pack = crate::app_packs::component_by_sha(&resources, crate::app_packs::Kind::Wd, &sha)?
        .ok_or_else(|| Error::invalid("WD 扩展包与可用的版本都不符"))?;
    apply_loaded(library, &Package::load(path, &pack)?, &pack.file.sha256, None, &Progress::silent())
}

fn apply_loaded(
    library: &Library,
    package: &Package,
    sha256: &str,
    scope: Option<&[String]>,
    progress: &Progress<'_>,
) -> Result<Value> {
    let mut source = writer(&library.source())?;
    let scope_json = scope.map(|ids| json!(ids).to_string());
    // 先读取版本，再打开评分快照。取得写锁后再核对版本，评分无需占用来源库写锁。
    let version =
        |c: &Connection| -> rusqlite::Result<i64> { c.pragma_query_value(None, "data_version", |r| r.get(0)) };
    let before = version(&source)?;
    let snapshot = source.transaction()?;
    let (checked, hits) = scan(&snapshot, package, scope_json.as_deref(), progress)?;
    snapshot.commit()?;
    if checked == 0 && scope.is_none() {
        return Err(Error::Conflict("这个图库没有已保存的 WD 向量".into()));
    }
    progress.check()?;
    let stable = source.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if version(&stable)? != before {
        return Err(Error::Conflict("检查期间图库来源有变化，请重新检查".into()));
    }
    progress.begin_write()?;
    let mut app = writer(&library.db())?;
    crate::schema::require_current(&app)?;
    let tx = app.transaction_with_behavior(TransactionBehavior::Immediate)?;
    crate::schema::begin_writing(&tx)?;
    let current = enabled(&tx)?;
    let changed = if let Some(ids) = scope {
        if current.as_ref().is_some_and(|state| state.package_sha256 == sha256) {
            wd_extension::apply_scoped(&tx, sha256, ids, &hits)?
        } else {
            0
        }
    } else {
        let now: String = tx.query_row("SELECT datetime('now')", [], |r| r.get(0))?;
        let entry = Enabled {
            package_sha256: sha256.into(),
            enabled_at: current.map_or_else(|| now.clone(), |state| state.enabled_at),
            checked_at: now,
            scored: checked,
        };
        let value = serde_json::to_string(&entry).expect("WD 扩展状态可序列化");
        tx.execute("INSERT OR REPLACE INTO meta(key,value) VALUES (?,?)", (META_KEY, value))?;
        wd_extension::apply(&tx, sha256, &hits)?
    };
    crate::schema::commit_writing(tx)?;
    Ok(json!({"package_sha256":sha256,"scored":checked,"hits":hits.len(),"changed":changed}))
}

pub fn withdraw(library: &Library) -> Result<Value> {
    ensure_writable(library)?;
    let mut app = writer(&library.db())?;
    crate::schema::require_current(&app)?;
    let tx = app.transaction_with_behavior(TransactionBehavior::Immediate)?;
    crate::schema::begin_writing(&tx)?;
    let changed = wd_extension::withdraw(&tx)?;
    tx.execute("DELETE FROM meta WHERE key=?", [META_KEY])?;
    crate::schema::commit_writing(tx)?;
    Ok(json!({"changed":changed}))
}

/// 本图库启用的、本机就位的包（自动检查和角色检查用）。启用的版本不在可用列表里、或包不在本机时为 None。
pub(crate) fn enabled_pack(resources: &Path, app: &Connection) -> Result<Option<(Pack, PathBuf)>> {
    let Some(active) = enabled(app)? else { return Ok(None) };
    let Some(pack) = crate::app_packs::component_by_sha(resources, crate::app_packs::Kind::Wd, &active.package_sha256)?
    else {
        return Ok(None);
    };
    Ok(crate::app_models::extension_file(resources, &pack)?.map(|path| (pack, path)))
}

/// 角色检查使用图库的持久启用状态和本机已校验的包（#561）。包在本机缺席时仍按未覆盖处理。二级头（#602）只出候选，
/// 不算已经认得的角色。
pub(crate) fn covered_labels(app: &Connection, resources: &Path) -> Result<Vec<String>> {
    let Some((pack, path)) = enabled_pack(resources, app)? else { return Ok(Vec::new()) };
    Ok(Package::load(&path, &pack)
        .map(|p| p.labels.into_iter().zip(p.candidate_only).filter(|(_, c)| !c).map(|(label, _)| label).collect())
        .unwrap_or_default())
}

fn ensure_writable(library: &Library) -> Result<()> {
    if library.frozen() {
        return Err(Error::Conflict("图库只读".into()));
    }
    Ok(())
}

use crate::app_tasks::{lock_ignoring_poison, Task, TaskContext, TaskIdentity, TaskOutcome};
use std::sync::{atomic::Ordering, Arc, Mutex};

struct Progress<'a>(Option<(&'a TaskContext, &'a Mutex<Value>)>);

impl Progress<'_> {
    fn silent() -> Self {
        Self(None)
    }

    fn check(&self) -> Result<()> {
        if let Some((ctx, _)) = self.0 {
            crate::app_sync::check(ctx.cancellation())?;
        }
        Ok(())
    }

    fn count(&self, done: usize, total: usize) -> Result<()> {
        self.check()?;
        if let Some((_, state)) = self.0 {
            let mut state = lock_ignoring_poison(state);
            state["done"] = done.into();
            state["total"] = total.into();
        }
        Ok(())
    }

    fn begin_write(&self) -> Result<()> {
        if let Some((_, state)) = self.0 {
            let mut state = lock_ignoring_poison(state);
            // 此锁也保护 HTTP 取消。先进入写入的任务必须完成提交或回滚。
            self.check()?;
            state["phase"] = "write".into();
        }
        Ok(())
    }
}

#[derive(Default)]
pub(crate) struct Tasks {
    active: Option<(Task, Arc<Mutex<Value>>)>,
    previous: Option<Value>,
}

impl Tasks {
    pub(crate) fn restore(&mut self, previous: Option<Value>) {
        self.previous = previous;
    }

    pub(crate) fn running(&self) -> bool {
        self.active.as_ref().is_some_and(|(task, _)| task.running())
    }

    pub(crate) fn stop(&mut self) {
        if let Some((task, _)) = &mut self.active {
            task.stop();
        }
    }

    pub(crate) fn snapshot(&self) -> Value {
        let Some((task, state)) = &self.active else { return self.previous.clone().unwrap_or(Value::Null) };
        let mut snap = lock_ignoring_poison(state).clone();
        if snap["status"] == "running" && !task.running() {
            if let TaskOutcome::Failed(message) = task.outcome() {
                snap["status"] = "failed".into();
                snap["message"] = message.into();
            }
        }
        snap
    }

    /// 取消扫描阶段的任务。任务已经结束（取消和完成擦肩而过）时什么都不做，页面读到的是结束后的状态；只有写入阶段
    /// 回 409（#559 走查）。
    pub(crate) fn cancel(&self, id: &str) -> Result<()> {
        if let Some((task, state)) = &self.active {
            let snap = lock_ignoring_poison(state);
            if snap["id"] == id && snap["status"] == "running" {
                if snap["phase"] != "scan" {
                    return Err(Error::Conflict("正在写入图库，无法取消".into()));
                }
                task.context().cancellation().store(true, Ordering::SeqCst);
                return Ok(());
            }
        }
        Ok(())
    }

    pub(crate) fn start_for(&mut self, library: &Library, identity: TaskIdentity, operation: Action) -> Result<Value> {
        if self.running() {
            return Err(Error::Conflict("已有 WD 角色扩展任务正在进行".into()));
        }
        let (kind, phase) = match &operation {
            Action::Enable(..) | Action::Pack { .. } => ("enable", "scan"),
            Action::Auto(..) => ("auto", "scan"),
            Action::Withdraw | Action::PackWithdraw { .. } => ("withdraw", "write"),
        };
        // `part` 是角色资料包正在处理的组成部分（`wd`、`ccip`），只给 WD 部分的旧任务也写 `wd`。
        let initial = json!({
            "id":identity.task_id,"kind":kind,"phase":phase,"status":"running","part":"wd",
            "done":0,"total":0,"message":null,"result":null,"diagnostic":null
        });
        let state = Arc::new(Mutex::new(initial.clone()));
        let shared = state.clone();
        let library = library.clone();
        let task = Task::start(identity, None, move |ctx| {
            let progress = Progress(Some((&ctx, &shared)));
            // 角色资料包分两步提交：WD 部分提交以后 CCIP 部分失败或取消时，结果里留着 WD 部分的（`result.wd`）。
            let mut done = Value::Null;
            let result = match operation {
                Action::Enable(pack, path) => check_file(&library, &pack, &path, None, &progress),
                Action::Auto(pack, path, ids) => check_file(&library, &pack, &path, Some(&ids), &progress),
                Action::Withdraw => withdraw(&library),
                Action::Pack { root, wd, ccip } => pack_enable(&library, &root, wd, ccip, &progress, &mut done),
                Action::PackWithdraw { wd, ccip } => pack_withdraw(&library, wd, ccip, &progress, &mut done),
            };
            let mut state = lock_ignoring_poison(&shared);
            if !done.is_null() {
                state["result"] = done;
            }
            match result {
                Ok(result) => {
                    state["status"] = "done".into();
                    state["result"] = result;
                }
                Err(_) if state["phase"] == "scan" && progress.check().is_err() => {
                    state["status"] = "cancelled".into();
                    state["message"] = "WD 角色扩展检查已取消".into();
                }
                Err(error) => {
                    use crate::diagnostics::{self, Operation, Publication, Stage};
                    let stage = if state["phase"] == "scan" { Stage::Reading } else { Stage::Writing };
                    let d = diagnostics::failure(Operation::Models, stage, &error, Publication::NotCommitted);
                    state["status"] = "failed".into();
                    state["message"] = match error {
                        Error::Conflict(message) | Error::Invalid(message) | Error::Schema(message) => message.into(),
                        _ => "WD 角色扩展任务失败，请查看诊断信息".into(),
                    };
                    state["diagnostic"] = d.value();
                }
            }
            Ok(())
        });
        self.previous = None;
        self.active = Some((task, state));
        Ok(initial)
    }
}

pub(crate) enum Action {
    /// 用这个版本检查整个图库：启用、重新检查，或换成另一个版本（#602）。
    Enable(Pack, PathBuf),
    /// 同步后用本图库启用的版本检查新识别的图。
    Auto(Pack, PathBuf, Vec<String>),
    Withdraw,
    /// 角色资料包（#602）：先用 WD 部分、再用 CCIP 部分检查整个图库，各自提交。没有对应向量的组成部分（`None`）只记下
    /// 启用了哪个文件，等识别结果交付时再检查。
    Pack { root: PathBuf, wd: Option<Part>, ccip: Option<Part> },
    /// 撤回角色资料包：两个组成部分里启用了的都撤回。
    PackWithdraw { wd: bool, ccip: bool },
}

/// 角色资料包的一个组成部分：这个文件，和要不要现在检查（图库里有这一种向量时）。
pub(crate) struct Part {
    pub(crate) pack: Pack,
    pub(crate) path: PathBuf,
    pub(crate) scan: bool,
}

impl Progress<'_> {
    /// 换到角色资料包的下一个组成部分：回到扫描阶段，计数清零。
    fn part(&self, part: &str) -> Result<()> {
        self.check()?;
        if let Some((_, state)) = self.0 {
            let mut state = lock_ignoring_poison(state);
            state["part"] = part.into();
            state["phase"] = "scan".into();
            state["done"] = 0.into();
            state["total"] = 0.into();
        }
        Ok(())
    }
}

/// CCIP 长尾部分（`app_ccip_longtail::apply`）的进度报告到同一个任务上。
impl crate::app_imports::Observer for Progress<'_> {
    fn stage(&self, stage: &'static str) {
        if stage == "committing" {
            // 取消在这之前到达时，apply 提交前的检查会停下；这里只把阶段改成写入。
            let _ = self.begin_write();
        }
    }
    fn count(&self, done: u64, total: u64) {
        let _ = Progress::count(self, done as usize, total as usize);
    }
}

fn changed(result: &Value) -> u64 {
    result["changed"].as_u64().unwrap_or(0)
}

fn pack_enable(
    library: &Library,
    root: &Path,
    wd: Option<Part>,
    ccip: Option<Part>,
    progress: &Progress<'_>,
    done: &mut Value,
) -> Result<Value> {
    let mut out = json!({"wd":null,"ccip":null,"changed":0,"hits":0});
    if let Some(part) = wd {
        progress.part("wd")?;
        let result = if part.scan {
            check_file(library, &part.pack, &part.path, None, progress)?
        } else {
            mark_enabled(library, &part.pack)?
        };
        out["changed"] = (changed(&out) + changed(&result)).into();
        out["hits"] = (out["hits"].as_u64().unwrap_or(0) + result["hits"].as_u64().unwrap_or(0)).into();
        out["wd"] = result;
        *done = out.clone();
    }
    if let Some(part) = ccip {
        progress.part("ccip")?;
        let result = if part.scan {
            crate::app_ccip_longtail::apply(library, root, &part.pack, &part.path, &|| progress.check(), progress)?
        } else {
            crate::app_ccip_longtail::mark_enabled(library, root, &part.pack)?
        };
        out["changed"] = (changed(&out) + changed(&result)).into();
        out["hits"] = (out["hits"].as_u64().unwrap_or(0) + result["hits"].as_u64().unwrap_or(0)).into();
        out["ccip"] = result;
    }
    set_declined(library, false)?;
    Ok(out)
}

fn pack_withdraw(library: &Library, wd: bool, ccip: bool, progress: &Progress<'_>, done: &mut Value) -> Result<Value> {
    let mut out = json!({"wd":null,"ccip":null,"changed":0});
    if wd {
        let result = withdraw(library)?;
        out["changed"] = changed(&result).into();
        out["wd"] = result;
        *done = out.clone();
    }
    if ccip {
        if let Some((_, state)) = progress.0 {
            lock_ignoring_poison(state)["part"] = "ccip".into();
        }
        let result = crate::app_ccip_longtail::withdraw(library, progress)?;
        out["changed"] = (changed(&out) + changed(&result)).into();
        out["ccip"] = result;
    }
    // 撤回以后不再默认启用，也不再提示。
    set_declined(library, true)?;
    Ok(out)
}

/// 还没有 WD 向量的图库启用角色资料包（新图库默认启用，#602）：只记下用哪个文件，不检查，没有证据要写。之后识别结果
/// 交付时按图检查；`scored` 为 0、`pending` 为真，表示这个文件还没在整库上检查过。
pub(crate) fn mark_enabled(library: &Library, pack: &Pack) -> Result<Value> {
    ensure_writable(library)?;
    let mut app = writer(&library.db())?;
    crate::schema::require_current(&app)?;
    let tx = app.transaction_with_behavior(TransactionBehavior::Immediate)?;
    crate::schema::begin_writing(&tx)?;
    let current = enabled(&tx)?;
    let now: String = tx.query_row("SELECT datetime('now')", [], |r| r.get(0))?;
    let changed = if current.as_ref().is_some_and(|state| state.package_sha256 != pack.file.sha256) {
        // 换了文件：旧文件给的候选不能留着冒充新文件的。
        wd_extension::withdraw(&tx)?
    } else {
        0
    };
    let entry = Enabled {
        package_sha256: pack.file.sha256.clone(),
        enabled_at: current.map_or_else(|| now.clone(), |state| state.enabled_at),
        checked_at: now,
        scored: 0,
    };
    let value = serde_json::to_string(&entry).expect("WD 扩展状态可序列化");
    tx.execute("INSERT OR REPLACE INTO meta(key,value) VALUES (?,?)", (META_KEY, value))?;
    crate::schema::commit_writing(tx)?;
    Ok(json!({"package_sha256":pack.file.sha256,"scored":0,"hits":0,"changed":changed}))
}

fn check_file(
    library: &Library,
    pack: &Pack,
    path: &Path,
    scope: Option<&[String]>,
    progress: &Progress<'_>,
) -> Result<Value> {
    progress.check()?;
    apply_loaded(library, &Package::load(path, pack)?, &pack.file.sha256, scope, progress)
}

/// 卡片上的角色资料包里的 WD 部分（`app_packs::library_pack`）。
pub(crate) fn card_pack(resources: &Path, library: &Library) -> Result<Pack> {
    Ok(crate::app_packs::for_library(resources, library)?.card.wd)
}

pub(crate) fn status(resources: &Path, library: &Library, tasks: &Tasks, busy: Option<&str>) -> Result<Value> {
    // 先看任务是否已经提交，再读应用库快照。看到终态时，库里的开关和计数必须已经生效。
    let task = tasks.snapshot();
    let mut info = crate::app_models::extension_info(resources, &card_pack(resources, library)?)?;
    let app = crate::app_library::read_connection(&library.db())?;
    let snapshot = app.unchecked_transaction()?;
    let source = crate::app_library::read_connection(&library.source())?;
    let vectors = vector_count(&source, None)?;
    let active = enabled(&snapshot)?;
    let (images, assigned): (usize, usize) = snapshot.query_row(
        "SELECT (SELECT count(DISTINCT uuid) FROM candidates WHERE source='WD_EXT'),
                (SELECT count(*) FROM memberships WHERE sources='WD_EXT')",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let blocked = if library.frozen() {
        Some("frozen")
    } else if busy.is_some() {
        busy
    } else if vectors == 0 {
        Some("no_vectors")
    } else {
        None
    };
    info["library"] = json!({
        "enabled":active.is_some(),
        "package_sha256":active.as_ref().map(|state| &state.package_sha256),
        "enabled_at":active.as_ref().map(|state| &state.enabled_at),
        "checked_at":active.as_ref().map(|state| &state.checked_at),
        "scored":active.as_ref().map_or(0, |state| state.scored),
        "images":images,"assigned":assigned,"vectors":vectors,"blocked":blocked
    });
    info["task"] = task;
    Ok(info)
}

/// 本图库启用了 WD 角色扩展（不论哪个版本）。用哪个包、包在不在本机，到真正检查时再看（`enabled_pack`）。
pub(crate) fn auto_enabled(library: &Library) -> Result<bool> {
    let app = crate::app_library::read_connection(&library.db())?;
    Ok(enabled(&app)?.is_some())
}

/// 识别账本的水位和本次同步可恢复的交付。已经完成的旧批次不会被重新评分。
pub(crate) struct DeliveryWindow {
    rowid: i64,
    pending: Vec<String>,
}

impl DeliveryWindow {
    pub(crate) fn start(library: &Library) -> Result<Option<Self>> {
        if !auto_enabled(library)? {
            return Ok(None);
        }
        let source = crate::app_library::read_connection(&library.source())?;
        if !has_table(&source, "recognition_imports")? {
            return Ok(Some(Self { rowid: 0, pending: Vec::new() }));
        }
        let rowid = source.query_row("SELECT coalesce(max(rowid),0) FROM recognition_imports", [], |r| r.get(0))?;
        let pending = source
            .prepare("SELECT digest FROM recognition_imports WHERE status NOT IN ('done','superseded')")?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(Some(Self { rowid, pending }))
    }

    pub(crate) fn delivered(self, library: &Library) -> Result<Vec<String>> {
        let source = crate::app_library::read_connection(&library.source())?;
        if !has_table(&source, "recognition_imports")? {
            return Ok(Vec::new());
        }
        let mut q = source.prepare(
            "WITH batches AS (
               SELECT digest FROM recognition_imports WHERE rowid>?1 AND status='done'
               UNION
               SELECT i.digest FROM json_each(?2) p JOIN recognition_imports i ON i.digest=p.value WHERE i.status='done'
             )
             SELECT r.asset FROM batches b JOIN recognition_assets r ON r.package=b.digest
             JOIN embeddings v ON v.uuid=r.asset AND v.model='wd'",
        )?;
        let ids = q
            .query_map((self.rowid, json!(self.pending).to_string()), |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(ids)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };

    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct Fixture {
        root: PathBuf,
        library: Library,
    }

    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "illustash-wd-extension-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let images = root.join("images");
            fs::create_dir_all(&images).unwrap();
            let dest = root.join("test.pilib");
            crate::app_create::create_folder(&dest, &images, None, None, None).unwrap();
            let library = Library::open(&dest).unwrap();
            Self { root, library }
        }

        fn write(&self, f: impl FnOnce(&Connection) -> Result<()>) -> Result<()> {
            let mut app = writer(&self.library.db())?;
            let tx = app.transaction_with_behavior(TransactionBehavior::Immediate)?;
            crate::schema::begin_writing(&tx)?;
            f(&tx)?;
            crate::schema::commit_writing(tx)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.root).unwrap();
        }
    }

    fn snapshot(c: &Connection, sql: &str) -> Vec<Value> {
        crate::app_migrate_data::rows(c, sql).unwrap()
    }

    /// 合成包：每个头只看向量的第 `axis` 维（权重 2），阈值和级别照给的。
    fn synthetic(heads: &[(&str, usize, f64, bool)]) -> Package {
        let mut weights = vec![0.; DIM * heads.len()];
        for (i, (_, axis, _, _)) in heads.iter().enumerate() {
            weights[i * DIM + axis] = 2.;
        }
        Package {
            wd_model_sha256: "model".into(),
            wd_tags_sha256: "tags".into(),
            labels: heads.iter().map(|h| h.0.to_owned()).collect(),
            thresholds: heads.iter().map(|h| h.2).collect(),
            candidate_only: heads.iter().map(|h| h.3).collect(),
            weights,
            bias: vec![0.; heads.len()],
        }
    }

    fn pack(format: &str, heads: usize, candidate_only: usize, bytes: &[u8]) -> Pack {
        Pack {
            kind: crate::app_packs::Kind::Wd,
            revision: 2,
            version: "wd-extension-v2".into(),
            format: format.into(),
            heads,
            candidate_only,
            file: crate::app_packs::File {
                name: "synthetic.bin".into(),
                size: bytes.len() as u64,
                sha256: crate::strict_json::sha256_hex(bytes),
                url: String::new(),
            },
            base: Base::Wd { model: "model".into(), tags: "tags".into() },
            license: "Apache-2.0".into(),
            builtin: false,
        }
    }

    fn encode(manifest: Value, heads: usize) -> Vec<u8> {
        let mut input = manifest.to_string().into_bytes();
        input.push(b'\n');
        input.resize(input.len() + heads * (DIM + 1) * 4, 0);
        input
    }

    #[test]
    fn package_formats_threshold_layouts_and_tiers() {
        let legacy = 0.8008725047111511;
        let high = f64::from(0.99_f32);
        let labels = |n: usize| (0..n).map(|i| format!("test_{i}")).collect::<Vec<_>>();
        let base = |format: &str, n: usize| json!({"format":format,"wd_model_sha256":"model","wd_tags_sha256":"tags","labels":labels(n)});
        // 内置 v1：统一阈值，全部一级。
        let mut v1 = base(WD_FORMAT_V1, 3);
        v1["threshold"] = legacy.into();
        let bytes = encode(v1.clone(), 3);
        let decoded = Package::decode(&bytes, &pack(WD_FORMAT_V1, 3, 0, &bytes)).unwrap();
        assert_eq!((decoded.thresholds, decoded.candidate_only), (vec![legacy; 3], vec![false; 3]));
        // #551 的 257 头包：旧格式另带逐头阈值，仍然全部一级。
        let mut v257 = v1.clone();
        v257["thresholds"] = json!([legacy, legacy, high]);
        let bytes = encode(v257.clone(), 3);
        let decoded = Package::decode(&bytes, &pack(WD_FORMAT_V1, 3, 0, &bytes)).unwrap();
        assert_eq!(decoded.thresholds, vec![legacy, legacy, high]);
        assert_eq!(decoded.candidate_only, vec![false; 3]);
        // 新格式：逐头阈值和级别都必须有，二级头数要和目录一致。
        let mut v2 = base(WD_FORMAT_V2, 3);
        v2["thresholds"] = json!([legacy, high, 0.5]);
        v2["tiers"] = json!([1, 2, 2]);
        let bytes = encode(v2.clone(), 3);
        let decoded = Package::decode(&bytes, &pack(WD_FORMAT_V2, 3, 2, &bytes)).unwrap();
        assert_eq!(decoded.candidate_only, vec![false, true, true]);
        assert!(Package::decode(&bytes, &pack(WD_FORMAT_V2, 3, 1, &bytes)).is_err());
        assert!(Package::decode(&bytes, &pack(WD_FORMAT_V1, 3, 2, &bytes)).is_err());

        let rejected = |manifest: Value, format: &str, candidate_only: usize| {
            let bytes = encode(manifest, 3);
            Package::decode(&bytes, &pack(format, 3, candidate_only, &bytes)).is_err()
        };
        let mut tiered_v1 = v257.clone();
        tiered_v1["tiers"] = json!([1, 1, 1]);
        assert!(rejected(tiered_v1, WD_FORMAT_V1, 0), "旧格式不能带级别");
        let mut untiered = v2.clone();
        untiered.as_object_mut().unwrap().remove("tiers");
        assert!(rejected(untiered, WD_FORMAT_V2, 2));
        let mut bad_tier = v2.clone();
        bad_tier["tiers"] = json!([1, 3, 2]);
        assert!(rejected(bad_tier, WD_FORMAT_V2, 2));
        let mut short = v2.clone();
        short["thresholds"] = json!([legacy]);
        assert!(rejected(short, WD_FORMAT_V2, 2));
        let mut out_of_range = v257.clone();
        out_of_range["thresholds"] = json!([legacy, 1.5, legacy]);
        assert!(rejected(out_of_range, WD_FORMAT_V1, 0));
        let mut other_base = v1.clone();
        other_base["wd_tags_sha256"] = "other".into();
        assert!(rejected(other_base, WD_FORMAT_V1, 0), "基础标签表不符");
        let bytes = encode(v1, 3);
        assert!(Package::decode(&bytes[..bytes.len() - 1], &pack(WD_FORMAT_V1, 3, 0, &bytes)).is_err());
        assert!(Package::decode(&bytes, &pack(WD_FORMAT_V1, 2, 0, &bytes)).is_err(), "头数不符");

        // 文件本身要和这个版本的大小、SHA-256 对上。
        let f = Fixture::new();
        let path = f.root.join("package.bin");
        fs::write(&path, &bytes).unwrap();
        let expected = pack(WD_FORMAT_V1, 3, 0, &bytes);
        assert!(Package::load(&path, &expected).is_ok());
        let mut tampered = bytes.clone();
        *tampered.last_mut().unwrap() ^= 1;
        fs::write(&path, &tampered).unwrap();
        assert!(Package::load(&path, &expected).is_err());
    }

    /// v1 → 新版本 → v1 → 撤回：换包沿用同一个 WD_EXT 来源的替换，图库记下用的是哪个包；二级头只出候选。
    #[test]
    fn switching_versions_round_trips_and_candidate_only_heads_never_assign() {
        let f = Fixture::new();
        let assets = ["one", "two", "three"];
        let source = writer(&f.library.source()).unwrap();
        source
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS embeddings(uuid TEXT,model TEXT,vec BLOB,PRIMARY KEY(uuid,model))",
            )
            .unwrap();
        for (i, uuid) in assets.iter().enumerate() {
            let mut wd = vec![0f32; DIM];
            wd[i] = 1.;
            let vector = wd.into_iter().flat_map(f32::to_le_bytes).collect::<Vec<_>>();
            source
                .execute(
                    "INSERT INTO assets(uuid,filename,src_path,width,height,added_at,src_kind,in_scope,trashed)
                     VALUES (?1,'synthetic.png','synthetic.png',8,8,'2026-11-17','folder',1,0)",
                    [uuid],
                )
                .unwrap();
            source.execute("INSERT INTO embeddings VALUES (?1,'wd',?2)", rusqlite::params![uuid, vector]).unwrap();
        }
        f.write(|tx| {
            let rows = assets
                .iter()
                .map(|uuid| json!([uuid, "synthetic.png", "synthetic.png", 8, 8, "2026-11-17", "folder"]))
                .collect::<Vec<_>>();
            crate::character_commands::command(
                tx,
                &json!({"op":"refresh","assets":rows,"indexed":"2026-11-17","index":{},"labels":[],"decisions":[],
                    "duplicates":[]}),
            )
            .map(|_| ())
        })
        .unwrap();
        let app = writer(&f.library.db()).unwrap();
        let members = "SELECT uuid,entity,sources FROM memberships ORDER BY uuid";
        let candidates = "SELECT uuid,entity,source FROM candidates ORDER BY uuid,entity";
        let active = "SELECT id,asset_id,term_id,version,score FROM term_current_evidence ORDER BY id";
        let baseline = (snapshot(&app, members), snapshot(&app, candidates), snapshot(&app, active));
        let state = || enabled(&app).unwrap().map(|e| e.package_sha256);
        let key = |uuid: &str| -> String {
            app.query_row("SELECT entity FROM candidates WHERE uuid=?1 AND source='WD_EXT'", [uuid], |r| r.get(0))
                .unwrap()
        };

        // v1：一个头，只认第一张图。
        let v1 = synthetic(&[("role_one", 0, 0.8, false)]);
        assert_eq!(apply_loaded(&f.library, &v1, "v1", None, &Progress::silent()).unwrap()["changed"], 1);
        assert_eq!(state().as_deref(), Some("v1"));
        let after_v1 = (snapshot(&app, members), snapshot(&app, candidates), snapshot(&app, active));
        let one = key("one");
        assert_eq!(after_v1.0, vec![json!(["one", one, "WD_EXT"])]);

        // 新版本：原来的头加一个一级头（第二张图）和一个二级头（第三张图）。二级头只进候选，不补归属。
        let v2 = synthetic(&[("role_one", 0, 0.8, false), ("role_two", 1, 0.8, false), ("role_three", 2, 0.8, true)]);
        assert_eq!(apply_loaded(&f.library, &v2, "v2", None, &Progress::silent()).unwrap()["changed"], 3);
        assert_eq!(state().as_deref(), Some("v2"));
        let (two, three) = (key("two"), key("three"));
        assert_eq!(snapshot(&app, members), vec![json!(["one", one, "WD_EXT"]), json!(["two", two, "WD_EXT"])]);
        assert_eq!(
            snapshot(&app, candidates),
            vec![json!(["one", one, "WD_EXT"]), json!(["three", three, "WD_EXT"]), json!(["two", two, "WD_EXT"])]
        );
        let tier: Vec<Value> = snapshot(
            &app,
            "SELECT json_extract(data,'$.tag'),json_extract(data,'$.tier') FROM term_current_evidence
             WHERE source='wd_extension' ORDER BY 1",
        );
        assert_eq!(tier, vec![json!(["role_one", null]), json!(["role_three", 2]), json!(["role_two", null])]);
        // 同一个一级头在两个版本里分数相同，但证据行按包分开记。
        assert_eq!(
            snapshot(&app, "SELECT DISTINCT version FROM term_current_evidence WHERE source='wd_extension'").len(),
            3
        );

        // 之后别的操作按图或整库重算归属时，二级候选照样不补归属。
        f.write(|tx| {
            illustash_core::character_projection::memberships(tx, Some(&["three".to_owned()]))?;
            illustash_core::character_projection::memberships(tx, None)
        })
        .unwrap();
        assert_eq!(snapshot(&app, members).len(), 2);

        // 换回 v1：恢复成 v1 那时的样子（证据行复用，没有新行）。
        let rows = snapshot(&app, "SELECT count(*) FROM term_evidence").pop();
        assert_eq!(apply_loaded(&f.library, &v1, "v1", None, &Progress::silent()).unwrap()["changed"], 3);
        assert_eq!(state().as_deref(), Some("v1"));
        assert_eq!((snapshot(&app, members), snapshot(&app, candidates), snapshot(&app, active)), after_v1);
        assert_eq!(snapshot(&app, "SELECT count(*) FROM term_evidence").pop(), rows);

        // 撤回：回到启用之前，历史证据保留。
        assert_eq!(withdraw(&f.library).unwrap()["changed"], 1);
        assert_eq!(state(), None);
        assert_eq!((snapshot(&app, members), snapshot(&app, candidates), snapshot(&app, active)), baseline);
        assert_eq!(snapshot(&app, "SELECT id FROM term_evidence WHERE source='wd_extension'").len(), 4);
    }

    #[test]
    fn cancel_stops_scan_and_write_boundary_refuses_it() {
        let f = Fixture::new();
        for in_write in [false, true] {
            let identity = TaskIdentity::new(&f.library, 1, "synthetic").unwrap();
            let id = identity.task_id.clone();
            let state = Arc::new(Mutex::new(json!({"id":id,"status":"running","phase":"scan"})));
            let shared = state.clone();
            let (ready, entered) = std::sync::mpsc::channel();
            let (release, resume) = std::sync::mpsc::channel();
            let task = Task::start(identity, None, move |ctx| {
                let progress = Progress(Some((&ctx, &shared)));
                if in_write {
                    progress.begin_write()?;
                }
                ready.send(()).unwrap();
                resume.recv_timeout(crate::test_wait::limit()).unwrap();
                progress.check()
            });
            let mut tasks = Tasks { active: Some((task, state)), previous: None };
            entered.recv_timeout(crate::test_wait::limit()).unwrap();
            let cancelled = tasks.cancel(&id);
            release.send(()).unwrap();
            let task = &mut tasks.active.as_mut().unwrap().0;
            assert!(task.wait(crate::test_wait::limit()));
            if in_write {
                assert!(matches!(cancelled, Err(Error::Conflict(_))));
                assert!(matches!(task.outcome(), TaskOutcome::Done));
            } else {
                assert!(cancelled.is_ok());
                assert!(matches!(task.outcome(), TaskOutcome::Cancelled));
            }
        }
    }

    #[test]
    fn shared_projection_keeps_existing_extension_until_it_is_withdrawn() {
        use illustash_core::ccip_longtail;
        let f = Fixture::new();
        let asset = "overlap".to_owned();
        f.write(|tx| {
            crate::character_commands::command(
                tx,
                &json!({"op":"refresh","assets":[[asset,"sample.png","sample.png",8,8,"2026-11-16","folder"]],
                    "indexed":"2026-11-16","index":{},"labels":[],"decisions":[],"duplicates":[]}),
            )?;
            let a = [ccip_longtail::Hit { uuid: asset.clone(), tag: "longtail_role".into(), score: 0.85 }];
            let b =
                [Hit { uuid: asset.clone(), tag: "extension_role".into(), probability: 0.99, candidate_only: false }];
            let scope = std::slice::from_ref(&asset);
            let sources = || snapshot(tx, "SELECT sources FROM memberships WHERE uuid='overlap'");
            let candidates = || snapshot(tx, "SELECT source FROM candidates WHERE uuid='overlap' ORDER BY source");

            assert_eq!(ccip_longtail::apply(tx, "synthetic-a", &a)?, 1);
            assert_eq!(wd_extension::apply_scoped(tx, "synthetic-b", scope, &b)?, 1);
            assert_eq!(sources(), vec![json!(["CCIP_LONGTAIL"])]);
            assert_eq!(candidates(), vec![json!(["CCIP_LONGTAIL"]), json!(["WD_EXT"])]);

            assert_eq!(ccip_longtail::withdraw(tx)?, 1);
            assert_eq!(sources(), vec![json!(["WD_EXT"])]);
            assert_eq!(ccip_longtail::apply(tx, "synthetic-a", &a)?, 1);
            assert_eq!(sources(), vec![json!(["WD_EXT"])]);
            assert_eq!(wd_extension::withdraw(tx)?, 1);
            assert_eq!(sources(), vec![json!(["CCIP_LONGTAIL"])]);
            assert_eq!(candidates(), vec![json!(["CCIP_LONGTAIL"])]);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn evidence_replaces_and_restores_without_rewriting_old_sources() {
        round_trip(false);
        round_trip(true);
    }

    fn round_trip(hybrid: bool) {
        let f = Fixture::new();
        let assets = ["existing", "weak", "empty", "human", "rejected"];
        let app_rows = assets
            .iter()
            .map(|uuid| json!([uuid, "synthetic.png", "synthetic.png", 8, 8, "2026-11-16", "folder"]))
            .collect::<Vec<_>>();
        let package = synthetic(&[("from_b", 0, 0.8008725047111511, false)]);
        let source = writer(&f.library.source()).unwrap();
        source
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS embeddings(uuid TEXT,model TEXT,vec BLOB,PRIMARY KEY(uuid,model))",
            )
            .unwrap();
        let mut wd = vec![0f32; DIM];
        wd[0] = 1.;
        let vector = wd.into_iter().flat_map(f32::to_le_bytes).collect::<Vec<_>>();
        for uuid in assets {
            source
                .execute(
                    "INSERT INTO assets(uuid,filename,src_path,width,height,added_at,src_kind,in_scope,trashed)
                 VALUES (?1,'synthetic.png','synthetic.png',8,8,'2026-11-16','folder',1,0)",
                    [uuid],
                )
                .unwrap();
            source.execute("INSERT INTO embeddings VALUES (?1,'wd',?2)", rusqlite::params![uuid, vector]).unwrap();
        }
        f.write(|tx| {
            if hybrid {
                tx.execute("INSERT OR REPLACE INTO meta VALUES ('identity_policy','hybrid')", [])?;
                tx.execute_batch(
                    &include_str!("app_migration_sql/hybrid.sql")
                        .replace("entities", illustash_core::character_entities::ENTITIES),
                )?;
            }
            crate::character_commands::command(
                tx,
                &json!({
                    "op":"refresh","assets":app_rows,"indexed":"2026-11-16","index":{},
                    "labels":[["existing","base",0.89,"character"],["weak","near",0.90,"char_ext"],
                              ["human","manually_chosen",0.40,"character"],["rejected","from_b",0.50,"character"]],
                    "decisions":[],"duplicates":[]
                }),
            )?;
            for (uuid, action) in [("human", "confirmed"), ("rejected", "rejected")] {
                let entity: String =
                    tx.query_row("SELECT entity FROM candidates WHERE uuid=?1", [uuid], |r| r.get(0))?;
                if uuid == "rejected" {
                    // 同一个公开键即使此前由人工操作或原来源创建，也不能绕过否定。
                    let term: i64 = tx.query_row("SELECT id FROM terms WHERE key=?1", [&entity], |r| r.get(0))?;
                    tx.execute(
                        "INSERT INTO term_links(system,key,term_id,source) VALUES ('danbooru','from_b',?1,'import')",
                        [term],
                    )?;
                }
                crate::character_commands::command(
                    tx,
                    &json!({"op":"change","asked":[uuid],"entity":entity,"action":action}),
                )?;
            }
            Ok(())
        })
        .unwrap();
        let app = writer(&f.library.db()).unwrap();
        let candidates = "SELECT * FROM candidates ORDER BY uuid,source,entity";
        let members = "SELECT * FROM memberships ORDER BY uuid,entity";
        let old_candidates = snapshot(&app, candidates);
        let old_members = snapshot(&app, members);
        let active_rows = "SELECT id,asset_id,term_id,version,score FROM term_current_evidence ORDER BY id";
        let old_evidence = snapshot(&app, active_rows);
        let verdicts = "SELECT * FROM term_verdicts ORDER BY id";
        let old_verdicts = snapshot(&app, verdicts);
        let projection = "SELECT * FROM term_projection ORDER BY asset_id,term_id";
        let old_projection = snapshot(&app, projection);
        assert_eq!(old_members.len(), 2);
        assert_eq!(withdraw(&f.library).unwrap()["changed"], 0);
        let applied = apply_loaded(&f.library, &package, "test", None, &Progress::silent()).unwrap();
        assert_eq!(applied["changed"], 5);
        assert_eq!(applied["hits"], 5);
        assert_eq!(
            snapshot(&app, "SELECT uuid FROM memberships WHERE sources='WD_EXT' ORDER BY uuid"),
            vec![json!(["empty"]), json!(["weak"])]
        );
        assert_eq!(
            snapshot(&app, "SELECT * FROM candidates WHERE source<>'WD_EXT' ORDER BY uuid,source,entity"),
            old_candidates
        );
        let evidence =
            snapshot(&app, "SELECT id,asset_id,term_id,version,score FROM term_current_evidence ORDER BY id");
        let sets = snapshot(&app, "SELECT * FROM term_evidence_sets ORDER BY id");
        assert_eq!(apply_loaded(&f.library, &package, "test", None, &Progress::silent()).unwrap()["changed"], 0);
        assert_eq!(snapshot(&app, "SELECT * FROM term_evidence_sets ORDER BY id"), sets);

        // 模拟批次生效途中数据库写入失败。上一个有效集和投影不动，重试仍能完成。
        app.execute_batch(
            "CREATE TRIGGER stop_extension BEFORE INSERT ON term_evidence_sets
             WHEN new.source='wd_extension' BEGIN SELECT RAISE(ABORT,'simulated interruption'); END;",
        )
        .unwrap();
        assert!(withdraw(&f.library).is_err());
        assert_eq!(
            snapshot(&app, "SELECT id,asset_id,term_id,version,score FROM term_current_evidence ORDER BY id"),
            evidence
        );
        app.execute_batch("DROP TRIGGER stop_extension").unwrap();
        assert_eq!(withdraw(&f.library).unwrap()["changed"], 5);
        assert_eq!(withdraw(&f.library).unwrap()["changed"], 0);
        assert_eq!(snapshot(&app, candidates), old_candidates);
        assert_eq!(snapshot(&app, members), old_members);
        assert_eq!(snapshot(&app, active_rows), old_evidence);
        assert_eq!(snapshot(&app, verdicts), old_verdicts);
        assert_eq!(snapshot(&app, projection), old_projection);
        assert!(snapshot(&app, "SELECT id FROM term_current_evidence WHERE source='wd_extension'").is_empty());
        assert_eq!(snapshot(&app, "SELECT id FROM term_evidence WHERE source='wd_extension'").len(), 5);
        let reopened = Library::open(&f.root.join("test.pilib")).unwrap();
        assert_eq!(snapshot(&writer(&reopened.db()).unwrap(), members), old_members);
        assert_eq!(apply_loaded(&reopened, &package, "test", None, &Progress::silent()).unwrap()["changed"], 5);
        assert_eq!(
            snapshot(&app, "SELECT id,asset_id,term_id,version,score FROM term_current_evidence ORDER BY id"),
            evidence
        );

        // 装错底座和向量损坏时，已有的有效集不被空结果覆盖。
        source.execute("UPDATE embeddings SET vec=X'00' WHERE uuid='weak'", []).unwrap();
        assert!(apply_loaded(&reopened, &package, "test", None, &Progress::silent()).is_err());
        assert_eq!(
            snapshot(&app, "SELECT id,asset_id,term_id,version,score FROM term_current_evidence ORDER BY id"),
            evidence
        );
        let mut manifest = json!({"format":"illustash-recognition/2","models":{"wd":{
            "version":"swinv2_448","dimension":1024,"sha256":"model","tags_sha256":"tags"
        }}});
        package.compatible(&manifest.to_string()).unwrap();
        package.compatible(&json!({"recognition": manifest,"character_claims": {}}).to_string()).unwrap();
        manifest["models"]["wd"]["sha256"] = "unrelated".into();
        assert!(package.compatible(&manifest.to_string()).is_err());

        f.write(|tx| wd_extension::invalidate(tx, &["weak".into()])).unwrap();
        assert_eq!(
            snapshot(&app, "SELECT uuid FROM memberships WHERE sources='WD_EXT' ORDER BY uuid"),
            vec![json!(["empty"])]
        );
    }
}
