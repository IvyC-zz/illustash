//! 原生角色索引：稳定查询、补丁提交和恢复；计算 worker 仅处理隔离快照。
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
use crate::{
    app_library::Library,
    app_tasks::{Task, TaskContext, TaskIdentity},
    http_json::Json,
    Error, Result,
};
use crate::{
    task_state::{State, Status},
    worker_protocol as protocol,
};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::{
    collections::BTreeSet,
    io::Read,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};
const DEFAULTS: [&str; 8] = [
    "blue_archive",
    "arknights:_endfield",
    "wuthering_waves",
    "meitantei_precure!",
    "genshin_impact",
    "honkai:_star_rail",
    "honkai_impact_3rd",
    "zenless_zone_zero",
];
fn bad<T>(s: &str) -> Result<T> {
    Err(Error::Invalid(s.into()))
}
fn now() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}
fn read(p: &Path, default: Value) -> Value {
    std::fs::read(p).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or(default)
}
fn write(p: &Path, v: &Value) -> Result<()> {
    std::fs::create_dir_all(p.parent().expect("index file path must have a parent"))?;
    let tmp = p.with_extension(format!("json.{}.tmp", crate::store::new_key()?));
    std::fs::write(&tmp, serde_json::to_vec(v).map_err(|e| Error::Invalid(e.to_string()))?)?;
    std::fs::rename(tmp, p)?;
    Ok(())
}
fn safe(s: &str) -> bool {
    !s.is_empty() && s.len() <= 200 && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}
