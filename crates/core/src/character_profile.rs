//! 角色资料（`set_profile`、`link_local`）与它的撤销（`_undo_profile`），逐行照 tests/reference/web/store.py（#9 已删除）。
//! 名字的语言、作品的可读名、历史文案由核心按旧代码算（名字 N5）。新建作品的 id 由调用方给：给的是 false 时，核心
//! 在真要新建的那一刻以 `Error::NeedId`（退出码 5，标准错误是这个 id 在命令里的 JSON 指针）要一个，调用方抽一个
//! uuid 填进去再执行一次。这样调用方抽 uuid 的次数和先后与旧代码相同。
use crate::character_commands::{begin_action, finish_action, hybrid, invalid, require_entity, text};
use crate::character_labels::{lang_of, Shown};
use crate::character_names::strip;
use crate::{character_graph, character_names, character_projection, legacy_characters, Error, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

/// `_tag_owner`：已经关联这个 Danbooru / WD tag 的身份（沿合并解析；是 besides 自己时算没有）。
pub fn tag_owner(c: &Connection, tag: &str, besides: Option<&str>) -> Result<Option<String>> {
    let row: Option<String> = c
        .query_row(
            "SELECT l.entity_id FROM links l JOIN entities e ON e.id=l.entity_id
             WHERE l.system IN ('danbooru','wd') AND l.key=? AND l.current=1 AND e.kind<>'work' ORDER BY l.system='wd' LIMIT 1",
            [tag],
            |r| r.get(0),
        )
        .optional()?;
    let owner = match row {
        Some(r) => Some(crate::character_commands::resolve(c, &r)?),
        None => None,
    };
    Ok(owner.filter(|o| Some(o.as_str()) != besides))
}

/// `Store._qualifier`：`name_(限定)` 末尾括号里的限定词。
pub fn qualifier(tag: &str) -> &str {
    let Some(body) = tag.strip_suffix(')') else { return "" };
    let Some(open) = body.rfind('(') else { return "" };
    let inner = &body[open + 1..];
    if inner.is_empty() || inner.contains(')') || !body[..open].ends_with('_') {
        return "";
    }
    inner
}

/// `Store._local_work`。
pub fn local_work(c: &Connection, keys: &[&str]) -> Result<Option<String>> {
    for key in keys.iter().filter(|k| !k.is_empty()) {
        for system in ["danbooru", "qualifier"] {
            let row: Option<String> = c
                .query_row(
                    "SELECT l.entity_id FROM links l JOIN entities e ON e.id=l.entity_id
                     WHERE l.system=? AND l.key=? AND e.kind='work' AND e.merged_into IS NULL",
                    [system, key],
                    |r| r.get(0),
                )
                .optional()?;
            if row.is_some() {
                return Ok(row);
            }
        }
    }
    Ok(None)
}

/// 调用方给的新 id；`false`：要调用方给（`Error::NeedId`）；没给：核心自己生成。
pub fn new_id(given: &Value) -> Result<String> {
    match given {
        Value::String(id) if !id.is_empty() => Ok(id.clone()),
        Value::Bool(false) => Err(Error::NeedId(String::new())),
        _ => crate::store::new_key(),
    }
}

/// `Store._work_for`：已有的作品，否则按 Danbooru 版权新建一个链接好的作品（名字是调用方给的可读名）。
pub fn work_for(
    c: &Connection,
    tag: &str,
    copyright: &str,
    readable: &str,
    id: &Value,
    created: &mut Vec<String>,
) -> Result<Option<String>> {
    let work = local_work(c, &[copyright, qualifier(tag)])?;
    if work.is_some() || copyright.is_empty() {
        return Ok(work);
    }
    let work = new_id(id)?;
    c.execute("INSERT INTO entities(id,name,kind) VALUES (?,'','work')", [&work])?;
    c.execute(
        "INSERT INTO work_review(entity_id,status,note) VALUES (?,'confirmed','从 Danbooru 角色的作品自动建立')",
        [&work],
    )?;
    c.execute("INSERT INTO links(system,key,entity_id,source) VALUES ('danbooru',?,?,'user')", [copyright, &work])?;
    c.execute(
        "INSERT OR IGNORE INTO links(system,key,entity_id,source) VALUES ('qualifier',?,?,'user')",
        [copyright, &work],
    )?;
    character_names::put_name(c, &work, "en", "index", readable, "display", 0, 0)?;
    created.push(work.clone());
    Ok(Some(work))
}

/// `Store._set_work`：角色归到作品（work 文字取作品的 qualifier 链接），皮肤跟随。
pub fn set_work(c: &Connection, entity: &str, work: Option<&str>) -> Result<()> {
    let key: Option<String> = match work {
        Some(w) => c
            .query_row("SELECT key FROM links WHERE system='qualifier' AND entity_id=? LIMIT 1", [w], |r| r.get(0))
            .optional()?,
        None => None,
    };
    let text = key.unwrap_or_default();
    c.execute("UPDATE entities SET work_id=?,work=? WHERE id=?", params![work, text, entity])?;
    c.execute("UPDATE entities SET work=? WHERE parent_id=?", [&text, entity])?;
    Ok(())
}

/// works.require：未合并的作品。
pub fn require_work(c: &Connection, ident: &str) -> Result<()> {
    if c.query_row("SELECT 1 FROM entities WHERE id=? AND kind='work' AND merged_into IS NULL", [ident], |_| Ok(()))
        .optional()?
        .is_none()
    {
        return invalid("作品不存在");
    }
    Ok(())
}

fn children(c: &Connection, entity: &str) -> Result<Vec<String>> {
    Ok(c.prepare("SELECT id FROM entities WHERE parent_id=?")?
        .query_map([entity], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

fn danbooru_links(c: &Connection, entity: &str) -> Result<Vec<Value>> {
    let links: Vec<Value> = c
        .prepare("SELECT system,key,current,source,added_at FROM links WHERE entity_id=? AND system='danbooru'")?
        .query_map([entity], |r| {
            Ok(json!([
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?
            ]))
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(links)
}

fn snapshot(c: &Connection, entity: &str) -> Result<Value> {
    let (work, work_id): (String, Option<String>) =
        c.query_row("SELECT work,work_id FROM entities WHERE id=?", [entity], |r| Ok((r.get(0)?, r.get(1)?)))?;
    let links = danbooru_links(c, entity)?;
    let names: Vec<Value> = c
        .prepare("SELECT lang,value,source,role,preferred,position FROM names WHERE entity_id=? AND source='user'")?
        .query_map([entity], |r| {
            Ok(json!([
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, i64>(5)?
            ]))
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(json!({"profile": {"entity": entity, "work": work, "work_id": work_id, "links": links, "names": names}}))
}

/// 一个角色的资料修改（`_apply_profile`）；返回它的撤销快照。p：name / aliases / work / tag / copyright /
/// work_id（新建作品用的 id）。缺省的字段不改（Python 的 None）。
fn apply(c: &Connection, p: &Value, touched: &mut Vec<String>) -> Result<Value> {
    let (entity, parent) = require_entity(c, &p["entity"])?;
    if parent.is_some() {
        return invalid("请编辑角色本身，子标签跟随角色");
    }
    let (oc, mark): (i64, i64) =
        c.query_row("SELECT original,label_only FROM entities WHERE id=?", [&entity], |r| Ok((r.get(0)?, r.get(1)?)))?;
    let before = snapshot(c, &entity)?;
    let tag = p.get("tag").filter(|v| !v.is_null());
    let work = p.get("work").filter(|v| !v.is_null());
    if let Some(name) = p.get("name").filter(|v| !v.is_null()) {
        let Some(name) = name.as_str().filter(|n| strip(n).chars().count() <= 120) else {
            return invalid("名称最多 120 个字符");
        };
        let name = strip(name);
        let linked = match tag {
            Some(t) => !t.as_str().unwrap_or("").is_empty(),
            None => c
                .query_row("SELECT 1 FROM links WHERE entity_id=? AND current=1", [&entity], |_| Ok(()))
                .optional()?
                .is_some(),
        };
        if name.is_empty() && (oc != 0 || !linked) {
            return invalid("没有关联 Danbooru 的角色需要一个名称");
        }
        c.execute("DELETE FROM names WHERE entity_id=? AND role='display' AND source='user'", [&entity])?;
        if !name.is_empty() {
            character_names::put_name(c, &entity, lang_of(name), "user", name, "display", 0, 1)?;
        }
    }
    if let Some(aliases) = p.get("aliases").filter(|v| !v.is_null()) {
        let list = aliases.as_array().filter(|a| a.len() <= 50);
        let Some(list) = list.filter(|l| {
            l.iter().all(|a| a.as_str().is_some_and(|s| !strip(s).is_empty() && strip(s).chars().count() <= 120))
        }) else {
            return invalid("别名最多 50 个，每个 1–120 个字符");
        };
        c.execute("DELETE FROM names WHERE entity_id=? AND role='alias' AND source='user'", [&entity])?;
        let mut seen: Vec<&str> = Vec::new();
        for a in list {
            let a = strip(a.as_str().expect("validated internal text field"));
            if !seen.contains(&a) {
                seen.push(a);
            }
        }
        for (i, a) in seen.iter().enumerate() {
            character_names::put_name(c, &entity, "", "user", a, "alias", i as i64, 0)?;
        }
    }
    let mut created = Vec::new();
    if let Some(tag) = tag {
        if mark != 0 {
            return invalid("未细分的原创角色不能关联 Danbooru");
        }
        c.execute("UPDATE links SET current=0 WHERE entity_id=? AND system='danbooru'", [&entity])?;
        let tag = tag.as_str().unwrap_or("");
        if !tag.is_empty() {
            if let Some(owner) = tag_owner(c, tag, Some(&entity))? {
                return Err(Error::Invalid(format!(
                    "{} 已经关联了 {tag}：它们是同一个角色的话，请用「合并到另一个角色」",
                    Shown::new().name(c, &owner)?
                )));
            }
            c.execute(
                "INSERT INTO links(system,key,entity_id,current,source) VALUES ('danbooru',?,?,1,'user')
                 ON CONFLICT(system,key) DO UPDATE SET current=1,source='user',entity_id=excluded.entity_id",
                [tag, &entity],
            )?;
            let work_id: Option<String> =
                c.query_row("SELECT work_id FROM entities WHERE id=?", [&entity], |r| r.get(0))?;
            if work.is_none() && oc == 0 && work_id.is_none() {
                let copyright = p["copyright"].as_str().unwrap_or("");
                let readable = crate::names_view::readable(copyright, false);
                let w = work_for(c, tag, copyright, &readable, &p["work_id"], &mut created)?;
                set_work(c, &entity, w.as_deref())?;
            }
        }
    }
    if let Some(work) = work {
        let work = crate::character_commands::work_arg(work)?;
        if let Some(w) = work {
            require_work(c, w)?;
            if oc != 0 {
                return invalid("原创角色不能归属作品");
            }
        }
        set_work(c, &entity, work)?;
    }
    touched.push(entity.clone());
    touched.extend(children(c, &entity)?);
    touched.extend(created);
    Ok(before)
}

/// `_profile_changed`：链接改了且身份层启用时重建图、全量重算证据与归属。
pub fn changed(c: &Connection) -> Result<()> {
    if hybrid(c)? {
        character_graph::rebuild(c)?;
        character_projection::evidence(c, None)?;
        character_projection::memberships(c, None)?;
    }
    Ok(())
}

pub fn set_profile(c: &Connection, args: &Value) -> Result<Value> {
    let mut touched = Vec::new();
    let before = apply(c, args, &mut touched).map_err(|e| match e {
        Error::NeedId(_) => Error::NeedId("/work_id".into()),
        e => e,
    })?;
    // 文案用改完之后的名字（旧代码在写完之后读 entity_name）。
    let shown = Shown::new().name(c, text(&before["profile"]["entity"], "资料快照不完整")?)?;
    let action = begin_action(c, &format!("已更新 {shown} 的资料"))?;
    legacy_characters::mirror_state(c, &touched, "user")?;
    if args.get("tag").is_some_and(|t| !t.is_null()) {
        changed(c)?;
    }
    let id = before["profile"]["entity"].clone();
    finish_action(c, action, "profile", vec![before])?;
    Ok(json!({"action": action, "id": id}))
}

pub fn link_local(c: &Connection, args: &Value) -> Result<Value> {
    let Some(items) = args["items"].as_array().filter(|a| (1..=200).contains(&a.len())) else {
        return invalid("请选择要关联的角色");
    };
    let action = begin_action(c, &format!("已把 {} 个自建角色关联到 Danbooru", items.len()))?;
    let mut before = Vec::new();
    let mut touched = Vec::new();
    for (i, item) in items.iter().enumerate() {
        let entity = crate::character_commands::resolve(c, text(&item["id"], "请选择角色")?)?;
        let values = |sql: &str| -> Result<Vec<String>> {
            Ok(c.prepare(sql)?.query_map([&entity], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?)
        };
        let typed = values("SELECT value FROM names WHERE entity_id=? AND role='display' AND source='user'")?;
        let kept =
            values("SELECT value FROM names WHERE entity_id=? AND role='alias' AND source='user' ORDER BY position")?;
        let mut p = item.clone();
        p["entity"] = item["id"].clone();
        p["name"] = json!("");
        p["aliases"] = json!(kept.into_iter().chain(typed).collect::<Vec<_>>());
        before.push(apply(c, &p, &mut touched).map_err(|e| match e {
            Error::NeedId(_) => Error::NeedId(format!("/items/{i}/work_id")),
            e => e,
        })?);
    }
    legacy_characters::mirror_state(c, &touched, "user")?;
    changed(c)?;
    let ids: Vec<Value> = before.iter().map(|b| b["profile"]["entity"].clone()).collect();
    finish_action(c, action, "profile", before)?;
    Ok(json!({"action": action, "ids": ids}))
}

/// `_undo_profile`：用户名字、Danbooru 链接、作品按快照恢复，皮肤的 work 跟随。
pub fn undo(c: &Connection, p: &Value) -> Result<bool> {
    let entity = text(&p["entity"], "资料快照不完整")?;
    c.execute("DELETE FROM names WHERE entity_id=? AND source='user'", [entity])?;
    for n in p["names"].as_array().into_iter().flatten() {
        c.execute(
            "INSERT OR REPLACE INTO names(entity_id,lang,value,source,role,preferred,position) VALUES (?,?,?,?,?,?,?)",
            params![entity, n[0].as_str(), n[1].as_str(), n[2].as_str(), n[3].as_str(), n[4].as_i64(), n[5].as_i64()],
        )?;
    }
    let links_changed = Value::Array(danbooru_links(c, entity)?) != p["links"];
    if links_changed {
        c.execute("DELETE FROM links WHERE entity_id=? AND system='danbooru'", [entity])?;
        for l in p["links"].as_array().into_iter().flatten() {
            c.execute(
                "INSERT OR REPLACE INTO links(system,key,current,source,added_at,entity_id) VALUES (?,?,?,?,?,?)",
                params![l[0].as_str(), l[1].as_str(), l[2].as_i64(), l[3].as_str(), l[4].as_str(), entity],
            )?;
        }
    }
    c.execute(
        "UPDATE entities SET work=?1,work_id=?2 WHERE id=?3 AND (work IS NOT ?1 OR work_id IS NOT ?2)",
        params![p["work"].as_str(), p["work_id"].as_str(), entity],
    )?;
    c.execute(
        "UPDATE entities SET work=?1 WHERE parent_id=?2 AND work IS NOT ?1",
        params![p["work"].as_str(), entity],
    )?;
    let mut ids = vec![entity.to_owned()];
    ids.extend(children(c, entity)?);
    legacy_characters::mirror_state(c, &ids, "user")?;
    Ok(links_changed)
}

/// 撤销名字也须完成旧版的延迟派生刷新；仅真实链接变化才重建身份图。
pub fn after_undo(c: &Connection, links_changed: bool) -> Result<()> {
    if hybrid(c)? {
        if links_changed {
            character_graph::rebuild(c)?;
        }
        character_projection::refresh_delayed(c)?;
    }
    Ok(())
}
