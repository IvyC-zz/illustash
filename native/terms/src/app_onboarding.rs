//! 新图库的首次引导（#96）：引导走到哪一步、这台电脑能识别什么、首次同步得到了什么，以及按已认出的角色推荐
//! 可能想关注的作品。进度按图库存在 prefs（键 `onboarding`）；记录进度不授权联网、不启动同步或索引任务。
//! 唯一的计算是用户打开引导时一次识别组件自检（`--check`，实际加载模型），结果只留在本次会话。
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
use crate::{
    app_library::Library,
    app_tasks::{Task, TaskIdentity},
    http_json::Json,
    Error, Result,
};
use rusqlite::{Connection, OptionalExtension};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

const PREF: &str = "onboarding";
/// 引导的步骤，按顺序：识别能力、联网选择、首次同步与结果、认出更多角色、开始整理。
pub(crate) const STEPS: [&str; 5] = ["ability", "network", "sync", "more", "start"];
/// 推荐作品最多几部。
const SUGGESTED: usize = 5;

fn bad<T>(message: &str) -> Result<T> {
    Err(Error::Invalid(message.into()))
}

/// 引导状态。存过的按存的来；没存过的：还没有一次同步走完的图库是新图库（`new`，界面打开时写入），
/// 其余（升级上来的旧图库、导入的归档）不弹引导（`off`），可以从设置里重新打开。
fn guide(c: &Connection) -> Result<Value> {
    let stored: Option<String> = c.query_row("SELECT value FROM prefs WHERE key=?", [PREF], |r| r.get(0)).optional()?;
    if let Some(text) = stored {
        let v: Value = serde_json::from_str(&text).map_err(|e| Error::Invalid(format!("无效的引导记录：{e}")))?;
        let status = v["status"].as_str().filter(|s| ["active", "skipped", "done"].contains(s)).unwrap_or("off");
        let step = v["step"].as_str().filter(|s| STEPS.contains(s)).unwrap_or(STEPS[0]);
        return Ok(json!({"status": status, "step": step}));
    }
    let synced: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM sync_runs WHERE status IN ('done','needs_recognition','needs_windows'))",
        [],
        |r| r.get(0),
    )?;
    Ok(json!({"status": if synced { "off" } else { "new" }, "step": STEPS[0]}))
}

fn save(lib: &Library, status: &str, step: &str) -> Result<()> {
    crate::app_settings::update_pref(lib, PREF, |_| Ok(json!({"status": status, "step": step}).to_string()))
}

/// 首次同步的结果分开算：在图库里（可见）、识别过（有 WD 分级，即识别结果已导入）、认出了角色、仍未识别。
fn results(c: &Connection) -> Result<Value> {
    let visible = "NOT EXISTS(SELECT 1 FROM duplicates h WHERE h.uuid=a.uuid AND h.hidden=1) \
                   AND NOT EXISTS(SELECT 1 FROM excluded x WHERE x.uuid=a.uuid)";
    let count = |clause: &str| -> Result<i64> {
        Ok(c.query_row(&format!("SELECT count(*) FROM assets a WHERE {visible}{clause}"), [], |r| r.get(0))?)
    };
    let characters: i64 = c.query_row(
        &format!("SELECT count(DISTINCT m.entity) FROM memberships m JOIN assets a ON a.uuid=m.uuid WHERE {visible}"),
        [],
        |r| r.get(0),
    )?;
    Ok(json!({
        "total": count("")?,
        "recognized": count(" AND EXISTS(SELECT 1 FROM asset_rating r WHERE r.uuid=a.uuid)")?,
        "with_character": count(" AND EXISTS(SELECT 1 FROM memberships m WHERE m.uuid=a.uuid)")?,
        "unmatched": count(
            " AND NOT EXISTS(SELECT 1 FROM memberships m WHERE m.uuid=a.uuid) \
             AND NOT EXISTS(SELECT 1 FROM tag_memberships t WHERE t.uuid=a.uuid) \
             AND NOT EXISTS(SELECT 1 FROM deferred d WHERE d.uuid=a.uuid)"
        )?,
        "characters": characters,
    }))
}