fn short(s: &str) -> String {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new("[^a-z0-9]+").expect("constant index tag regex is valid"))
        .replace_all(&s.to_lowercase(), "_")
        .trim_matches('_')
        .into()
}
fn writer(p: &Path) -> Result<Connection> {
    crate::app_name_sources::db(p)
}
fn meta(p: &Path, id: &str) -> Value {
    if safe(id) {
        read(&p.join("char-index").join(format!("{id}.json")), Value::Null)
    } else {
        Value::Null
    }
}
fn feature_digest(c: &Connection) -> Result<String> {
    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    if c.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='embeddings')", [], |r| r.get::<_, bool>(0))? {
        let mut q = c.prepare("SELECT uuid,vec FROM embeddings WHERE model='ccip' ORDER BY uuid")?;
        let mut rows = q.query([])?;
        while let Some(r) = rows.next()? {
            let uuid: String = r.get(0)?;
            digest.update(uuid.as_bytes());
            digest.update(r.get_ref(1)?.as_blob().map_err(|_| Error::Invalid("CCIP 特征不是二进制".into()))?);
        }
    }
    Ok(format!("{:x}", digest.finalize()))
}
fn fingerprint(c: &Connection) -> Result<String> {
    Ok(crate::strict_json::sha256_hex(
        serde_json::to_string(&json!([
            crate::app_migrate_data::rows(c, "SELECT * FROM characters ORDER BY char_id")?,
            crate::app_migrate_data::rows(c, "SELECT * FROM character_aliases ORDER BY char_id,alias")?,
            crate::app_migrate_data::rows(
                c,
                "SELECT * FROM labels WHERE dim IN ('char_ext','character','general') ORDER BY uuid,dim,value"
            )?,
            crate::app_migrate_data::rows(c, "SELECT uuid FROM assets ORDER BY uuid")?,
            feature_digest(c)?
        ]))
        .map_err(|e| Error::Invalid(e.to_string()))?
        .as_bytes(),
    ))
}
// The watch list holds work IDs (docs/DATA_MODEL.md), which exist only in the library that owns them, so each library
// keeps its own list and threshold in its prefs (#205). A library without one starts from the shared `watch.json` of
// earlier versions: only its own works and the unresolved tags carry over, and that file is never written again.
const WATCH_PREF: &str = "index_watch";
fn work_for_tag(c: &Connection, tag: &str, current: bool) -> Result<Option<String>> {
    let sql = if current {
        "SELECT l.entity_id FROM links l JOIN entities e ON e.id=l.entity_id WHERE l.system='danbooru' AND l.key=? AND l.current=1 AND e.kind='work'"
    } else {
        "SELECT l.entity_id FROM links l JOIN entities e ON e.id=l.entity_id WHERE l.system='danbooru' AND l.key=? AND e.kind='work'"
    };
    Ok(c.query_row(sql, [tag], |r| r.get(0)).optional()?)
}
fn legacy_watch(c: &Connection, dir: &Path) -> Result<Value> {
    let w = read(&dir.join("watch.json"), json!({"works":DEFAULTS,"min_posts":100}));
    let mut ids = vec![];
    let mut pending = vec![];
    if w["version"] == 2 {
        for id in w["works"].as_array().into_iter().flatten().filter_map(Value::as_str) {
            let own: bool =
                c.query_row("SELECT EXISTS(SELECT 1 FROM entities WHERE id=? AND kind='work')", [id], |r| r.get(0))?;
            if own && !ids.contains(&json!(id)) {
                ids.push(json!(id));
            }
        }
        pending = w["pending_tags"].as_array().cloned().unwrap_or_default();
    } else {
        for tag in w["works"].as_array().into_iter().flatten() {
            match work_for_tag(c, tag.as_str().unwrap_or(""), false)? {
                Some(id) if !ids.contains(&json!(id)) => ids.push(json!(id)),
                Some(_) => {}
                None => pending.push(tag.clone()),
            }
        }
    }
    Ok(json!({"version":2,"works":ids,"pending_tags":pending,"min_posts":w["min_posts"]}))
}
fn load_watch(c: &Connection, dir: &Path) -> Result<Value> {
    let stored: Option<String> =
        c.query_row("SELECT value FROM prefs WHERE key=?", [WATCH_PREF], |r| r.get(0)).optional()?;
    let mut w = match stored {
        Some(text) => serde_json::from_str(&text).map_err(|e| Error::Invalid(format!("无效的关注列表：{e}")))?,
        None => legacy_watch(c, dir)?,
    };
    // An old tag resolves once a work in this library carries it as its current Danbooru link.
    let mut ids = w["works"].as_array().cloned().unwrap_or_default();
    let mut remaining = vec![];
    for tag in w["pending_tags"].as_array().into_iter().flatten() {
        match work_for_tag(c, tag.as_str().unwrap_or(""), true)? {
            Some(id) => {
                if !ids.contains(&json!(id)) {
                    ids.push(json!(id));
                }
            }
            None => remaining.push(tag.clone()),
        }
    }
    w["works"] = json!(ids);
    w["pending_tags"] = json!(remaining);
    Ok(w)
}
fn watch(lib: &Library, dir: &Path) -> Result<Value> {
    load_watch(&crate::app_library::read_connection(&lib.db())?, dir)
}
fn set_watch(lib: &Library, dir: &Path, b: &Value) -> Result<()> {
    let Some(works) = b["works"].as_array().filter(|a| a.len() <= 40 && a.iter().all(Value::is_string)) else {
        return bad("关注列表应为最多 40 个作品 ID");
    };
    let Some(n) = b["min_posts"].as_i64().filter(|n| (20..=5000).contains(n)) else {
        return bad("最少图片数应在 20–5000 之间");
    };
    let mut c = crate::sqlite::open_with_flags(lib.db(), rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    c.busy_timeout(Duration::from_secs(20))?;
    // Take the writer lock first so two saves cannot drop each other's changes.
    let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    crate::schema::require_current(&tx)?;
    for w in works {
        if !tx.query_row("SELECT EXISTS(SELECT 1 FROM entities e JOIN links l ON l.entity_id=e.id WHERE e.id=? AND e.kind='work' AND l.system='danbooru' AND l.current=1)",[w.as_str().expect("watch list was validated as strings")],|r|r.get::<_,bool>(0))?{return bad("只能关注已确认 Danbooru 链接的作品")}
    }
    let mut w = load_watch(&tx, dir)?;
    let mut ids = vec![];
    for id in works {
        if !ids.contains(id) {
            ids.push(id.clone());
        }
    }
    w["works"] = json!(ids);
    w["min_posts"] = n.into();
    tx.execute("INSERT OR REPLACE INTO prefs(key,value) VALUES (?1,?2)", params![WATCH_PREF, w.to_string()])?;
    tx.commit()?;
    Ok(())
}
fn candidates(lib: &Library, shared: &Path, dir: &Path, net: &crate::app_name_sources::Source) -> Result<Value> {
    let w = watch(lib, dir)?;
    let app = crate::app_library::read_connection(&lib.db())?;
    let src = crate::app_library::read_connection(&lib.source())?;
    let mut known = BTreeSet::new();
    for column in ["char_id", "danbooru_tag", "wd_tag"] {
        if src
            .prepare("PRAGMA table_info(characters)")?
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .iter()
            .any(|c| c == column)
        {
            known.extend(
                src.prepare(&format!("SELECT {column} FROM characters WHERE {column} IS NOT NULL"))?
                    .query_map([], |r| r.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?,
            );
        }
    }
    let mut tags = vec![];
    for id in w["works"].as_array().into_iter().flatten() {
        tags.extend(
            app.prepare("SELECT key FROM links WHERE entity_id=? AND system='danbooru' AND current=1")?
                .query_map([id.as_str().unwrap_or("")], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?,
        );
    }
    let rejected = if dir.join("rejected").is_dir() {
        std::fs::read_dir(dir.join("rejected"))?
            .filter_map(|e| e.ok())
            .filter_map(|e| e.file_name().to_str().and_then(|s| s.strip_suffix(".json")).map(str::to_owned))
            .collect::<BTreeSet<_>>()
    } else {
        BTreeSet::new()
    };
    let suffix = regex::Regex::new(r"_\([^()]*\)$").expect("constant index suffix regex is valid");
    let mut rows = vec![];
    let mut counts = serde_json::Map::new();
    let mut seen = BTreeSet::new();
    let work_order = tags.clone();
    for cp in tags {
        let rel = net.fetch(
            &crate::app_name_sources::url(
                &net.db_base,
                "/related_tag.json",
                &[("query", cp.clone()), ("category", "character".into()), ("limit", "1000".into())],
            ),
            None,
            None,
        )?;
        let related = rel["related_tags"].as_array().ok_or_else(|| Error::Network("索引查询响应不正确".into()))?;
        let mut n = 0;
        for r in related {
            let Some(tag) = r["tag"]["name"].as_str() else { return bad("索引角色 tag 不正确") };
            let id = short(tag);
            let posts = r["tag"]["post_count"].as_i64().unwrap_or(0);
            let overlap = r["overlap_coefficient"].as_f64().unwrap_or(0.0);
            if !safe(&id)
                || overlap < 0.5
                || posts < w["min_posts"].as_i64().unwrap_or(100)
                || known.contains(tag)
                || known.contains(&id)
                || seen.contains(tag)
                || shared.join("char-index").join(format!("{id}.json")).exists()
                || rejected.contains(&id)
                || suffix.is_match(&suffix.replace(tag, ""))
            {
                continue;
            }
            seen.insert(tag.to_owned());
            rows.push(json!({"tag":tag,"short_tag":id,"copyright":cp,"post_count":posts,"overlap":(overlap*1000.0).round()/1000.0}));
            n += 1;
        }
        counts.insert(cp, if related.is_empty() { Value::Null } else { json!(n) });
    }
    Ok(json!({"checked_at":now(),"works":counts,"min_posts":w["min_posts"],"missing":rows,"work_order":work_order}))
}

pub(crate) struct Updater {
    shared: PathBuf,
    dir: PathBuf,
    journal: PathBuf,
    state: Arc<Mutex<State>>,
    task: Option<Task>,
    generation: u64,
    session: String,
    network: bool,
    pub automatic: bool,
    next_retry: Instant,
}
impl Updater {
    pub fn new(
        lib: &Library,
        root: &Path,
        generation: u64,
        session: &str,
        network: bool,
        automatic: bool,
    ) -> Result<Self> {
        let shared = crate::app_resources::shared_path(root)?;
        Ok(Self {
            dir: shared.join("char-patch"),
            shared,
            journal: lib.instance_path().join("index-job.json"),
            state: Arc::new(Mutex::new(State::from(json!({"status":"idle"})))),
            task: None,
            generation,
            session: session.into(),
            network,
            automatic,
            next_retry: Instant::now(),
        })
    }
    pub fn writing(&self) -> bool {
        self.task.as_ref().is_some_and(Task::running)
            && self.state.lock().expect("index state mutex poisoned").status() != Status::Checking
    }
    pub fn stop(&mut self) {
        if let Some(mut t) = self.task.take() {
            t.stop();
        }
    }
    pub fn progress(&self) -> Json {
        Json::object([(
            "state",
            ordered(
                &self.state.lock().expect("index state mutex poisoned").value(),
                &[
                    "status",
                    "started_at",
                    "done",
                    "total",
                    "found",
                    "kind",
                    "added",
                    "changed",
                    "message",
                    "finished_at",
                ],
            ),
        )])
    }
    pub fn info(&self, lib: &Library) -> Result<Json> {
        let c = crate::app_library::read_connection(&lib.db())?;
        crate::http_read::register_names(&c)?;
        let (state, mut history) = state_and_history(&self.state, &self.dir);
        history.truncate(6);
        for h in &mut history {
            if let Some(h) = h.as_object_mut() {
                h.remove("job_id");
            }
            for r in h["characters"].as_array_mut().into_iter().flatten() {
                let id:Option<String>=c.query_row("SELECT coalesce(e.parent_id,e.id) FROM links l JOIN entities e ON e.id=l.entity_id WHERE l.system='ccip' AND l.key=?",[r["short_tag"].as_str().unwrap_or("")],|r|r.get(0)).optional()?;
                r["entity"] = if let Some(id) = id {
                    let e=c.query_row("SELECT entity_name(e.id),(SELECT count(*) FROM memberships m WHERE m.entity=e.id AND NOT EXISTS(SELECT 1 FROM duplicates h WHERE h.uuid=m.uuid AND h.hidden=1) AND NOT EXISTS(SELECT 1 FROM excluded x WHERE x.uuid=m.uuid)) FROM entities e WHERE e.id=?",[&id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?)))?;
                    let mut shown = crate::http_read::display(
                        &c,
                        Json::object([("id", id.clone().into()), ("name", e.0.into()), ("count", e.1.into())]),
                    )?;
                    shown.set("id", crate::character_commands::resolve(&c, &id)?.into());
                    serde_json::from_str(&shown.dump()).map_err(|e| Error::Invalid(e.to_string()))?
                } else {
                    Value::Null
                };
            }
        }
        let works = crate::http_read::works(&c, "")?;
        Ok(Json::object([
            (
                "state",
                ordered(
                    &state,
                    &[
                        "status",
                        "started_at",
                        "done",
                        "total",
                        "found",
                        "kind",
                        "added",
                        "changed",
                        "message",
                        "finished_at",
                    ],
                ),
            ),
            ("watch", ordered(&watch(lib, &self.dir)?, &["version", "works", "pending_tags", "min_posts"])),
            ("check", read_check(&self.dir.join("check.json"))),
            ("history", Json::Array(history.iter().map(history_json).collect())),
            ("mb_per_character", Json::Float(1.1)),
            ("works", works),
        ]))
    }
    pub fn post(&mut self, lib: &Library, b: &Json, busy: bool) -> Result<Json> {
        let b: Value = serde_json::from_str(&b.dump()).map_err(|e| Error::Invalid(e.to_string()))?;
        if lib.frozen() {
            return bad("测试图库不能更新索引");
        };
        match b["action"].as_str() {
            Some("watch") => set_watch(lib, &self.dir, &b)?,
            Some("check" | "apply" | "drop") => {
                if b.get("allow_network").is_some_and(|v| !v.is_boolean()) {
                    return bad("联网确认必须是布尔值");
                }
                if b["action"] == "check" && !self.network && b["allow_network"] != true {
                    return bad("请先确认本次检查允许联网查询 Danbooru");
                }
                if b["action"] != "check" && busy {
                    return bad("正在与 Photos 同步，完成后再更新角色索引");
                };
                if self.journal.exists() && b["action"] != "check" {
                    let job: protocol::IndexJob = protocol::read(&self.journal)?;
                    let job = protocol::value(&job)?;
                    let same = if b["action"] == "apply" {
                        b["tags"].as_array().is_some_and(|tags| {
                            tags.iter().map(|r| r.as_str().unwrap_or("")).collect::<BTreeSet<_>>()
                                == job["rows"]
                                    .as_array()
                                    .into_iter()
                                    .flatten()
                                    .map(|r| r["short_tag"].as_str().unwrap_or(""))
                                    .collect::<BTreeSet<_>>()
                        })
                    } else {
                        job["rows"][0]["short_tag"] == b["char"] && job["reason"] == b["reason"]
                    };
                    if !same || job["action"] != b["action"] {
                        return bad("上次索引任务尚待恢复，请先重试原任务");
                    }
                    self.next_retry = Instant::now() + Duration::from_secs(60);
                    self.start(lib, json!({"action":"resume", "allow_network":b["allow_network"]}))?;
                } else {
                    self.next_retry = Instant::now() + Duration::from_secs(60);
                    self.start(lib, b)?;
                }
            }
            _ => return bad("未知操作"),
        };
        self.info(lib)
    }
    pub fn recover(&mut self, lib: &Library, busy: bool) {
        if self.task.as_ref().is_some_and(Task::running) || Instant::now() < self.next_retry || lib.frozen() {
            return;
        }
        if self.journal.exists() {
            if !busy {
                self.next_retry = Instant::now() + Duration::from_secs(60);
                let _ = self.start(lib, json!({"action":"resume"}));
            }
            return;
        }
        if !self.automatic || !self.network || lib.frozen() {
            return;
        }
        let prefs = crate::app_settings::startup(lib)
            .ok()
            .and_then(|v| serde_json::from_str::<Value>(&v.dump()).ok())
            .unwrap_or_default();
        if prefs["index_check"] == false {
            return;
        }
        let at = read(&self.dir.join("check.json"), Value::Null)["checked_at"]
            .as_str()
            .and_then(|s| chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S").ok());
        if at.is_none_or(|t| chrono::Local::now().naive_local().signed_duration_since(t).num_seconds() >= 7 * 86400) {
            self.next_retry = Instant::now() + Duration::from_secs(60);
            let _ = self.start(lib, json!({"action":"check"}));
        }
    }
    fn start(&mut self, lib: &Library, b: Value) -> Result<()> {
        if self.task.as_ref().is_some_and(Task::running) {
            return bad("角色索引正在更新，请稍后");
        };
        let action = b["action"].as_str().unwrap_or("");
        let mut rows = vec![];
        if action == "apply" {
            let check = read(&self.dir.join("check.json"), json!({}));
            let offered = check["missing"].as_array().cloned().unwrap_or_default();
            let Some(tags) = b["tags"].as_array().filter(|t| !t.is_empty()) else {
                return bad("请从最近一次检查的结果里选择角色");
            };
            for t in tags {
                let r = offered
                    .iter()
                    .find(|r| r["short_tag"] == *t)
                    .ok_or_else(|| Error::Invalid("请从最近一次检查的结果里选择角色".into()))?;
                if !rows.contains(r) {
                    rows.push(r.clone());
                }
            }
        }
        if action == "drop" {
            let id = b["char"].as_str().unwrap_or("");
            let m = meta(&self.shared, id);
            if m["source"] != "danbooru-patch" {
                return bad("只能放弃从 Danbooru 补进来的角色");
            };
            if b["reason"].as_str().is_none_or(|r| r.trim().is_empty() || r.trim().chars().count() > 200) {
                return bad("请写一句放弃的原因");
            };
            rows.push(m);
        }
        if action != "check" && action != "resume" && std::env::var_os("ILLUSTASH_INDEX_WORKER").is_none() {
            return bad("角色索引计算 worker 未配置");
        };
        self.task.take();
        let identity = TaskIdentity::new(lib, self.generation, &self.session)?;
        let lib = lib.clone();
        let (shared, dir, journal, state, network) = (
            self.shared.clone(),
            self.dir.clone(),
            self.journal.clone(),
            self.state.clone(),
            self.network || b["allow_network"] == true,
        );
        *state.lock().expect("index state mutex poisoned") = json!({"status":if action=="check"{"checking"}else if action=="drop"{"building"}else{"fetching"},"started_at":now()}).into();
        self.task = Some(Task::start(
            identity,
            Some(Duration::from_millis(
                std::env::var("ILLUSTASH_INDEX_TIMEOUT_MS").ok().and_then(|s| s.parse().ok()).unwrap_or(600000),
            )),
            move |ctx| {
                let run = || {
                    let net = crate::app_name_sources::Source::new(lib.db(), ctx.cancellation_arc(), network, true)?;

                    if b["action"] == "check" {
                        let checked = candidates(&lib, &shared, &dir, &net)?;
                        ctx.check()?;
                        write_check(&dir.join("check.json"), &checked)?;
                        update(
                            &state,
                            json!({"status":"checked","found":checked["missing"].as_array().expect("candidates always returns a missing array").len(),"finished_at":now()}),
                        );
                        Ok(())
                    } else {
                        patch(
                            &lib,
                            &PatchPaths { shared: &shared, dir: &dir, journal: &journal },
                            &b,
                            &rows,
                            &net,
                            &ctx,
                            &state,
                        )
                    }
                };
                let result = run();
                if let Err(ref e) = result {
                    update(
                        &state,
                        json!({"status":"error","message":e.to_string().chars().take(300).collect::<String>(),"finished_at":now()}),
                    );
                }
                result
            },
        ));
        Ok(())
    }
}
// Cross-process shared-index lock. The OS releases it on crash; no stale lock file blocks restart.
struct SharedLock(std::fs::File);
impl SharedLock {
    fn acquire(dir: &Path, ctx: &TaskContext) -> Result<Self> {
        if crate::app_file_lock::ensure_supported().is_err() {
            return bad("当前平台尚不支持索引计算 worker");
        }
        std::fs::create_dir_all(dir)?;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(dir.join("native.lock"))?;
        loop {
            ctx.check()?;
            match crate::app_file_lock::acquire(&file) {
                Ok(()) => return Ok(Self(file)),
                Err(e) if crate::app_file_lock::should_retry(&e) => std::thread::sleep(Duration::from_millis(20)),
                Err(e) => return Err(e.into()),
            }
        }
    }
}
impl Drop for SharedLock {
    fn drop(&mut self) {
        let _ = crate::app_file_lock::release(&self.0);
    }
}

fn copy_tree(src: &Path, dst: &Path, ctx: &TaskContext) -> Result<()> {
    if !src.exists() {
        return Ok(());
    }
    std::fs::create_dir_all(dst)?;
    for e in std::fs::read_dir(src)? {
        ctx.check()?;
        let e = e?;
        if e.file_type()?.is_symlink() {
            return bad("索引资源不能是符号链接");
        }
        if e.file_type()?.is_dir() {
            copy_tree(&e.path(), &dst.join(e.file_name()), ctx)?;
        } else {
            std::fs::copy(e.path(), dst.join(e.file_name()))?;
        }
    }
    Ok(())
}
fn download(address: &str, path: &Path, ctx: &TaskContext, authorized: bool, db: &Path) -> Result<()> {
    ctx.check()?;
    let u = url::Url::parse(address).map_err(|_| Error::Invalid("参考图地址不正确".into()))?;
    let local = matches!(u.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"));
    if (!local && !authorized)
        || !u.username().is_empty()
        || u.password().is_some()
        || (!local && (u.scheme() != "https" || !matches!(u.host_str(), Some("cdn.donmai.us" | "danbooru.donmai.us"))))
        || (local && u.scheme() != "http")
    {
        return bad("参考图来源尚未授权或不在 Danbooru 来源范围");
    };
    let mut last = None;
    for _ in 0..3 {
        ctx.check()?;
        let mut c = writer(db)?;
        let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let used: i64 = tx
            .query_row("SELECT CAST(value AS INTEGER) FROM name_settings WHERE key='traffic_reserved'", [], |r| {
                r.get(0)
            })
            .optional()?
            .unwrap_or(0);
        if used + 2_000_000 > 500_000_000 {
            return Err(Error::Invalid("索引流量预算已用完".into()));
        }
        tx.execute(
            "INSERT OR REPLACE INTO name_settings(key,value) VALUES ('traffic_reserved',?)",
            [(used + 2_000_000).to_string()],
        )?;
        tx.commit()?;
        match ureq::AgentBuilder::new().redirects(0).timeout(Duration::from_secs(25)).build().get(address).call() {
            Ok(r) => {
                let mut bytes = vec![];
                r.into_reader().take(2_000_001).read_to_end(&mut bytes)?;
                if bytes.len() > 2_000_000 {
                    return bad("参考图超过 2 MB");
                };
                ctx.check()?;
                std::fs::create_dir_all(path.parent().expect("index file path must have a parent"))?;
                std::fs::write(path, bytes)?;
                return Ok(());
            }
            Err(_) => last = Some(Error::Network("参考图下载失败".into())),
        }
    }
    Err(last.expect("download retry loop always records a failure"))
}
struct PatchPaths<'a> {
    shared: &'a Path,
    dir: &'a Path,
    journal: &'a Path,
}
fn patch(
    lib: &Library,
    paths: &PatchPaths<'_>,
    b: &Value,
    rows: &[Value],
    net: &crate::app_name_sources::Source,
    ctx: &TaskContext,
    state: &Mutex<State>,
) -> Result<()> {
    let (shared, dir, journal) = (paths.shared, paths.dir, paths.journal);
    let _lock = SharedLock::acquire(dir, ctx)?;
    let mut job: protocol::IndexJob = if journal.exists() {
        protocol::read(journal)?
    } else {
        let src = crate::app_library::read_connection(&lib.source())?;
        let id = &ctx.identity().task_id;
        let folder = lib.instance_path().join("index-jobs").join(id);
        std::fs::create_dir_all(&folder)?;
        let mut prepared = PreparedStage(folder.clone(), false);
        backup_db(&src, &folder.join("pi.db"), ctx)?;
        copy_tree(&shared.join("char-index"), &folder.join("char-index"), ctx)?;
        copy_tree(&shared.join("game-chars"), &folder.join("game-chars"), ctx)?;
        copy_tree(&dir.join("raw"), &folder.join("char-patch/raw"), ctx)?;
        copy_tree(&dir.join("rejected"), &folder.join("char-patch/rejected"), ctx)?;
        for name in ["full-features.npy", "full-manifest.csv"] {
            let from = lib.instance_path().join("features").join(name);
            if from.exists() {
                std::fs::copy(from, folder.join(name))?;
            }
        }
        let album = lib.instance_path().join("album-char-map.json");
        if album.exists() {
            std::fs::copy(album, folder.join("album-char-map.json"))?;
        } else {
            write(&folder.join("album-char-map.json"), &json!({"map":{}}))?;
        }
        std::fs::create_dir_all(folder.join("char-patch/rejected"))?;
        let mut plan = read(&dir.join("plan.json"), json!([])).as_array().cloned().unwrap_or_default();
        for r in rows {
            if !plan.iter().any(|x| x["short_tag"] == r["short_tag"]) {
                plan.push(r.clone());
            }
        }
        write(&folder.join("char-patch/plan.json"), &json!(plan))?;
        let job = protocol::parse(
            json!({"version":1,"package":lib.instance_path(),"job_id":id,"identity":ctx.identity(),"action":b["action"],"reason":b["reason"],"rows":rows,"folder":folder,"before":fingerprint(&crate::app_library::read_connection(&folder.join("pi.db"))?)?,"phase":"prepared"}),
            journal,
        )?;
        write(journal, &protocol::value(&job)?)?;
        prepared.1 = true;
        job
    };
    if job.version != 1 || job.package != lib.instance_path() {
        return bad("索引恢复包实例不匹配");
    };
    let folder = job.folder.clone();
    if folder.parent() != Some(lib.instance_path().join("index-jobs").as_path())
        || folder.file_name().and_then(|n| n.to_str()) != Some(job.job_id.as_str())
    {
        return bad("索引任务目录越界");
    }
    if job.phase == protocol::JobPhase::Prepared {
        job.identity = ctx.identity().clone();
        write(journal, &protocol::value(&job)?)?;
        if job.action == protocol::IndexAction::Apply {
            let rows = &job.rows;
            for (i, r) in rows.iter().enumerate() {
                ctx.check()?;
                let cid = r.short_tag.as_str();
                if !safe(cid) {
                    return bad("索引角色 ID 不正确");
                };
                update(state, json!({"status":"fetching","done":i,"total":rows.len()}));
                if meta(&folder, cid)["source"] == "danbooru-patch" {
                    continue;
                }
                let posts = net.fetch(
                    &crate::app_name_sources::url(
                        &net.db_base,
                        "/posts.json",
                        &[
                            ("tags", format!("{} solo rating:g,s", r.tag)),
                            ("limit", "20".into()),
                            ("only", "id,tag_string_general,media_asset".into()),
                        ],
                    ),
                    None,
                    None,
                )?;
                let Some(posts) = posts.as_array().filter(|p| p.len() <= 20) else {
                    return bad("参考图帖子响应不正确");
                };
                let mut refs = vec![];
                for p in posts {
                    let variant =
                        p["media_asset"]["variants"].as_array().into_iter().flatten().find(|v| v["type"] == "720x720");
                    if let Some(v) = variant {
                        let pid = p["id"].as_u64().ok_or_else(|| Error::Invalid("参考图帖子 ID 不正确".into()))?;
                        let path = folder.join("char-patch/img").join(cid).join(format!("{pid}.img"));
                        if let Err(e) = download(v["url"].as_str().unwrap_or(""), &path, ctx, net.authorized, &net.path)
                        {
                            ctx.check()?;
                            if matches!(e, Error::Network(_)) {
                                continue;
                            } else {
                                return Err(e);
                            }
                        }
                        refs.push(json!({"id":pid,"path":path,"general":p["tag_string_general"]}));
                    }
                }
                write(&folder.join("char-patch").join(format!("{cid}-refs.json")), &json!(refs))?;
            }
        }
        let request = protocol::value(&protocol::IndexRequest {
            format: protocol::Format::Index,
            identity: ctx.identity(),
            before: &job.before,
            action: job.action,
            rows: &job.rows,
            reason: &job.reason,
            review: &folder,
        })?;
        write(&folder.join("request.json"), &request)?;
        update(state, json!({"status":"building"}));
        let executable = std::env::var_os("ILLUSTASH_INDEX_WORKER")
            .ok_or_else(|| Error::Invalid("角色索引计算 worker 未配置".into()))?;
        let process_ctx = ctx.clone();
        let worker_folder = folder.clone();
        let process = std::thread::spawn(move || {
            process_ctx.process(
                std::process::Command::new(executable)
                    .arg(worker_folder.join("request.json"))
                    .arg(worker_folder.join("response.json")),
            )
        });
        while !process.is_finished() {
            if ctx.check().is_err() {
                ctx.cancel();
            } else {
                let p = read(&folder.join("progress.json"), Value::Null);
                if p["identity"] == json!(ctx.identity()) {
                    let step = match p["step"].as_str() {
                        Some("fetch") => Some("fetching"),
                        Some("clean") => Some("cleaning"),
                        Some("build") => Some("building"),
                        Some("scan") => Some("scanning"),
                        _ => None,
                    };
                    if let Some(step) = step {
                        let mut fields = json!({"status":step});
                        for key in ["done", "total"] {
                            if let Some(n) = p[key].as_u64().filter(|n| *n <= 100000) {
                                fields[key] = n.into();
                            }
                        }
                        update(state, fields);
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let status = process.join().map_err(|_| Error::Invalid("索引计算进程等待失败".into()))??;
        if !status.success() {
            return bad("角色索引计算 worker 失败");
        };
        ctx.check()?;
        if std::fs::metadata(folder.join("response.json"))?.len() > 64_000_000 {
            return bad("索引结果超过 64 MB");
        }
        let response: protocol::IndexResponse = protocol::read(&folder.join("response.json"))?;
        if &response.identity != ctx.identity()
            || response.format != protocol::Format::Index
            || response.before != job.before
        {
            return bad("索引结果身份或版本不匹配");
        };
        validate(lib, &response, &folder.join("response.json"))?;
        job.result = Some(response);
        job.phase = protocol::JobPhase::Ready;
        write(journal, &protocol::value(&job)?)?;
    }
    ctx.receive(ctx.identity(), || commit(lib, shared, dir, journal, &mut job, ctx, state))?;
    Ok(())
}
fn validate(lib: &Library, v: &protocol::IndexResponse, file: &Path) -> Result<()> {
    let field_error =
        |field: &str, message: &str| Error::Invalid(format!("{}: 字段 {field}: {message}", file.display()));
    let chars = &v.characters;
    if chars.len() > 100000 {
        return Err(field_error("characters", "索引角色结果不正确"));
    }
    let mut ids = BTreeSet::new();
    for (index, r) in chars.iter().enumerate() {
        let id = r.char_id.as_str();
        if !safe(id) {
            return Err(field_error(&format!("characters[{index}].char_id"), "索引角色 ID 不正确"));
        }
        if !ids.insert(id) {
            return bad("索引角色重复");
        };
        for key in [
            "danbooru_tag",
            "wd_tag",
            "copyright",
            "base_tag",
            "skin",
            "gender",
            "core_tags",
            "name_en",
            "name_ja",
            "name_zh",
        ] {
            if !r.columns.get(key).unwrap_or(&Value::Null).is_null()
                && r.columns.get(key).unwrap_or(&Value::Null).as_str().is_none_or(|s| s.len() > 10000)
            {
                return Err(field_error(&format!("characters[{index}].{key}"), "索引角色文本字段不正确"));
            }
        }
        for key in ["post_count", "n_ref", "anchor_row"] {
            if !r.columns.get(key).unwrap_or(&Value::Null).is_null()
                && !r
                    .columns
                    .get(key)
                    .unwrap_or(&Value::Null)
                    .as_i64()
                    .is_some_and(|n| (0..=1_000_000_000).contains(&n))
            {
                return Err(field_error(&format!("characters[{index}].{key}"), "索引角色计数字段不正确"));
            }
        }
        for key in ["gender_conf", "anchor_p50", "anchor_p75", "anchor_p90"] {
            if !r.columns.get(key).unwrap_or(&Value::Null).is_null()
                && !r.columns.get(key).unwrap_or(&Value::Null).as_f64().is_some_and(|n| (0.0..=1.0).contains(&n))
            {
                return Err(field_error(&format!("characters[{index}].{key}"), "索引角色阈值字段不正确"));
            }
        }
    }
    let src = crate::app_library::read_connection(&lib.source())?;
    let assets = src
        .prepare("SELECT uuid FROM assets")?
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<BTreeSet<_>>>()?;
    let old_labels = crate::app_migrate_data::rows(&src, "SELECT uuid,value,conf FROM labels WHERE dim='char_ext'")?;
    let labels = &v.labels;
    let mut seen = BTreeSet::new();
    for r in labels {
        if (!assets.contains(&r.0) && !old_labels.contains(&json!(r)))
            || !ids.contains(r.1.as_str())
            || !(0.0..=1.0).contains(&r.2)
        {
            return bad("索引认领越界");
        };
        if !seen.insert((&r.0, &r.1)) {
            return bad("索引认领重复");
        }
    }
    let aliases = &v.aliases;
    for r in aliases {
        if !safe(&r.0) {
            return bad("索引别名越界");
        }
    }
    Ok(())
}
fn commit(
    lib: &Library,
    shared: &Path,
    dir: &Path,
    journal: &Path,
    job: &mut protocol::IndexJob,
    ctx: &TaskContext,
    state: &Mutex<State>,
) -> Result<()> {
    ctx.check()?;
    let id = job.job_id.clone();
    let folder = job.folder.clone();
    let mut src = writer(&lib.source())?;
    let has_runs: bool =
        src.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='native_index_runs')", [], |r| r.get(0))?;
    let stored: Option<String> = if has_runs {
        src.query_row("SELECT result FROM native_index_runs WHERE id=?", [&id], |r| r.get(0)).optional()?
    } else {
        None
    };
    if let Some(stored) = stored {
        *job = protocol::parse(
            serde_json::from_str(&stored)
                .map_err(|e| Error::Invalid(format!("source.sqlite native_index_runs.result: {e}")))?,
            Path::new("source.sqlite native_index_runs.result"),
        )?;
    } else {
        if fingerprint(&src)? != job.before {
            std::fs::remove_file(journal)?;
            let _ = std::fs::remove_dir_all(&folder);
            return bad("索引计算输入已改变，请重试检查");
        };
        let result = job
            .result
            .as_ref()
            .ok_or_else(|| Error::Invalid(format!("{}: 字段 result: ready job 缺少结果", journal.display())))?;
        validate(lib, result, journal)?;
        let old =
            crate::app_migrate_data::rows(&src, "SELECT uuid,value,round(conf,4) FROM labels WHERE dim='char_ext'")?;
        let new = &result.labels;
        let old_set = old.iter().map(|r| r.to_string()).collect::<BTreeSet<_>>();
        let new_set = new
            .iter()
            .map(|r| json!([r.0, r.1, (r.2 * 10000.0).round() / 10000.0]).to_string())
            .collect::<BTreeSet<_>>();
        let changed = old_set
            .symmetric_difference(&new_set)
            .map(|s| {
                let row: (String, String, f64) = protocol::parse(
                    serde_json::from_str(s).map_err(|e| Error::Invalid(e.to_string()))?,
                    Path::new("source.sqlite labels"),
                )?;
                Ok(row.0)
            })
            .collect::<Result<BTreeSet<_>>>()?;
        job.extra.insert("changed".into(), json!(changed));
        let backup = dir.join("backup").join(&id);
        std::fs::create_dir_all(&backup)?;
        backup_db(&src, &backup.join("characters.sqlite"), ctx)?;
        for name in ["char-anchors.npy", "char-anchors.txt"] {
            if shared.join(name).exists() {
                std::fs::copy(shared.join(name), backup.join(name))?;
            }
        }
        let tx = src.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if fingerprint(&tx)? != job.before {
            std::fs::remove_file(journal)?;
            let _ = std::fs::remove_dir_all(&folder);
            return bad("索引计算输入已改变");
        };
        tx.execute_batch("CREATE TABLE IF NOT EXISTS native_index_runs(id TEXT PRIMARY KEY,result TEXT NOT NULL)")?;
        tx.execute("DELETE FROM character_aliases", [])?;
        tx.execute("DELETE FROM characters", [])?;
        let cols = tx
            .prepare("PRAGMA table_info(characters)")?
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for r in &result.characters {
            let vals = cols
                .iter()
                .map(|k| {
                    if k == "char_id" {
                        rusqlite::types::Value::Text(r.char_id.clone())
                    } else {
                        sql_value(r.columns.get(k).unwrap_or(&Value::Null))
                    }
                })
                .collect::<Vec<_>>();
            tx.execute(
                &format!("INSERT INTO characters ({}) VALUES ({})", cols.join(","), vec!["?"; cols.len()].join(",")),
                rusqlite::params_from_iter(vals),
            )?;
        }
        let alias_cols = tx
            .prepare("PRAGMA table_info(character_aliases)")?
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for r in &result.aliases {
            if alias_cols.iter().any(|c| c == "kind") {
                tx.execute(
                    "INSERT OR IGNORE INTO character_aliases(char_id,alias,kind) VALUES (?,?,?)",
                    params![r.0, r.1, r.2],
                )?;
            } else {
                tx.execute("INSERT OR IGNORE INTO character_aliases(char_id,alias) VALUES (?,?)", params![r.0, r.1])?;
            }
        }
        tx.execute("DELETE FROM labels WHERE dim='char_ext'", [])?;
        let label_cols = tx
            .prepare("PRAGMA table_info(labels)")?
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for r in &result.labels {
            if ["source", "model_ver", "run_id"].iter().all(|k| label_cols.iter().any(|c| c == k)) {
                tx.execute("INSERT INTO labels(uuid,dim,value,conf,source,model_ver,run_id) VALUES (?,'char_ext',?,?,'char_index',?,?)",params![r.0,r.1,r.2,format!("character_index@2025-09-25+danbooru@{}",chrono::Local::now().format("%Y-%m-%d")),format!("char-scan-{id}")])?;
                continue;
            }
            tx.execute("INSERT INTO labels(uuid,dim,value,conf) VALUES (?,'char_ext',?,?)", params![r.0, r.1, r.2])?;
        }
        let result = job.result.take();
        tx.execute("INSERT INTO native_index_runs VALUES (?,?)", params![id, protocol::value(&job)?.to_string()])?;
        job.result = result;
        ctx.check()?;
        tx.commit()?;
    }
    // Files are copied from an immutable validated job. Repeating this boundary is safe after a crash.
    let allowed = job.rows.iter().map(|r| r.short_tag.as_str()).collect::<BTreeSet<_>>();
    for cid in allowed {
        if !safe(cid) {
            return bad("索引文件 ID 不正确");
        };
        for ext in ["json", "npy"] {
            let from = folder.join("char-index").join(format!("{cid}.{ext}"));
            let to = shared.join("char-index").join(format!("{cid}.{ext}"));
            std::fs::create_dir_all(to.parent().expect("index file path must have a parent"))?;
            if from.exists() {
                std::fs::copy(from, to)?;
            } else if to.exists() {
                std::fs::remove_file(to)?;
            }
        }
        let raw = folder.join("char-patch/raw").join(format!("{cid}.npy"));
        if raw.exists() {
            std::fs::create_dir_all(dir.join("raw"))?;
            std::fs::copy(raw, dir.join("raw").join(format!("{cid}.npy")))?;
        }
        let rejected = folder.join("char-patch/rejected").join(format!("{cid}.json"));
        if rejected.exists() {
            write(&dir.join("rejected").join(format!("{cid}.json")), &read(&rejected, Value::Null))?;
        }
    }
    for name in ["char-anchors.npy", "char-anchors.txt"] {
        if folder.join(name).exists() {
            std::fs::copy(folder.join(name), shared.join(name))?;
        }
    }
    write(&dir.join("plan.json"), &read(&folder.join("char-patch/plan.json"), json!([])))?;
    update(state, json!({"status":"importing"}));
    let mut app = writer(&lib.db())?;
    crate::http_read::register_names(&app)?;
    let marker = format!("index_job:{id}");
    let tx = app.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    crate::schema::begin_writing(&tx)?;
    let done: Option<String> =
        tx.query_row("SELECT value FROM meta WHERE key=?", [&marker], |r| r.get(0)).optional()?;
    let changed = if let Some(n) = done {
        n.parse::<i64>().unwrap_or(0)
    } else {
        let list = job.extra.get("changed").unwrap_or(&Value::Null).to_string();
        let labels=crate::app_migrate_data::rows_with(&src,"SELECT uuid,value,conf,dim FROM labels WHERE ((dim='character' AND conf>=0.35) OR dim='char_ext') AND uuid IN (SELECT value FROM json_each(?))",[list])?;
        let realistic = crate::app_migrate_data::rows(
            &src,
            "SELECT uuid FROM labels WHERE dim='general' AND value='realistic' AND conf>=0.15",
        )?
        .iter()
        .map(|r| r[0].clone())
        .collect::<Vec<_>>();
        let out = crate::character_commands::command(
            &tx,
            &json!({"op":"reindex","index":crate::app_migrate_data::index(&src)?,"uuids":job.extra.get("changed").unwrap_or(&Value::Null),"labels":labels,"realistic":realistic,"indexed":id}),
        )?;
        let n = out["changed"].as_i64().unwrap_or(0);
        tx.execute("INSERT INTO meta VALUES (?,?)", params![marker, n.to_string()])?;
        tx.execute("INSERT INTO backend_effects(kind) VALUES ('names:{\"entity\":null,\"refresh\":false}')", [])?;
        tx.execute("INSERT INTO backend_effects(kind) VALUES ('index_changed')", [])?;
        n
    };
    ctx.check()?;
    crate::schema::commit_writing(tx)?;
    let mut chars = vec![];
    let drop = job.action == protocol::IndexAction::Drop;
    for row in &job.rows {
        let mut r = protocol::value(row)?;
        let cid = row.short_tag.clone();
        if drop {
            r = json!({"tag":r["tag"],"short_tag":cid,"copyright":r["copyright"],"status":"dropped","reason":job.reason.as_deref().unwrap_or("").trim()});
        } else if meta(shared, &cid)["source"] == "danbooru-patch" {
            let n: i64 = src.query_row(
                "SELECT count(DISTINCT uuid) FROM labels WHERE dim='char_ext' AND value=?",
                [&cid],
                |r| r.get(0),
            )?;
            r["status"] = "added".into();
            r["claims"] = n.into();
        } else {
            r["status"] = "rejected".into();
            r["reason"] = read(&dir.join("rejected").join(format!("{cid}.json")), json!({}))["rejected"]
                .as_str()
                .unwrap_or("没有可用的参考图")
                .into();
        }
        chars.push(r);
    }
    let mut check = read(&dir.join("check.json"), Value::Null);
    if !drop && !check.is_null() {
        let done = chars.iter().map(|r| r["short_tag"].clone()).collect::<Vec<_>>();
        if let Some(missing) = check["missing"].as_array_mut() {
            missing.retain(|r| !done.contains(&r["short_tag"]));
        }
        write(&dir.join("check.json"), &check)?;
    }
    let mut ledger = read(&dir.join("native-history.json"), json!({}));
    let record = if let Some(record) = ledger.get(&id) {
        record.clone()
    } else {
        let record = json!({"at":now(),"kind":if drop{"drop"}else{"add"},"characters":chars,"changed":changed});
        ledger[&id] = record.clone();
        write(&dir.join("native-history.json"), &ledger)?;
        record
    };
    let mut history = read(&dir.join("history.json"), json!([])).as_array().cloned().unwrap_or_default();
    if !history.contains(&record) {
        history.truncate(30);
        history.insert(0, record);
        write(&dir.join("history.json"), &json!(history))?;
    }
    let mut backups = std::fs::read_dir(dir.join("backup"))?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_str().is_some_and(|n| n.len() == 32 && n.bytes().all(|b| b.is_ascii_hexdigit())))
        .collect::<Vec<_>>();
    backups.sort_by_key(|e| e.metadata().and_then(|m| m.modified()).ok());
    let excess = backups.len().saturating_sub(5);
    for e in backups.into_iter().take(excess) {
        let _ = std::fs::remove_dir_all(e.path());
    }
    let added = chars.iter().filter(|r| r["status"] == "added").count();
    update(
        state,
        json!({"status":"done","kind":if drop{"drop"}else{"add"},"added":added,"changed":changed,"finished_at":now()}),
    );
    std::fs::remove_file(journal)?;
    let _ = std::fs::remove_dir_all(&folder);
    Ok(())
}

fn sql_value(v: &Value) -> rusqlite::types::Value {
    use rusqlite::types::Value as S;
    match v {
        Value::Null => S::Null,
        Value::String(s) => S::Text(s.clone()),
        Value::Number(n) => n
            .as_i64()
            .map(S::Integer)
            .unwrap_or_else(|| S::Real(n.as_f64().expect("JSON number has a numeric representation"))),
        Value::Bool(b) => S::Integer(i64::from(*b)),
        _ => S::Text(v.to_string()),
    }
}

fn ordered(v: &Value, keys: &[&str]) -> Json {
    if !v.is_object() {
        return Json::from(v.clone());
    }
    let mut fields =
        keys.iter().filter_map(|k| v.get(*k).map(|x| ((*k).to_string(), Json::from(x.clone())))).collect::<Vec<_>>();
    fields.extend(
        v.as_object()
            .expect("ordered checked object type")
            .iter()
            .filter(|(k, _)| !keys.contains(&k.as_str()))
            .map(|(k, v)| (k.clone(), Json::from(v.clone()))),
    );
    Json::Object(fields)
}
fn character_json(v: &Value) -> Json {
    let mut out =
        ordered(v, &["tag", "short_tag", "copyright", "post_count", "overlap", "status", "claims", "reason", "entity"]);
    if let Some(e) = v.get("entity") {
        let mut shown = ordered(
            e,
            &[
                "id",
                "name",
                "count",
                "name_source",
                "names",
                "work_id",
                "node_type",
                "character_id",
                "variant_id",
                "canonical_id",
            ],
        );
        if let Some(names) = e.get("names") {
            shown.set("names", ordered(names, &["zh", "ja", "en"]));
        }
        out.set("entity", shown);
    }
    out
}
fn check_json(v: &Value) -> Json {
    let mut value = v.clone();
    value.as_object_mut().map(|o| o.remove("work_order"));
    let mut out = ordered(&value, &["checked_at", "works", "min_posts", "missing"]);
    if let Some(order) = v["work_order"].as_array() {
        out.set(
            "works",
            Json::Object(
                order
                    .iter()
                    .filter_map(|key| {
                        key.as_str().and_then(|k| v["works"].get(k).map(|x| (k.to_owned(), Json::from(x.clone()))))
                    })
                    .collect(),
            ),
        );
    }
    if let Some(rows) = v["missing"].as_array() {
        out.set("missing", Json::Array(rows.iter().map(character_json).collect()));
    }
    out
}
fn history_json(v: &Value) -> Json {
    let mut out = ordered(v, &["at", "kind", "characters", "changed"]);
    if let Some(rows) = v["characters"].as_array() {
        out.set("characters", Json::Array(rows.iter().map(character_json).collect()));
    }
    out
}

fn backup_db(src: &Connection, path: &Path, ctx: &TaskContext) -> Result<()> {
    use rusqlite::backup::{Backup, StepResult};
    src.busy_timeout(Duration::from_millis(50))?;
    let mut out = crate::sqlite::open(path)?;
    let copy = Backup::new(src, &mut out)?;
    let end = Instant::now() + Duration::from_secs(30);
    loop {
        ctx.check()?;
        match copy.step(256)? {
            StepResult::Done => return Ok(()),
            StepResult::More => {}
            _ if Instant::now() >= end => return bad("索引备份超时"),
            _ => std::thread::sleep(Duration::from_millis(10)),
        }
    }
}

fn read_check(path: &Path) -> Json {
    std::fs::read_to_string(path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(Json::from(Value::Null))
}
fn write_check(path: &Path, v: &Value) -> Result<()> {
    let out = check_json(v).dump();
    std::fs::create_dir_all(path.parent().expect("index file path must have a parent"))?;
    let tmp = path.with_extension(format!("{}.tmp", crate::store::new_key()?));
    std::fs::write(&tmp, out)?;
    std::fs::rename(tmp, path)?;
    Ok(())
}

struct PreparedStage(PathBuf, bool);
impl Drop for PreparedStage {
    fn drop(&mut self) {
        if !self.1 {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

fn update(state: &Mutex<State>, fields: Value) {
    let mut s = state.lock().expect("index state mutex poisoned");
    s.merge(fields);
}

/// The job state and its history for `info`, state first. A job writes its history entry before it reports
/// done, so a done state read first always comes with a history that contains it; reading the history first
/// could pair the previous history with the new done (#85).
fn state_and_history(state: &Mutex<State>, dir: &Path) -> (Value, Vec<Value>) {
    let state = state.lock().expect("index state mutex poisoned").value();
    let history = read(&dir.join("history.json"), json!([])).as_array().cloned().unwrap_or_default();
    (state, history)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn info_never_pairs_a_done_state_with_the_previous_history() {
        // #85: a finished job writes its history entry and then reports done. Hold the state lock while
        // the reader starts, finish the job that way, and only then let the reader take the state.
        let dir = std::env::temp_dir().join(format!("illustash-index-info-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        write(&dir.join("history.json"), &json!([{"kind":"add"}])).unwrap();
        let state = Arc::new(Mutex::new(State::from(json!({"status":"building"}))));
        let mut held = state.lock().unwrap();
        let reader = {
            let (state, dir) = (state.clone(), dir.clone());
            std::thread::spawn(move || state_and_history(&state, &dir))
        };
        // Let the reader get as far as it can before the job finishes. Whatever it reached, a done state
        // must come with the new history; the wait only decides whether a wrong order gets caught.
        std::thread::sleep(std::time::Duration::from_millis(200));
        write(&dir.join("history.json"), &json!([{"kind":"drop"},{"kind":"add"}])).unwrap();
        *held = State::from(json!({"status":"done"}));
        drop(held);
        let (seen, history) = reader.join().unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(seen["status"], "done");
        assert_eq!(history[0]["kind"], "drop", "{history:?}");
    }
}
