//! 离线识别 v2：验证后源事务提交；应用投影与 imported 事件可独立重试。
use crate::app_imports::{Observer, Silent};
use crate::{app_library::Library, Error, Result};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, TransactionBehavior};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::Read,
    path::Path,
    sync::atomic::AtomicBool,
};

const SCHEMA: &str = r#"CREATE TABLE IF NOT EXISTS recognition_imports(
 digest TEXT PRIMARY KEY, manifest TEXT NOT NULL, assets TEXT NOT NULL, status TEXT NOT NULL,
 error TEXT NOT NULL DEFAULT '', imported_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
 CREATE TABLE IF NOT EXISTS recognition_assets(
 asset TEXT PRIMARY KEY, digest TEXT NOT NULL, package TEXT NOT NULL);
 CREATE TABLE IF NOT EXISTS embeddings(uuid TEXT NOT NULL,model TEXT NOT NULL,vec BLOB NOT NULL,PRIMARY KEY(uuid,model));"#;
fn invalid() -> Error {
    Error::Invalid("识别结果包不合法或与目标图库不匹配".into())
}
fn sha(data: &[u8]) -> String {
    crate::strict_json::sha256_hex(data)
}
fn compact(v: &Value) -> String {
    // Python json.dumps(sort_keys=True,separators=(',',':'),ensure_ascii=True).
    match v {
        Value::Array(a) => format!("[{}]", a.iter().map(compact).collect::<Vec<_>>().join(",")),
        Value::Object(o) => format!(
            "{{{}}}",
            o.iter().map(|(k, v)| format!("{}:{}", compact(&json!(k)), compact(v))).collect::<Vec<_>>().join(",")
        ),
        _ => crate::character_json::dump(v, true),
    }
}
fn file(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.len() > limit {
        return Err(invalid());
    }
    let options = crate::platform_filesystem::read_nofollow_options();
    let mut data = Vec::new();
    options.open(path)?.take(limit + 1).read_to_end(&mut data)?;
    if data.len() as u64 > limit {
        return Err(invalid());
    }
    Ok(data)
}
pub fn model_specs(root: &Path) -> Result<Value> {
    crate::recognition_compute::models::ModelFiles::resolve(root)?.specs(&|| Ok(()))
}
// NPY 的文件头是 Python 字面量，绝不执行。只按允许的三个字段精确解析。
#[cfg(test)]
fn vectors(raw: Vec<u8>, count: usize, dimension: usize) -> Result<Vec<u8>> {
    vectors_checked(raw, count, dimension, &|| Ok(()))
}
pub(crate) fn vectors_checked(
    mut raw: Vec<u8>,
    count: usize,
    dimension: usize,
    check: &dyn Fn() -> Result<()>,
) -> Result<Vec<u8>> {
    if raw.get(..6) != Some(b"\x93NUMPY") {
        return Err(invalid());
    }
    let (start, n) = match raw.get(6..8) {
        Some([1, 0]) if raw.len() >= 10 => {
            (10, u16::from_le_bytes(raw[8..10].try_into().expect("app_recognition internal invariant")) as usize)
        }
        Some([2, 0]) if raw.len() >= 12 => {
            (12, u32::from_le_bytes(raw[8..12].try_into().expect("app_recognition internal invariant")) as usize)
        }
        _ => return Err(invalid()),
    };
    let header = raw.get(start..start + n).and_then(|b| std::str::from_utf8(b).ok()).ok_or_else(invalid)?;
    let h = header.trim();
    // 分词时保留字符串，遇到多余的键或表达式直接拒绝，而不是在子串里查找。
    let mut tokens = Vec::new();
    let mut it = h.chars().peekable();
    while let Some(ch) = it.next() {
        if ch.is_whitespace() {
            continue;
        }
        if ch == '\'' || ch == '"' {
            let mut s = String::new();
            loop {
                let x = it.next().ok_or_else(invalid)?;
                if x == ch {
                    break;
                }
                if x == '\\' {
                    return Err(invalid());
                }
                s.push(x);
            }
            tokens.push(format!("s:{s}"));
        } else if ch.is_ascii_alphanumeric() {
            let mut s = ch.to_string();
            while it.peek().is_some_and(|x| x.is_ascii_alphanumeric()) {
                s.push(it.next().expect("app_recognition internal invariant"));
            }
            tokens.push(s);
        } else {
            tokens.push(ch.to_string());
        }
    }
    let mut p = 0;
    fn take(t: &[String], p: &mut usize, expected: &str) -> Result<()> {
        if t.get(*p).map(String::as_str) != Some(expected) {
            return Err(invalid());
        }
        *p += 1;
        Ok(())
    }
    take(&tokens, &mut p, "{")?;
    let mut keys = HashSet::new();
    while tokens.get(p).map(String::as_str) != Some("}") {
        let key = tokens.get(p).ok_or_else(invalid)?.clone();
        p += 1;
        if !keys.insert(key.clone()) {
            return Err(invalid());
        }
        take(&tokens, &mut p, ":")?;
        match key.as_str() {
            "s:descr" => take(&tokens, &mut p, "s:<f4")?,
            "s:fortran_order" => take(&tokens, &mut p, "False")?,
            "s:shape" => {
                take(&tokens, &mut p, "(")?;
                take(&tokens, &mut p, &count.to_string())?;
                take(&tokens, &mut p, ",")?;
                take(&tokens, &mut p, &dimension.to_string())?;
                if tokens.get(p).map(String::as_str) == Some(",") {
                    p += 1;
                }
                take(&tokens, &mut p, ")")?;
            }
            _ => return Err(invalid()),
        }
        if tokens.get(p).map(String::as_str) == Some(",") {
            p += 1;
        } else if tokens.get(p).map(String::as_str) != Some("}") {
            return Err(invalid());
        }
    }
    take(&tokens, &mut p, "}")?;
    if keys.len() != 3 || p != tokens.len() || raw.len() - start - n != count * dimension * 4 {
        return Err(invalid());
    }
    for row in raw[start + n..].chunks_exact(dimension * 4) {
        check()?;
        let mut norm = 0.0f64;
        for b in row.chunks_exact(4) {
            let v = f32::from_le_bytes(b.try_into().expect("app_recognition internal invariant"));
            if !v.is_finite() {
                return Err(invalid());
            }
            norm += (v as f64).powi(2);
        }
        if norm <= 1e-24 {
            return Err(invalid());
        }
    }
    raw.drain(..start + n);
    Ok(raw)
}
struct Package {
    manifest: Value,
    digest: String,
    ids: Vec<String>,
    inputs: HashMap<String, String>,
    labels: HashMap<String, Vec<Value>>,
    wd: Vec<u8>,
    ccip: Vec<u8>,
}
fn validate(
    path: &Path,
    library: &Library,
    root: &Path,
    cancel: &AtomicBool,
    observer: &dyn Observer,
) -> Result<Package> {
    observer.stage("models");
    let specs =
        crate::recognition_compute::models::ModelFiles::resolve(root)?.specs(&|| crate::app_sync::check(cancel))?;
    validate_against(path, library, root, &specs, cancel, observer)
}
fn validate_against(
    path: &Path,
    library: &Library,
    root: &Path,
    specs: &Value,
    cancel: &AtomicBool,
    observer: &dyn Observer,
) -> Result<Package> {
    observer.stage("reading");
    crate::app_sync::check(cancel)?;
    let m = crate::strict_json::parse(
        std::str::from_utf8(&file(&path.join("manifest.json"), 4 * 1024 * 1024)?).map_err(|_| invalid())?,
    )
    .map_err(Error::Invalid)?;
    if m["format"] == "illustash-recognition/1" {
        return Err(Error::invalid("v1 识别结果包没有输入内容版本，请重新生成 v2 结果包"));
    }
    if m["format"] != "illustash-recognition/2" || m["library_id"] != library.id() || &m["models"] != specs {
        return Err(invalid());
    }
    let ids: Vec<String> = serde_json::from_value(m["assets"].clone()).map_err(|_| invalid())?;
    if ids.is_empty()
        || ids.len() > 100000
        || ids.iter().any(String::is_empty)
        || ids.iter().collect::<HashSet<_>>().len() != ids.len()
    {
        return Err(invalid());
    }
    let inputs: HashMap<String, String> = serde_json::from_value(m["inputs"].clone()).map_err(|_| invalid())?;
    if m["preprocessing"] != crate::recognition_compute::producer::PREPROCESSING
        || inputs.len() != ids.len()
        || ids
            .iter()
            .any(|id| !inputs.get(id).is_some_and(|hash| crate::recognition_compute::producer::valid_input_hash(hash)))
    {
        return Err(invalid());
    }
    if m["files"].as_object().map(|o| o.len()) != Some(3) {
        return Err(invalid());
    }
    let mut payload = HashMap::new();
    for (read, name) in ["labels.jsonl", "wd.npy", "ccip.npy"].into_iter().enumerate() {
        observer.count(read as u64, 3);
        crate::app_sync::check(cancel)?;
        let raw = file(&path.join(name), 1024 * 1024 * 1024)?;
        if m["files"][name] != json!({"bytes":raw.len(),"sha256":sha(&raw)}) {
            return Err(invalid());
        }
        payload.insert(name, raw);
    }
    observer.stage("verifying");
    let wd = vectors_checked(
        payload.remove("wd.npy").expect("app_recognition internal invariant"),
        ids.len(),
        1024,
        &|| crate::app_sync::check(cancel),
    )?;
    let ccip = vectors_checked(
        payload.remove("ccip.npy").expect("app_recognition internal invariant"),
        ids.len(),
        768,
        &|| crate::app_sync::check(cancel),
    )?;
    let mut vocab = HashSet::new();
    let mut csv = csv::Reader::from_path(root.join("tags/tags_v3.csv")).map_err(|_| invalid())?;
    let headers = csv.headers().map_err(|_| invalid())?;
    let cat = headers.iter().position(|s| s == "category").ok_or_else(invalid)?;
    let name = headers.iter().position(|s| s == "name").ok_or_else(invalid)?;
    for row in csv.records() {
        let row = row.map_err(|_| invalid())?;
        let dim = match row.get(cat) {
            Some("0") => "general",
            Some("4") => "character",
            Some("9") => "rating",
            _ => continue,
        };
        vocab.insert((dim.to_owned(), row.get(name).ok_or_else(invalid)?.to_owned()));
    }
    let mut labels = ids.iter().map(|u| (u.clone(), Vec::new())).collect::<HashMap<_, _>>();
    let mut keys = HashSet::new();
    let mut rated = HashSet::new();
    for line in std::str::from_utf8(&payload["labels.jsonl"]).map_err(|_| invalid())?.lines() {
        crate::app_sync::check(cancel)?;
        let row = crate::strict_json::parse(line).map_err(Error::Invalid)?;
        let u = row["asset"].as_str().ok_or_else(invalid)?;
        let dim = row["dim"].as_str().ok_or_else(invalid)?;
        let value = row["value"].as_str().ok_or_else(invalid)?;
        let conf = row["confidence"].as_f64().ok_or_else(invalid)?;
        if !labels.contains_key(u)
            || !vocab.contains(&(dim.to_owned(), value.to_owned()))
            || !(0.0..=1.0).contains(&conf)
            || !keys.insert((u.to_owned(), dim.to_owned(), value.to_owned()))
        {
            return Err(invalid());
        }
        labels.get_mut(u).expect("app_recognition internal invariant").push(json!([dim, value, conf]));
        if dim == "rating" {
            rated.insert(u.to_owned());
        }
    }
    if rated.len() != ids.len() {
        return Err(invalid());
    }
    let digest = sha(compact(&m).as_bytes());
    Ok(Package { manifest: m, digest, ids, inputs, labels, wd, ccip })
}
/// 复核当前可发布的内容。恢复调用方先按账本筛选仍属于本包的资产。
/// v1 不能证明输入版本：已有应用投影可以只确认通知，但不得据旧账本发布新的机器证据。
pub(crate) fn verify_current_inputs(
    c: &Connection,
    manifest: &Value,
    ids: &[String],
    cancel: &AtomicBool,
) -> Result<()> {
    // 公开 manifest 允许附加元数据；同名字段不能把外层 v2 的输入版本遮盖掉。
    let manifest =
        if manifest.get("format").is_some() { manifest } else { manifest.get("recognition").ok_or_else(invalid)? };
    if manifest["format"] == "illustash-recognition/1" {
        return Err(Error::Conflict("v1 待恢复结果没有输入内容版本，不能发布，请重新识别生成 v2 结果".into()));
    }
    if manifest["format"] != "illustash-recognition/2" {
        return Err(invalid());
    }
    for id in ids {
        crate::app_sync::check(cancel)?;
        let path: Option<String> = c
            .query_row("SELECT src_path FROM assets WHERE uuid=? AND in_scope=1 AND coalesce(trashed,0)=0", [id], |r| {
                r.get(0)
            })
            .optional()?;
        let expected = manifest["inputs"][id]
            .as_str()
            .filter(|hash| crate::recognition_compute::producer::valid_input_hash(hash))
            .ok_or_else(invalid)?;
        let Some(path) = path else {
            return Err(invalid());
        };
        let current =
            crate::recognition_compute::producer::input_hash(Path::new(&path), &|| crate::app_sync::check(cancel))?;
        if current != expected {
            return Err(Error::Conflict("图片内容已改变，识别结果已过期，请重新识别".into()));
        }
    }
    Ok(())
}
fn writer(path: &Path) -> Result<Connection> {
    let c = crate::sqlite::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    crate::configure(&c)?;
    c.busy_timeout(crate::app_library::TASK_WRITE_WAIT)?;
    Ok(c)
}
/// 可选的研究分发器确认的是通知已送达，不是算法已完成。
/// 调用这个 API 之前，调用方必须把这个 Library 绑定到它不可变的图库包和会话上。
pub fn import_package(
    library: &Library,
    path: &Path,
    root: &Path,
    cancel: &AtomicBool,
    dispatch: &dyn Fn(&Library, &str) -> Result<()>,
) -> Result<Value> {
    import_internal(library, path, root, cancel, dispatch, None, &Silent)
}
/// 通过 `observer` 报告各阶段并发布源库事务的 `import_package`（#88）。
pub(crate) fn import_package_observed(
    library: &Library,
    path: &Path,
    root: &Path,
    cancel: &AtomicBool,
    dispatch: &dyn Fn(&Library, &str) -> Result<()>,
    observer: &dyn Observer,
) -> Result<Value> {
    import_internal(library, path, root, cancel, dispatch, None, observer)
}
/// 只归一个识别任务所有；在取 TaskContext::receive 之前构建。
pub(crate) struct WorkerInputs {
    identity: crate::app_tasks::TaskIdentity,
    snapshot: crate::recognition_compute::anchor_snapshot::Snapshot,
    models: Value,
    root: std::path::PathBuf,
    shared: std::path::PathBuf,
}
impl WorkerInputs {
    pub(crate) fn load(
        library: &Library,
        identity: &crate::app_tasks::TaskIdentity,
        root: &Path,
        cancel: &AtomicBool,
    ) -> Result<Self> {
        if identity.library_id != library.id() || identity.package != library.instance_path() {
            return Err(invalid());
        }
        let check = || crate::app_sync::check(cancel);
        check()?;
        let models = crate::recognition_compute::models::ModelFiles::resolve(root)?.specs(&check)?;
        let shared = crate::app_resources::shared_path(root)?;
        let snapshot =
            crate::recognition_compute::anchor_snapshot::Snapshot::load_checked(&shared, &library.source(), &check)?;
        Ok(Self { identity: identity.clone(), snapshot, models, root: root.to_owned(), shared })
    }
}
/// 完整预校验的回执凭证，绝不是第二份特征或锚点缓存。
pub(crate) struct PreparedWorkerPackage {
    package: std::path::PathBuf,
    sidecar: std::path::PathBuf,
    digest: String,
    sidecar_digest: String,
}
pub(crate) fn prepare_worker_package(
    library: &Library,
    path: &Path,
    sidecar: &Path,
    inputs: &WorkerInputs,
    cancel: &AtomicBool,
) -> Result<PreparedWorkerPackage> {
    if inputs.identity.library_id != library.id() || inputs.identity.package != library.instance_path() {
        return Err(invalid());
    }
    let pkg = validate_against(path, library, &inputs.root, &inputs.models, cancel, &Silent)?;
    let bytes = file(sidecar, 4 * 1024 * 1024)?;
    let value =
        crate::strict_json::parse(std::str::from_utf8(&bytes).map_err(|_| invalid())?).map_err(Error::Invalid)?;
    let claims: crate::recognition_compute::claims::Claims = crate::worker_protocol::parse(value, sidecar)?;
    claims.validate(&inputs.identity, &pkg.ids, &inputs.models, &inputs.snapshot, &pkg.ccip, &|| {
        crate::app_sync::check(cancel)
    })?;
    Ok(PreparedWorkerPackage {
        package: path.to_owned(),
        sidecar: sidecar.to_owned(),
        digest: pkg.digest,
        sidecar_digest: sha(&bytes),
    })
}
/// 调用方持有任务回执。源库事务里只做线性的权威复核。
pub(crate) fn import_worker_package(
    library: &Library,
    prepared: &PreparedWorkerPackage,
    inputs: &WorkerInputs,
    cancel: &AtomicBool,
    dispatch: &dyn Fn(&Library, &str) -> Result<()>,
) -> Result<Value> {
    if inputs.identity.library_id != library.id() || inputs.identity.package != library.instance_path() {
        return Err(invalid());
    }
    import_internal(library, &prepared.package, &inputs.root, cancel, dispatch, Some((prepared, inputs)), &Silent)
}
fn import_internal(
    library: &Library,
    path: &Path,
    root: &Path,
    cancel: &AtomicBool,
    dispatch: &dyn Fn(&Library, &str) -> Result<()>,
    claims: Option<(&PreparedWorkerPackage, &WorkerInputs)>,
    observer: &dyn Observer,
) -> Result<Value> {
    if library.frozen() {
        return Err(Error::Conflict("图库只读".into()));
    }
    let mut pkg = if let Some((prepared, inputs)) = claims {
        let pkg = validate_against(path, library, root, &inputs.models, cancel, observer)?;
        if pkg.digest != prepared.digest {
            return Err(invalid());
        }
        pkg
    } else {
        validate(path, library, root, cancel, observer)?
    };
    observer.stage("writing");
    let mut c = writer(&library.source())?;
    let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
    // 源库租约冻结了权威的角色元数据。这里复核字节、身份和版本，
    // 绝不重建任务的 O(A²) 别名索引，也不重新计算批次的认领。
    let claims = if let Some((prepared, inputs)) = claims {
        let check = || crate::app_sync::check(cancel);
        check()?;
        let bytes = file(&prepared.sidecar, 4 * 1024 * 1024)?;
        if sha(&bytes) != prepared.sidecar_digest {
            return Err(invalid());
        }
        let value =
            crate::strict_json::parse(std::str::from_utf8(&bytes).map_err(|_| invalid())?).map_err(Error::Invalid)?;
        let claims: crate::recognition_compute::claims::Claims =
            crate::worker_protocol::parse(value.clone(), &prepared.sidecar)?;
        if claims.identity != inputs.identity
            || claims.anchors
                != crate::recognition_compute::anchor_snapshot::Snapshot::fingerprint(&inputs.shared, &tx, &check)?
            || crate::recognition_compute::models::ModelFiles::resolve(root)?.specs(&check)? != inputs.models
        {
            return Err(invalid());
        }
        for claim in &claims.claims {
            let known: bool =
                tx.query_row("SELECT EXISTS(SELECT 1 FROM characters WHERE char_id=?)", [&claim.character], |r| {
                    r.get(0)
                })?;
            if !known {
                return Err(invalid());
            }
        }
        if claims.anchors.vectors.is_some() {
            pkg.digest = sha(compact(&json!([pkg.manifest, value])).as_bytes());
            pkg.manifest = json!({"recognition":pkg.manifest,"character_claims":value});
            Some(claims)
        } else {
            // 无锚点的 sidecar 对原生任务仍是必需的、要校验，
            // 但保持公开的 v2 按图片和按包的指纹以及重复处理的行为。
            None
        }
    } else {
        None
    };
    let models =
        if claims.is_some() { pkg.manifest["recognition"]["models"].clone() } else { pkg.manifest["models"].clone() };
    tx.execute_batch(SCHEMA)?;
    verify_current_inputs(&tx, &pkg.manifest, &pkg.ids, cancel)?;
    let seen: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM recognition_imports WHERE digest=? OR digest GLOB ?)",
        params![pkg.digest, format!("{}:redelivery:*", pkg.digest)],
        |r| r.get(0),
    )?;
    let mut added = Vec::new();
    let mut delivery = None;
    // 整包账本记录过去的交付，不证明每张图片的结果现在仍有效。每次重试都复核逐图指纹。
    for (i, u) in pkg.ids.iter().enumerate() {
        observer.count(i as u64, pkg.ids.len() as u64);
        crate::app_sync::check(cancel)?;
        let wd = &pkg.wd[i * 1024 * 4..(i + 1) * 1024 * 4];
        let ccip = &pkg.ccip[i * 768 * 4..(i + 1) * 768 * 4];
        let mut labels = pkg.labels[u].clone();
        labels.sort_by(|a, b| a[0].as_str().cmp(&b[0].as_str()).then(a[1].as_str().cmp(&b[1].as_str())));
        let mut bytes = crate::character_json::dump(&models, true).into_bytes();
        bytes.extend(compact(&json!([pkg.inputs[u], crate::recognition_compute::producer::PREPROCESSING])).as_bytes());
        bytes.extend(compact(&json!(labels)).as_bytes());
        bytes.extend(wd);
        bytes.extend(ccip);
        if let Some(claims) = &claims {
            let rows = claims.claims.iter().filter(|c| &c.asset == u).collect::<Vec<_>>();
            bytes.extend(compact(&json!({"anchors":claims.anchors,"claims":rows})).as_bytes());
        }
        let fingerprint = sha(&bytes);
        let old: Option<String> =
            tx.query_row("SELECT digest FROM recognition_assets WHERE asset=?", [u], |r| r.get(0)).optional()?;
        if let Some(old) = old {
            if old != fingerprint {
                return Err(invalid());
            }
            continue;
        }
        let delivery = match &delivery {
            Some(value) => value,
            None => {
                // 兼容旧抢救流程清掉源账本、却留下应用投影的图库；也不能复用它的投影标记。
                let projected = if seen {
                    false
                } else {
                    crate::app_library::read_connection(&library.db())?.query_row(
                        "SELECT EXISTS(SELECT 1 FROM meta WHERE key=?)",
                        [format!("recognition_projection:{}", pkg.digest)],
                        |r| r.get::<_, bool>(0),
                    )?
                };
                delivery.insert(if seen || projected {
                    // 同包重新交付有自己的回执与投影标记；旧 done/投影记录保留为历史，不能挡住恢复。
                    format!("{}:redelivery:{}", pkg.digest, crate::store::new_key()?)
                } else {
                    pkg.digest.clone()
                })
            }
        };
        tx.execute(
            "DELETE FROM labels WHERE uuid=? AND source='wd_v3' AND dim IN ('general','character','rating')",
            [u],
        )?;
        for label in &pkg.labels[u] {
            tx.execute(
                "INSERT INTO labels(uuid,dim,value,conf,source,model_ver,run_id) VALUES (?,?,?,?,'wd_v3',?,?)",
                params![
                    u,
                    label[0].as_str(),
                    label[1].as_str(),
                    label[2].as_f64(),
                    models["wd"]["version"].as_str(),
                    format!("import:{delivery}")
                ],
            )?;
        }
        if let Some(claims) = &claims {
            tx.execute("DELETE FROM labels WHERE uuid=? AND source='char_index' AND dim='char_ext'", [u])?;
            for claim in claims.claims.iter().filter(|c| &c.asset == u) {
                tx.execute("INSERT INTO labels(uuid,dim,value,conf,source,model_ver,run_id) VALUES (?,'char_ext',?,?,'char_index','character_index@2025-09-25',?)",params![u,claim.character,claim.confidence,format!("import:{delivery}")])?;
            }
        }
        for (model, vec) in [("wd", wd), ("ccip", ccip)] {
            tx.execute("INSERT OR REPLACE INTO embeddings VALUES (?,?,?)", params![u, model, vec])?;
        }
        tx.execute("INSERT INTO recognition_assets VALUES (?,?,?)", params![u, fingerprint, delivery])?;
        added.push(u.clone());
    }
    let duplicate = seen && added.is_empty();
    if !duplicate {
        tx.execute(
            "INSERT INTO recognition_imports(digest,manifest,assets,status) VALUES (?,?,?,'pending')",
            params![
                delivery.as_deref().unwrap_or(&pkg.digest),
                crate::character_json::dump(&pkg.manifest, true),
                crate::character_json::dump(&json!(added), true)
            ],
        )?;
    }
    observer.stage("committing");
    crate::app_sync::check(cancel)?;
    // 发布点：只有这次提交执行了，内容才会进入图库；而且只在没有先到的取消时才执行。提交期间到达的取消会等待；
    // 之后下面的投影以待处理状态暂停。
    let mut tx = Some(tx);
    observer.publish(&mut || {
        let tx = tx.take().expect("源事务只发布一次");
        verify_current_inputs(&tx, &pkg.manifest, &pkg.ids, cancel)?;
        Ok(tx.commit()?)
    })?;
    drop(tx);
    drop(c);
    observer.stage("projecting");
    let mut result = resume(library, cancel, dispatch)?;
    result["digest"] = json!(pkg.digest);
    result["duplicate"] = json!(duplicate);
    Ok(result)
}
/// 只取仍属于这次交付的有效资产；旧回执不能再发布被扫描撤销或被新交付接管的结果。
fn delivery_ids(source: &Connection, digest: &str, ids: Vec<String>) -> Result<Vec<String>> {
    ids.into_iter().filter_map(|id| {
        match source.query_row(
            "SELECT EXISTS(SELECT 1 FROM recognition_assets r JOIN assets a ON a.uuid=r.asset WHERE r.asset=? AND r.package=? AND a.in_scope=1 AND coalesce(a.trashed,0)=0)",
            params![id, digest], |r| r.get::<_, bool>(0),
        ) {
            Ok(true) => Some(Ok(id)),
            Ok(false) => None,
            Err(e) => Some(Err(e)),
        }
    }).collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
}
/// 同步可以撤回尚未投影、但输入已过期或缺少版本的机器结果，再扫描/识别。
/// 应用先撤回并记录标记，源事务随后提交；中断后按同一回执幂等完成撤回，不把它重新发布。
pub(crate) fn retire_unverifiable(library: &Library, cancel: &AtomicBool) -> Result<()> {
    let mut source = writer(&library.source())?;
    let exists: bool =
        source.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='recognition_imports')", [], |r| {
            r.get(0)
        })?;
    if !exists {
        return Ok(());
    }
    let tx = source.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let pending = tx.prepare("SELECT digest,assets,manifest FROM recognition_imports WHERE status NOT IN ('done','superseded') ORDER BY rowid")?
        .query_map([], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
    let files: bool =
        tx.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='files')", [], |r| r.get(0))?;
    let mut app = writer(&library.db())?;
    for (digest, raw_ids, raw_manifest) in pending {
        crate::app_sync::check(cancel)?;
        let published: bool = app.query_row(
            "SELECT EXISTS(SELECT 1 FROM meta WHERE key=?)",
            [format!("recognition_projection:{digest}")],
            |r| r.get(0),
        )?;
        if published {
            continue;
        }
        let ids = delivery_ids(&tx, &digest, serde_json::from_str(&raw_ids).map_err(|_| invalid())?)?;
        if ids.is_empty() {
            continue;
        }
        let manifest: Value = serde_json::from_str(&raw_manifest).map_err(|_| invalid())?;
        let marker = format!("recognition_invalidated:{digest}");
        let withdrawing: bool =
            app.query_row("SELECT EXISTS(SELECT 1 FROM meta WHERE key=?)", [&marker], |r| r.get(0))?;
        let stale = matches!(verify_current_inputs(&tx, &manifest, &ids, cancel), Err(Error::Conflict(_)));
        crate::app_sync::check(cancel)?;
        // IO/损坏记录不是输入已变化的证据：留待扫描和严格 resume 报告，不能猜测着丢弃。
        if !withdrawing && !stale {
            continue;
        }
        for id in &ids {
            tx.execute(
                "DELETE FROM labels WHERE uuid=? AND (source='wd_v3' OR (source='char_index' AND dim='char_ext'))",
                [id],
            )?;
            tx.execute("DELETE FROM embeddings WHERE uuid=? AND model IN ('wd','ccip')", [id])?;
            tx.execute("DELETE FROM recognition_assets WHERE asset=? AND package=?", params![id, digest])?;
            // 下一步扫描必须重新读字节；即使文件被替换后刻意保留了 size/mtime，也会更新内容元数据。
            if files {
                tx.execute("UPDATE files SET size=-1 WHERE asset=?", [id])?;
            }
        }
        if !withdrawing {
            let app_tx = app.transaction_with_behavior(TransactionBehavior::Immediate)?;
            crate::schema::begin_writing(&app_tx)?;
            crate::app_migrate::invalidate_wd(&app_tx, &ids)?;
            let mut visible = Vec::new();
            for id in &ids {
                if app_tx
                    .query_row("SELECT EXISTS(SELECT 1 FROM assets WHERE uuid=?)", [id], |r| r.get::<_, bool>(0))?
                {
                    visible.push(id.clone());
                }
            }
            if !visible.is_empty() {
                import_changes(&app_tx, &tx, library, &visible)?;
            }
            app_tx.execute("INSERT INTO backend_effects(kind) VALUES ('imported')", [])?;
            app_tx.execute("INSERT INTO meta VALUES (?,?)", params![marker, app_tx.last_insert_rowid().to_string()])?;
            crate::app_sync::check(cancel)?;
            crate::schema::commit_writing(app_tx)?;
        }
        tx.execute("UPDATE recognition_imports SET status='superseded',error='输入版本无法确认，未完成发布的机器结果已撤回，等待重新识别' WHERE digest=?", [&digest])?;
    }
    crate::app_sync::check(cancel)?;
    tx.commit()?;
    Ok(())
}
/// 源库已提交的记录在应用或副作用失败后仍保留；投影标记和事件一起提交。
pub fn resume(
    library: &Library,
    cancel: &AtomicBool,
    dispatch: &dyn Fn(&Library, &str) -> Result<()>,
) -> Result<Value> {
    if library.frozen() {
        return Err(Error::Conflict("图库只读".into()));
    }
    let mut c = writer(&library.source())?;
    let exists: bool =
        c.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='recognition_imports')", [], |r| r.get(0))?;
    if !exists {
        return Ok(json!({"status":"done"}));
    }
    // 不用进程全局锁或按图库 ID 的锁，就让这个包的并发恢复串行执行。
    let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let pending = tx
        .prepare("SELECT digest,assets,manifest FROM recognition_imports WHERE status NOT IN ('done','superseded') ORDER BY rowid")?
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (digest, raw, raw_manifest) in pending {
        let process: Result<()> = (|| {
            crate::app_sync::check(cancel)?;
            let ids: Vec<String> = serde_json::from_str(&raw).map_err(|_| invalid())?;
            if ids.is_empty() {
                return Ok(());
            }
            let mut app = writer(&library.db())?;
            let marker = format!("recognition_projection:{digest}");
            let event: Option<String> =
                app.query_row("SELECT value FROM meta WHERE key=?", [&marker], |r| r.get(0)).optional()?;
            let event = if let Some(event) = event {
                event
            } else {
                // 未投影的旧回执可能已有部分图片失效或由新交付接管，只交付仍属于它的逐图结果。
                let ids = delivery_ids(&tx, &digest, ids)?;
                if ids.is_empty() {
                    return Ok(());
                }
                let withdrawing: bool = app.query_row(
                    "SELECT EXISTS(SELECT 1 FROM meta WHERE key=?)",
                    [format!("recognition_invalidated:{digest}")],
                    |r| r.get(0),
                )?;
                if withdrawing {
                    return Err(Error::Conflict("识别结果撤回尚未完成，请同步后重新识别".into()));
                }
                let manifest: Value = serde_json::from_str(&raw_manifest).map_err(|_| invalid())?;
                verify_current_inputs(&tx, &manifest, &ids, cancel)?;
                crate::app_migrate::deliver_wd(&mut app, &library.source(), library.id(), Some(&ids), true)?;
                let src = crate::app_library::read_connection(&library.source())?;
                let app_tx = app.transaction_with_behavior(TransactionBehavior::Immediate)?;
                crate::schema::begin_writing(&app_tx)?;
                if !ids.is_empty() {
                    import_changes(&app_tx, &src, library, &ids)?;
                }
                app_tx.execute("INSERT INTO backend_effects(kind) VALUES ('imported')", [])?;
                let event = app_tx.last_insert_rowid().to_string();
                app_tx.execute("INSERT INTO meta VALUES (?,?)", params![marker, event])?;
                crate::app_sync::check(cancel)?;
                crate::schema::commit_writing(app_tx)?;
                event
            };
            let status: String =
                app.query_row("SELECT status FROM backend_effects WHERE id=?", [&event], |r| r.get(0))?;
            if status != "done" {
                if let Err(e) = dispatch(library, "imported") {
                    app.execute(
                        "UPDATE backend_effects SET error=? WHERE id=?",
                        params![e.to_string().chars().take(500).collect::<String>(), event],
                    )?;
                    return Err(e);
                }
                crate::app_sync::check(cancel)?;
                app.execute("UPDATE backend_effects SET status='done',error='' WHERE id=?", [event])?;
            }
            Ok(())
        })();
        match process {
            Ok(()) => {
                tx.execute("UPDATE recognition_imports SET status='done',error='' WHERE digest=?", [digest])?;
            }
            Err(e) => {
                let message = e.to_string().chars().take(500).collect::<String>();
                tx.execute(
                    "UPDATE recognition_imports SET status='pending',error=? WHERE digest=?",
                    params![message, digest],
                )?;
                tx.commit()?;
                return Ok(json!({"status":"pending","error":message}));
            }
        }
    }
    tx.commit()?;
    Ok(json!({"status":"done"}))
}
// 保持现有的每批 500 个 ID 的查询分批和行顺序。
fn source_rows(src: &Connection, sql: &str, ids: &[String]) -> Result<Vec<Value>> {
    let mut out = Vec::new();
    for chunk in ids.chunks(500) {
        let marks = vec!["?"; chunk.len()].join(",");
        out.extend(crate::app_migrate_data::rows_with(
            src,
            &sql.replace("{}", &marks),
            rusqlite::params_from_iter(chunk),
        )?);
    }
    Ok(out)
}
fn import_changes(app: &Connection, src: &Connection, library: &Library, ids: &[String]) -> Result<()> {
    let list = serde_json::to_string(ids).expect("internal JSON value is serializable").replace('\'', "''");
    let scope = format!("uuid IN (SELECT value FROM json_each('{list}'))");
    let rows=source_rows(src,"SELECT uuid,filename,src_path,width,height,added_at,src_kind FROM assets WHERE in_scope=1 AND coalesce(trashed,0)=0 AND uuid IN ({})",ids)?;
    let added =
        rows.iter().map(|r| r[0].as_str().expect("validated internal text field").to_owned()).collect::<Vec<_>>();
    if rows.len() != ids.len() {
        return Err(invalid());
    }
    let mut shape = crate::app_migrate_data::rows(
        app,
        &format!("SELECT uuid,filename,src_path,width,height,added_at,src_kind FROM assets WHERE NOT ({scope})"),
    )?;
    shape.extend(rows.clone());
    let album = library.source().parent().expect("internal file path must have a parent").join("album-char-map.json");
    let album = if album.exists() {
        crate::strict_json::parse(&fs::read_to_string(album)?).map_err(Error::Invalid)?["map"].clone()
    } else {
        json!({})
    };
    let stamp: String = src.query_row("SELECT datetime('now','localtime')", [], |r| r.get(0))?;
    let args = json!({"op":"import","rows":rows,"removed":[],"run_id":null,"new_ids":{},"wd_tags":crate::app_migrate_data::rows(src,"SELECT char_id,wd_tag FROM characters")?,"labels":source_rows(src,"SELECT uuid,value,conf,dim FROM labels WHERE ((dim='character' AND conf>=0.35) OR dim='char_ext') AND uuid IN ({})",&added)?,"decisions":source_rows(src,"SELECT uuid,value FROM decisions WHERE dim='character' AND uuid IN ({})",&added)?,"album_map":album,"duplicates":crate::app_migrate_data::duplicates(app,src,&shape)?,"realistic":crate::app_migrate_data::rows(src,"SELECT uuid FROM labels WHERE dim='general' AND value='realistic' AND conf>=0.15")?.iter().map(|r|r[0].clone()).collect::<Vec<_>>(),"indexed":stamp});
    crate::character_commands::command(app, &args)?;
    Ok(())
}

