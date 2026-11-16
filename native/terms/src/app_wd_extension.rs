//! 用冻结的角色扩展头检查已存的 WD 向量。
use crate::app_library::Library;
use crate::{Error, Result};
use illustash_core::wd_extension::{self, Hit};
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior};
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::Path;

const DIM: usize = 1024;

#[derive(Deserialize)]
struct Manifest {
    format: String,
    wd_model_sha256: String,
    wd_tags_sha256: String,
    threshold: f64,
    labels: Vec<String>,
}

struct Package {
    manifest: Manifest,
    weights: Vec<f32>,
    bias: Vec<f32>,
}

fn float32(bytes: &[u8]) -> Vec<f32> {
    bytes.chunks_exact(4).map(|b| f32::from_le_bytes(b.try_into().expect("四字节分块"))).collect()
}

impl Package {
    fn load(path: &Path, spec: &crate::app_models::WdExtension) -> Result<Self> {
        let input = crate::app_recognition::file(path, spec.file.size)?;
        if input.len() as u64 != spec.file.size || crate::strict_json::sha256_hex(&input) != spec.file.sha256 {
            return Err(Error::invalid("WD 扩展包与 #534 冻结版本不符"));
        }
        let end = input.iter().position(|&byte| byte == b'\n').ok_or_else(|| Error::invalid("缺少 WD 扩展包清单"))?;
        let manifest: Manifest =
            serde_json::from_slice(&input[..end]).map_err(|e| Error::invalid(format!("WD 扩展包清单无效：{e}")))?;
        let coefficients = &input[end + 1..];
        if manifest.format != spec.format
            || manifest.wd_model_sha256 != spec.base_sha256
            || manifest.labels.len() != spec.heads
            || coefficients.len() != spec.heads * (DIM + 1) * 4
        {
            return Err(Error::invalid("WD 扩展包格式不符"));
        }
        let (weights, bias) = coefficients.split_at(spec.heads * DIM * 4);
        Ok(Self { manifest, weights: float32(weights), bias: float32(bias) })
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
            || wd["sha256"].as_str() != Some(&self.manifest.wd_model_sha256)
            || wd["tags_sha256"].as_str() != Some(&self.manifest.wd_tags_sha256)
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
        for ((tag, weights), bias) in self.manifest.labels.iter().zip(self.weights.chunks_exact(DIM)).zip(&self.bias) {
            // 累加与 sigmoid 用 f64，阈值与证据分数沿用冻结实验的 f32 概率精度。
            let sum: f64 = vector.iter().zip(weights).map(|(&x, &w)| f64::from(x) * f64::from(w)).sum();
            let probability = (1.0 / (1.0 + (-(sum + f64::from(*bias))).exp())) as f32;
            if f64::from(probability) >= self.manifest.threshold {
                out.push(Hit { uuid: uuid.to_owned(), tag: tag.clone(), probability: f64::from(probability) });
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

fn has_table(src: &Connection, name: &str) -> Result<bool> {
    Ok(src
        .query_row("SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?)", [name], |r| r.get(0))?)
}

fn vector_count(src: &Connection, scope: &Option<String>) -> Result<usize> {
    if !has_table(src, "embeddings")? {
        return Ok(0);
    }
    Ok(src.query_row(&format!("SELECT count(*) {VECTORS} WHERE {IN_SCOPE} {SCOPE}"), [scope], |r| r.get(0))?)
}

fn scan(
    src: &Connection,
    package: &Package,
    scope: &Option<String>,
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

pub fn apply(library: &Library, path: &Path) -> Result<Value> {
    ensure_writable(library)?;
    let spec = crate::app_models::wd_extension()?;
    apply_loaded(library, &Package::load(path, spec)?, &spec.file.sha256, None, &Progress::silent())
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
    let (checked, hits) = scan(&snapshot, package, &scope_json, progress)?;
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
        Err(Error::Conflict("WD 角色扩展任务已结束或已变化".into()))
    }

    pub(crate) fn start_for(&mut self, library: &Library, identity: TaskIdentity, operation: Action) -> Result<Value> {
        if self.running() {
            return Err(Error::Conflict("已有 WD 角色扩展任务正在进行".into()));
        }
        let (kind, phase) = match &operation {
            Action::Enable(_) => ("enable", "scan"),
            Action::Auto(_, _) => ("auto", "scan"),
            Action::Withdraw => ("withdraw", "write"),
        };
        let initial = json!({
            "id":identity.task_id,"kind":kind,"phase":phase,"status":"running",
            "done":0,"total":0,"message":null,"result":null,"diagnostic":null
        });
        let state = Arc::new(Mutex::new(initial.clone()));
        let shared = state.clone();
        let library = library.clone();
        let task = Task::start(identity, None, move |ctx| {
            let progress = Progress(Some((&ctx, &shared)));
            let result = match operation {
                Action::Enable(path) => check_file(&library, &path, None, &progress),
                Action::Auto(path, ids) => check_file(&library, &path, Some(&ids), &progress),
                Action::Withdraw => withdraw(&library),
            };
            let mut state = lock_ignoring_poison(&shared);
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
    Enable(std::path::PathBuf),
    Auto(std::path::PathBuf, Vec<String>),
    Withdraw,
}

fn check_file(library: &Library, path: &Path, scope: Option<&[String]>, progress: &Progress<'_>) -> Result<Value> {
    let spec = crate::app_models::wd_extension()?;
    progress.check()?;
    apply_loaded(library, &Package::load(path, spec)?, &spec.file.sha256, scope, progress)
}

pub(crate) fn status(resources: &Path, library: &Library, tasks: &Tasks, busy: Option<&str>) -> Result<Value> {
    let mut info = crate::app_models::extension_info(resources)?;
    let app = crate::app_library::read_connection(&library.db())?;
    let source = crate::app_library::read_connection(&library.source())?;
    let vectors = vector_count(&source, &None)?;
    let active = enabled(&app)?;
    let (images, assigned): (usize, usize) = app.query_row(
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
    info["task"] = tasks.snapshot();
    Ok(info)
}

pub(crate) fn auto_enabled(library: &Library) -> Result<bool> {
    let app = crate::app_library::read_connection(&library.db())?;
    let spec = crate::app_models::wd_extension()?;
    Ok(enabled(&app)?.is_some_and(|state| state.package_sha256 == spec.file.sha256))
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
        let mut weights = vec![0.; DIM];
        weights[0] = 2.;
        let package = Package {
            manifest: Manifest {
                format: "illustash-wd-extension/1".into(),
                wd_model_sha256: "model".into(),
                wd_tags_sha256: "tags".into(),
                threshold: 0.8008725047111511,
                labels: vec!["from_b".into()],
            },
            weights,
            bias: vec![0.],
        };
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