/// 按已认出的角色推荐作品：角色已归到的作品，或角色 tag 末尾的作品限定词（`name_(作品)`）。
/// 限定词同时是普通画面标签的（如 swimsuit）不当作品。按图片数取前几部，已关注的也列出并标明。
fn suggestions(c: &Connection, general: &[String], watched: &[String]) -> Result<Value> {
    let general: BTreeSet<&str> = general.iter().map(String::as_str).collect();
    let mut keys: HashMap<String, Option<String>> = HashMap::new(); // entity → work key
    let mut pictures: BTreeMap<String, BTreeSet<String>> = BTreeMap::new(); // work key → assets
    let mut rows = c.prepare(
        "SELECT m.uuid,e.id,e.work_id FROM memberships m JOIN entities e ON e.id=m.entity JOIN assets a ON a.uuid=m.uuid
         WHERE NOT EXISTS(SELECT 1 FROM duplicates h WHERE h.uuid=a.uuid AND h.hidden=1)
           AND NOT EXISTS(SELECT 1 FROM excluded x WHERE x.uuid=a.uuid)",
    )?;
    let mut tags =
        c.prepare_cached("SELECT key FROM links WHERE entity_id=? AND system IN ('wd','danbooru','ccip')")?;
    for row in
        rows.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<String>>(2)?)))?
    {
        let (uuid, entity, work) = row?;
        let key = match keys.get(&entity) {
            Some(key) => key.clone(),
            None => {
                let key = match work.filter(|w| !w.is_empty()) {
                    Some(work) => Some(format!("id:{work}")),
                    None => tags
                        .query_map([&entity], |r| r.get::<_, String>(0))?
                        .collect::<rusqlite::Result<Vec<_>>>()?
                        .iter()
                        .map(|t| crate::character_profile::qualifier(t))
                        .find(|q| !q.is_empty() && !general.contains(q))
                        .map(|q| format!("tag:{q}")),
                };
                keys.insert(entity, key.clone());
                key
            }
        };
        if let Some(key) = key {
            pictures.entry(key).or_default().insert(uuid);
        }
    }
    // A qualifier that names a work this library already has counts with that work.
    let mut merged: BTreeMap<String, (Option<String>, String, BTreeSet<String>)> = BTreeMap::new();
    for (key, assets) in pictures {
        let (id, tag) = if let Some(id) = key.strip_prefix("id:") {
            (Some(id.to_owned()), String::new())
        } else {
            let tag = key.trim_start_matches("tag:").to_owned();
            (crate::character_profile::local_work(c, &[&tag])?, tag)
        };
        let slot = merged.entry(id.clone().map_or(format!("tag:{tag}"), |id| format!("id:{id}"))).or_insert((
            id,
            tag,
            BTreeSet::new(),
        ));
        slot.2.extend(assets);
    }
    let mut ranked: Vec<_> = merged.into_values().collect();
    ranked.sort_by(|a, b| b.2.len().cmp(&a.2.len()).then_with(|| a.1.cmp(&b.1)));
    let mut out = Vec::new();
    for (id, tag, assets) in ranked.into_iter().take(SUGGESTED) {
        let (name, tag, watchable) = match &id {
            Some(id) => {
                let (name, linked): (String, Option<String>) = c.query_row(
                    "SELECT entity_name(?1),(SELECT key FROM links WHERE entity_id=?1 AND system='danbooru' AND current=1 LIMIT 1)",
                    [id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?;
                (name, linked.clone().unwrap_or(tag), linked.is_some())
            }
            None => (crate::names_view::readable(&tag, false), tag, true),
        };
        out.push(json!({
            "id": id,
            "tag": tag,
            "name": name,
            "count": assets.len(),
            "watchable": watchable,
            "watched": id.as_ref().is_some_and(|id| watched.contains(id)),
        }));
    }
    Ok(Value::Array(out))
}

/// 识别组件：本次启动配置的识别 worker 是否存在，以及（用户打开引导时）一次实际加载模型的自检。
pub(crate) struct Onboarding {
    check: Arc<Mutex<Value>>,
    task: Option<Task>,
    generation: u64,
    session: String,
}
impl Onboarding {
    pub fn new(generation: u64, session: &str) -> Self {
        Self { check: Arc::new(Mutex::new(json!({"status": "idle"}))), task: None, generation, session: session.into() }
    }
    pub fn stop(&mut self) {
        if let Some(mut task) = self.task.take() {
            task.stop();
        }
    }
    fn worker(name: &str) -> Option<std::path::PathBuf> {
        std::env::var_os(name).map(std::path::PathBuf::from).filter(|p| p.is_file())
    }
    fn capability(&self, shared: Option<&Path>) -> Value {
        let anchors = shared
            .and_then(|s| std::fs::read_dir(s.join("char-index")).ok())
            .map(|d| d.filter_map(|e| e.ok()).filter(|e| e.path().extension().is_some_and(|x| x == "json")).count())
            .unwrap_or(0);
        json!({
            "recognition": Self::worker("ILLUSTASH_RECOGNITION_WORKER").is_some(),
            "index": Self::worker("ILLUSTASH_INDEX_WORKER").is_some(),
            "anchors": anchors,
            "check": self.check.lock().expect("onboarding check mutex poisoned").clone(),
        })
    }
    /// 识别组件自检。同一次会话只跑一次（失败后可以再点）；没有组件时直接报告，不启动进程。
    fn start_check(&mut self, lib: &Library) -> Result<()> {
        let status = self.check.lock().expect("onboarding check mutex poisoned")["status"].clone();
        if status == "checking" || status == "ok" {
            return Ok(());
        }
        let Some(worker) = Self::worker("ILLUSTASH_RECOGNITION_WORKER") else {
            *self.check.lock().expect("onboarding check mutex poisoned") =
                json!({"status": "failed", "message": "没有识别组件"});
            return Ok(());
        };
        *self.check.lock().expect("onboarding check mutex poisoned") = json!({"status": "checking"});
        let state = self.check.clone();
        let identity = TaskIdentity::new(lib, self.generation, &self.session)?;
        let log = std::env::temp_dir().join(format!("illustash-check-{}-{}.log", std::process::id(), self.generation));
        self.task.take();
        self.task = Some(Task::start(identity, Some(Duration::from_secs(180)), move |ctx| {
            let outcome = (|| -> Result<Value> {
                let mut command = std::process::Command::new(&worker);
                command.arg("--check").stderr(std::fs::File::create(&log)?);
                let status = ctx.process(&mut command)?;
                if status.success() {
                    return Ok(json!({"status": "ok"}));
                }
                let said = std::fs::read_to_string(&log).unwrap_or_default();
                let last = said.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
                let last: String = last.chars().take(300).collect();
                Ok(
                    json!({"status": "failed", "message": if last.is_empty() { format!("自检退出：{status}") } else { last }}),
                )
            })();
            let _ = std::fs::remove_file(&log);
            let value = outcome.unwrap_or_else(
                |e| json!({"status": "failed", "message": e.to_string().chars().take(300).collect::<String>()}),
            );
            *state.lock().expect("onboarding check mutex poisoned") = value;
            Ok(())
        }));
        Ok(())
    }
    pub fn info(
        &self,
        lib: &Library,
        shared: Option<&Path>,
        general: Option<&[String]>,
        watched: &[String],
    ) -> Result<Json> {
        let c = crate::app_library::read_connection(&lib.db())?;
        crate::http_read::register_names(&c)?;
        let mut out = json!({
            "guide": if lib.frozen() { json!({"status": "off", "step": STEPS[0]}) } else { guide(&c)? },
            "capability": self.capability(shared),
            "results": results(&c)?,
        });
        if let Some(general) = general {
            out["works"] = suggestions(&c, general, watched)?;
        }
        Ok(out.into())
    }
    /// The guide for the session's library; `works` adds the suggested works (reads the whole membership table).
    pub(crate) fn read(
        session: &crate::app_service::Session,
        root: &Path,
        context: &crate::http::LibraryContext,
        works: bool,
    ) -> Result<Json> {
        let shared = crate::app_resources::shared_path(root).ok();
        let general = if works { Some(crate::http_read::tag_vocabulary(context)?) } else { None };
        let watched = if works { session.index.watched(&session.library)? } else { Vec::new() };
        session.onboarding.info(&session.library, shared.as_deref(), general.as_deref().map(Vec::as_slice), &watched)
    }
    /// `{action: step|skip|done|reopen|check, step?}`。
    pub fn post(&mut self, lib: &Library, body: &Json) -> Result<()> {
        let body: Value = serde_json::from_str(&body.dump()).map_err(|e| Error::Invalid(e.to_string()))?;
        if lib.frozen() {
            return bad("测试图库没有引导");
        }
        let current = guide(&crate::app_library::read_connection(&lib.db())?)?;
        let step = current["step"].as_str().unwrap_or(STEPS[0]).to_owned();
        match body["action"].as_str() {
            Some("step") => {
                let Some(next) = body["step"].as_str().filter(|s| STEPS.contains(s)) else {
                    return bad("未知的引导步骤");
                };
                save(lib, "active", next)
            }
            Some("skip") => save(lib, "skipped", &step),
            Some("done") => save(lib, "done", &step),
            Some("reopen") => save(lib, "active", STEPS[0]),
            Some("check") => self.start_check(lib),
            _ => bad("未知操作"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::OpenFlags;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Root(std::path::PathBuf);
    impl Drop for Root {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn fixture() -> (Root, Library) {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "illustash-onboarding-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let pictures = root.join("pictures");
        std::fs::create_dir_all(&pictures).unwrap();
        let dest = root.join("new.pilib");
        crate::app_create::create_folder(&dest, &pictures, Some("new"), None, None).unwrap();
        let lib = Library::open(&dest).unwrap();
        (Root(root), lib)
    }
    fn read(o: &Onboarding, lib: &Library, general: Option<&[String]>) -> Value {
        serde_json::from_str(&o.info(lib, None, general, &[]).unwrap().dump()).unwrap()
    }

    #[test]
    fn a_new_library_starts_the_guide_and_its_choices_are_kept() {
        let (_root, lib) = fixture();
        let mut o = Onboarding::new(1, "token");
        assert_eq!(read(&o, &lib, None)["guide"], json!({"status": "new", "step": "ability"}));
        o.post(&lib, &Json::from(json!({"action": "step", "step": "sync"}))).unwrap();
        assert_eq!(read(&o, &lib, None)["guide"], json!({"status": "active", "step": "sync"}));
        assert!(o.post(&lib, &Json::from(json!({"action": "step", "step": "elsewhere"}))).is_err());
        o.post(&lib, &Json::from(json!({"action": "skip"}))).unwrap();
        assert_eq!(read(&o, &lib, None)["guide"], json!({"status": "skipped", "step": "sync"}));
        o.post(&lib, &Json::from(json!({"action": "reopen"}))).unwrap();
        assert_eq!(read(&o, &lib, None)["guide"], json!({"status": "active", "step": "ability"}));
        o.post(&lib, &Json::from(json!({"action": "done"}))).unwrap();
        assert_eq!(read(&o, &lib, None)["guide"]["status"], "done");
    }

    #[test]
    fn a_library_that_has_synced_before_does_not_open_the_guide() {
        let (_root, lib) = fixture();
        let c = crate::sqlite::open_with_flags(lib.db(), OpenFlags::SQLITE_OPEN_READ_WRITE).unwrap();
        c.execute("INSERT INTO sync_runs(status,started_at) VALUES ('interrupted','2026-10-01 10:00:00')", []).unwrap();
        let o = Onboarding::new(1, "token");
        assert_eq!(read(&o, &lib, None)["guide"]["status"], "new", "an unfinished first sync is still new");
        c.execute("INSERT INTO sync_runs(status,started_at) VALUES ('done','2026-10-01 11:00:00')", []).unwrap();
        assert_eq!(read(&o, &lib, None)["guide"], json!({"status": "off", "step": "ability"}));
    }

    #[test]
    fn suggestions_group_characters_by_work_and_skip_picture_tags() {
        let (_root, lib) = fixture();
        let c = crate::sqlite::open_with_flags(lib.db(), OpenFlags::SQLITE_OPEN_READ_WRITE).unwrap();
        // Seeded as the core would have: entities and links are written only while holding its key.
        c.execute("INSERT INTO meta(key,value) VALUES (?1,'1')", [crate::schema::WRITER_KEY]).unwrap();
        c.execute_batch(
            "INSERT INTO assets(uuid) VALUES ('a1'),('a2'),('a3'),('a4');
             INSERT INTO entities(id,name,kind) VALUES ('hoshino','','character'),('shiroko','','character'),
               ('swim','','character'),('miku','','character');
             INSERT INTO links(system,key,entity_id,source) VALUES ('wd','hoshino_(blue_archive)','hoshino','import'),
               ('wd','shiroko_(blue_archive)','shiroko','import'),('wd','artoria_(swimsuit)','swim','import'),
               ('wd','hatsune_miku','miku','import');
             INSERT INTO memberships(uuid,entity,status,sources,source_count,ccip_only,skey)
               SELECT column1,column2,'auto','WD',1,0,'' FROM (VALUES ('a1','hoshino'),('a2','hoshino'),
               ('a2','shiroko'),('a3','swim'),('a4','miku'));",
        )
        .unwrap();
        c.execute("DELETE FROM meta WHERE key=?1", [crate::schema::WRITER_KEY]).unwrap();
        let o = Onboarding::new(1, "token");
        let out = read(&o, &lib, Some(&["swimsuit".to_owned()]));
        assert_eq!(
            out["works"],
            json!([{"id": null, "tag": "blue_archive", "name": "Blue Archive", "count": 2, "watchable": true, "watched": false}])
        );
        assert_eq!(out["results"]["with_character"], 4);
        assert_eq!(out["results"]["characters"], 4);
    }

    #[test]
    fn a_missing_recognition_component_is_reported_without_a_process() {
        let (_root, lib) = fixture();
        let mut o = Onboarding::new(1, "token");
        if std::env::var_os("ILLUSTASH_RECOGNITION_WORKER").is_some() {
            return;
        }
        o.post(&lib, &Json::from(json!({"action": "check"}))).unwrap();
        let out = read(&o, &lib, None);
        assert_eq!(out["capability"]["recognition"], false);
        assert_eq!(out["capability"]["check"], json!({"status": "failed", "message": "没有识别组件"}));
        assert!(o.task.is_none());
    }
}