#[cfg(test)]
#[path = "app_recognition/tests.rs"]
mod claims_tests;
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    #[test]
    fn source_queries_keep_python_500_id_batch_row_order() {
        let src = Connection::open_in_memory().unwrap();
        src.execute_batch("CREATE TABLE assets(uuid TEXT PRIMARY KEY)").unwrap();
        let ids = (0..503).rev().map(|i| format!("u{i:03}")).collect::<Vec<_>>();
        for id in &ids {
            src.execute("INSERT INTO assets VALUES (?)", [id]).unwrap();
        }
        let rows = source_rows(&src, "SELECT uuid FROM assets WHERE uuid IN ({})", &ids).unwrap();
        assert_eq!(rows[0], json!(["u003"]));
        assert_eq!(rows[499], json!(["u502"]));
        assert_eq!(rows[500], json!(["u000"]));
        assert_eq!(rows[502], json!(["u002"]));
    }
    #[test]
    fn numpy_literal_parser_does_not_accept_hidden_tokens() {
        fn npy(header: &str) -> Vec<u8> {
            let mut b = b"\x93NUMPY\x01\x00".to_vec();
            b.extend((header.len() as u16).to_le_bytes());
            b.extend(header.as_bytes());
            b.extend(1.0f32.to_le_bytes());
            b
        }
        assert_eq!(
            vectors(npy("{'descr': '<f4', 'fortran_order': False, 'shape': (1, 1), }\n"), 1, 1).unwrap(),
            1.0f32.to_le_bytes()
        );
        for h in [
            "{'descr':'<f4','fortran_order':False,'shape':(1,1),'extra':'<f4'}",
            "{'descr':'<f4','descr':'<f4','fortran_order':False,'shape':(1,1)}",
            "{'descr':'<f4','fortran_order':False,'shape':(1,1)} junk",
            "{'descr':'O','fortran_order':False,'shape':(1,1)}",
            "{'descr':'<f4','fortran_order':False,'shape':(1,1+0)}",
        ] {
            assert!(vectors(npy(h), 1, 1).is_err());
        }
    }
    #[test]
    fn persisted_python_fingerprints_keep_ascii_and_float_layout() {
        assert_eq!(
            compact(&json!({"z":"人物🙂","a":[0.9,1e-7,-0.0]})),
            r#"{"a":[0.9,1e-07,-0.0],"z":"\u4eba\u7269\ud83d\ude42"}"#
        );
    }
    #[test]
    fn dispatch_failure_recovery_and_cancel_preserve_committed_projection() {
        let root = std::env::temp_dir().join(format!(
            "illustash-recognition-{}-{}.pilib",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let run = || -> Result<()> {
            fs::write(root.join("library.json"), r#"{"format":1,"id":"synthetic","name":"synthetic","sources":[]}"#)?;
            let app = crate::sqlite::open(root.join("library.sqlite"))?;
            app.execute_batch(&format!("CREATE TABLE meta(key TEXT PRIMARY KEY,value TEXT); INSERT INTO meta VALUES ('schema_version','36'),('terms_schema','{v}'),('terms_artists','core'),('terms_labels','core'),('terms_characters','core'),('terms_names','core'),('recognition_projection:d','1'); CREATE TABLE backend_effects(id INTEGER PRIMARY KEY,kind TEXT,status TEXT,error TEXT); INSERT INTO backend_effects VALUES (1,'imported','pending','');", v = crate::SCHEMA_VERSION))?;
            let source = crate::sqlite::open(root.join("source.sqlite"))?;
            source.execute_batch("CREATE TABLE assets(uuid TEXT,filename TEXT,src_path TEXT,width INTEGER,height INTEGER,added_at TEXT,src_kind TEXT,in_scope INTEGER,trashed INTEGER); CREATE TABLE labels(uuid TEXT,dim TEXT,value TEXT,conf REAL);")?;
            source.execute_batch(SCHEMA)?;
            source.execute(
                "INSERT INTO recognition_imports(digest,manifest,assets,status) VALUES ('d','{}','[\"a\"]','pending')",
                [],
            )?;
            let library = Library::open(&root)?;
            let cancel = AtomicBool::new(false);
            assert_eq!(
                resume(&library, &cancel, &|_, _| Err(Error::Invalid("synthetic dispatch failure".into())))?["status"],
                "pending"
            );
            assert!(app
                .query_row("SELECT error FROM backend_effects", [], |r| r.get::<_, String>(0))?
                .contains("synthetic dispatch failure"));
            cancel.store(true, Ordering::SeqCst);
            assert_eq!(resume(&library, &cancel, &|_, _| panic!("cancel must precede dispatch"))?["status"], "pending");
            cancel.store(false, Ordering::SeqCst);
            assert_eq!(
                resume(&library, &cancel, &|lib, kind| {
                    assert_eq!(lib.instance_path(), crate::app_paths::canonical(&root)?);
                    assert_eq!(kind, "imported");
                    Ok(())
                })?["status"],
                "done"
            );
            assert_eq!(resume(&library, &cancel, &|_, _| panic!("done must not redispatch"))?["status"], "done");
            assert_eq!(app.query_row("SELECT count(*) FROM backend_effects", [], |r| r.get::<_, i64>(0))?, 1);
            Ok(())
        };
        let result = run();
        fs::remove_dir_all(&root).unwrap();
        result.unwrap();
    }
}
