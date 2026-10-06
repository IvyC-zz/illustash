//! 身份与作品的结构命令（方案第 8 节「不进历史」的那些）：建身份、改类型、当作标签、归作品、作品编辑、
//! 作品证据（`works.apply_evidence`）。逐行照 tests/reference/web/store.py、tests/reference/web/works.py；作品的可读名由调用方给，
//! 当前显示名由核心读（character_labels）。
use crate::character_commands::{hybrid, invalid, require_entity, text};
use crate::character_profile::{new_id, require_work, set_work, tag_owner, work_for};
use crate::{character_graph, character_names, character_projection, legacy_characters, Error, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

fn oc_fields(args: &Value) -> Result<(String, String)> {
    let kind = args["kind"].as_str().unwrap_or("");
    if !matches!(kind, "" | "oc") {
        return invalid("未知角色类型");
    }
    let artist = match &args["artist"] {
        Value::Null => "",
        Value::String(s) => s.trim(),
        _ => return invalid("画师名称不正确"),
    };
    if artist.chars().count() > 120 || (!artist.is_empty() && kind != "oc") {
        return invalid("画师名称不正确");
    }
    Ok((kind.to_owned(), artist.to_owned()))
}

/// 新身份尚无候选或人工归属；图重建已刷新受影响图片，不再对全库重复派生。
fn created(c: &Connection, ids: &[String]) -> Result<()> {
    legacy_characters::mirror_state(c, ids, "user")?;
    if hybrid(c)? {
        character_graph::rebuild(c)?;
        character_projection::refresh_delayed(c)?;
    }
    Ok(())
}

/// `create_entity`（调用方已确认没有同名同链接的身份可复用；原创角色的「原创角色 N」也由调用方算好）。
fn create_entity(c: &Connection, args: &Value) -> Result<Value> {
    let (kind, artist) = oc_fields(args)?;
    let name = text(&args["name"], "角色名称应为 1–120 个字符")?.trim();
    if name.is_empty() || name.chars().count() > 120 {
        return invalid("角色名称应为 1–120 个字符");
    }
    let id = new_id(&args["id"])?;
    c.execute(
        "INSERT INTO entities(id,name,kind,artist,original) VALUES (?,'','character',?,?)",
        params![id, artist, (kind == "oc") as i64],
    )?;
    character_names::put_name(c, &id, text(&args["lang"], "缺少名字的语言")?, "user", name, "display", 0, 1)?;
    created(c, std::slice::from_ref(&id))?;
    Ok(json!({"id": id}))
}

fn create_linked(c: &Connection, args: &Value) -> Result<Value> {
    let tag = text(&args["tag"], "请选择 Danbooru 角色 tag")?;
    if let Some(owner) = tag_owner(c, tag, None)? {
        return Ok(json!({"id": owner, "existing": true}));
    }
    let id = new_id(&args["id"])?;
    c.execute("INSERT INTO entities(id,name,kind) VALUES (?,'','character')", [&id])?;
    c.execute("INSERT INTO links(system,key,entity_id,source) VALUES ('danbooru',?,?,'user')", [tag, &id])?;
    let mut ids = vec![id.clone()];
    let work = work_for(
        c,
        tag,
        args["copyright"].as_str().unwrap_or(""),
        args["readable"].as_str().unwrap_or(""),
        &args["work_id"],
        &mut ids,
    )?;
    set_work(c, &id, work.as_deref())?;
    if let Some(alias) = args["alias"].as_str().filter(|a| !a.is_empty()) {
        character_names::put_name(c, &id, "", "user", alias, "alias", 0, 0)?;
    }
    created(c, &ids)?;
    Ok(json!({"id": id}))
}

pub const SHARED_OC: &str = "原创角色（未细分）";

fn shared_oc(c: &Connection, args: &Value) -> Result<Value> {
    let row: Option<String> = c
        .query_row(
            "SELECT id FROM entities WHERE label_only=1 AND merged_into IS NULL ORDER BY rowid LIMIT 1",
            [],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(id) = row {
        return Ok(json!({"id": id, "existing": true}));
    }
    let id = new_id(&args["id"])?;
    c.execute("INSERT INTO entities(id,name,kind,original,label_only) VALUES (?,'','character',1,1)", [&id])?;
    character_names::put_name(c, &id, "zh", "user", SHARED_OC, "display", 0, 1)?;
    created(c, std::slice::from_ref(&id))?;
    Ok(json!({"id": id}))
}

/// `set_kind`。
fn set_kind(c: &Connection, args: &Value) -> Result<Value> {
    let (kind, artist) = oc_fields(args)?;
    let (entity, parent) = require_entity(c, &args["entity"])?;
    if parent.is_some() {
        return invalid("只能标记角色本身，不能标记子标签");
    }
    let (original, label_only): (i64, i64) =
        c.query_row("SELECT original,label_only FROM entities WHERE id=?", [&entity], |r| Ok((r.get(0)?, r.get(1)?)))?;
    if kind != "oc" && label_only != 0 {
        return invalid("未细分的原创角色只能是原创角色");
    }
    let oc = kind == "oc";
    if oc && original != 1 {
        // 标成原创角色：把现在的显示名固定成用户名字（旧代码读 entity_name）。
        let shown = crate::character_labels::Shown::new().name(c, &entity)?;
        character_names::put_name(
            c,
            &entity,
            crate::character_labels::lang_of(&shown),
            "user",
            &shown,
            "display",
            0,
            1,
        )?;
    }
    c.execute(
        "UPDATE entities SET original=?,artist=?,work_id=CASE WHEN ? THEN NULL ELSE work_id END,work=CASE WHEN ? THEN '' ELSE work END WHERE id=?",
        params![oc as i64, artist, oc, oc, entity],
    )?;
    legacy_characters::mirror_state(c, std::slice::from_ref(&entity), "user")?;
    Ok(json!({"id": entity, "kind": kind, "artist": artist}))
}

fn set_mascot(c: &Connection, args: &Value) -> Result<Value> {
    let Some(mascot) = args["mascot"].as_bool() else { return invalid("参数不正确") };
    let (entity, parent) = require_entity(c, &args["entity"])?;
    if parent.is_some() {
        return invalid("只能标记角色本身，不能标记子标签");
    }
    c.execute("UPDATE entities SET mascot=?,mascot_origin='user' WHERE id=?", params![mascot as i64, entity])?;
    let uuids: Vec<String> = c
        .prepare(
            "SELECT uuid FROM family_evidence WHERE entity=?1 UNION
             SELECT s.uuid FROM assertions s JOIN entities x ON x.id=s.entity WHERE coalesce(x.parent_id,x.id)=?1",
        )?
        .query_map([&entity], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    legacy_characters::mirror_state(c, std::slice::from_ref(&entity), "user")?;
    if !uuids.is_empty() {
        character_projection::evidence(c, Some(&uuids))?;
        character_projection::memberships(c, Some(&uuids))?;
    }
    Ok(json!({"id": entity, "mascot": mascot}))
}

fn set_character_work(c: &Connection, args: &Value) -> Result<Value> {
    let (entity, parent) = require_entity(c, &args["entity"])?;
    if parent.is_some() {
        return invalid("皮肤跟随角色的作品");
    }
    let work = crate::character_commands::work_arg(&args["work"])?;
    if let Some(w) = work {
        require_work(c, w)?;
        let original: i64 = c.query_row("SELECT original FROM entities WHERE id=?", [&entity], |r| r.get(0))?;
        if original != 0 {
            return invalid("原创角色不能归属作品");
        }
    }
    set_work(c, &entity, work)?;
    let mut ids = vec![entity.clone()];
    ids.extend(children(c, &entity)?);
    legacy_characters::mirror_state(c, &ids, "user")?;
    Ok(json!({"id": entity, "work_id": work}))
}

fn children(c: &Connection, entity: &str) -> Result<Vec<String>> {
    Ok(c.prepare("SELECT id FROM entities WHERE parent_id=?")?
        .query_map([entity], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

// ---- 作品（tests/reference/web/works.py） ----

/// works.TREE：作品和它的全部子作品（UNION 防旧数据成环）。
fn descendants(c: &Connection, ident: &str) -> Result<Vec<String>> {
    require_work(c, ident)?;
    Ok(c.prepare(
        "WITH RECURSIVE tree(id) AS (SELECT id FROM entities WHERE id=? AND kind='work'
          UNION SELECT e.id FROM entities e JOIN tree t ON e.parent_id=t.id WHERE e.kind='work') SELECT id FROM tree",
    )?
    .query_map([ident], |r| r.get(0))?
    .collect::<rusqlite::Result<_>>()?)
}

fn work_create(c: &Connection, args: &Value) -> Result<String> {
    let title = args["name"].as_str().map(str::trim).unwrap_or("");
    if title.is_empty() || title.chars().count() > 120 {
        return invalid("请输入 1–120 字的作品名");
    }
    let parent = args["parent"].as_str().filter(|p| !p.is_empty());
    if let Some(p) = parent {
        require_work(c, p)?;
    }
    let ident = new_id(&args["id"])?;
    c.execute("INSERT INTO entities(id,name,kind,parent_id) VALUES (?,'','work',?)", params![ident, parent])?;
    c.execute("INSERT INTO work_review(entity_id,parent_locked) VALUES (?,1)", [&ident])?;
    character_names::put_name(c, &ident, text(&args["lang"], "缺少名字的语言")?, "user", title, "display", 0, 1)?;
    if let Some(tag) = args["tag"].as_str().filter(|t| !t.is_empty()) {
        work_link(c, &ident, tag)?;
    }
    Ok(ident)
}

fn work_link(c: &Connection, ident: &str, tag: &str) -> Result<()> {
    require_work(c, ident)?;
    let tag = tag.trim();
    if tag.is_empty() || tag.chars().count() > 200 || tag.chars().any(char::is_whitespace) {
        return invalid("请输入有效的 Danbooru 作品 tag");
    }
    let tag = tag.to_lowercase();
    let owner: Option<String> = c
        .query_row("SELECT entity_id FROM links WHERE system='danbooru' AND key=?", [&tag], |r| r.get(0))
        .optional()?;
    if owner.as_deref().is_some_and(|o| o != ident) {
        return invalid("此 tag 已链接到其他身份，不能自动合并");
    }
    c.execute("UPDATE links SET current=0 WHERE entity_id=? AND system='danbooru'", [ident])?;
    c.execute(
        "INSERT INTO links(system,key,entity_id,current,source) VALUES ('danbooru',?,?,1,'user')
         ON CONFLICT(system,key) DO UPDATE SET current=1,source='user'",
        [&tag, ident],
    )?;
    c.execute("UPDATE work_review SET status='confirmed',note='' WHERE entity_id=?", [ident])?;
    Ok(())
}

fn work_move(c: &Connection, ident: &str, parent: Option<&str>) -> Result<()> {
    require_work(c, ident)?;
    if let Some(p) = parent {
        require_work(c, p)?;
        if descendants(c, ident)?.iter().any(|d| d == p) {
            return invalid("作品不能挂到自身或自己的子作品下面");
        }
    }
    c.execute("UPDATE entities SET parent_id=? WHERE id=?", params![parent, ident])?;
    c.execute("UPDATE work_review SET parent_locked=1 WHERE entity_id=?", [ident])?;
    Ok(())
}

/// `Store.edit_work`。
fn edit_work(c: &Connection, args: &Value) -> Result<Value> {
    let action = args["action"].as_str().unwrap_or("");
    let parent = args["parent"].as_str().filter(|p| !p.is_empty());
    let mut ids: Vec<String> = Vec::new();
    let ident = match action {
        "create" => work_create(c, args)?,
        "edit" => {
            let ident = text(&args["ident"], "作品不存在")?.to_owned();
            require_work(c, &ident)?;
            let name = args["name"].as_str().unwrap_or("");
            if name.trim().is_empty() || name.chars().count() > 120 {
                return invalid("请输入作品名");
            }
            work_move(c, &ident, parent)?;
            match args.get("tag") {
                Some(Value::String(tag)) if !tag.is_empty() => work_link(c, &ident, tag)?,
                Some(Value::String(_)) => {
                    c.execute("UPDATE links SET current=0 WHERE entity_id=? AND system='danbooru'", [&ident])?;
                    c.execute(
                        "UPDATE work_review SET status='confirmed',note='外部链接已由用户清空' WHERE entity_id=?",
                        [&ident],
                    )?;
                }
                _ => {}
            }
            character_names::put_name(
                c,
                &ident,
                text(&args["lang"], "缺少名字的语言")?,
                "user",
                name,
                "display",
                0,
                1,
            )?;
            ident
        }
        "from_tag" => {
            // 在作品框里选中一个 Danbooru 版权：已有的作品，否则新建一个链接好的作品（名字是调用方给的可读名）。
            let tag = text(&args["tag"], "请选择 Danbooru 角色 tag")?;
            match work_for(c, "x", tag, args["readable"].as_str().unwrap_or(""), &args["id"], &mut ids)? {
                Some(w) => w,
                None => return invalid("作品不存在"),
            }
        }
        "move" => {
            let ident = text(&args["ident"], "作品不存在")?.to_owned();
            work_move(c, &ident, parent)?;
            ident
        }
        "link" => {
            let ident = text(&args["ident"], "作品不存在")?.to_owned();
            work_link(c, &ident, args["tag"].as_str().unwrap_or(""))?;
            ident
        }
        "rename" => {
            let ident = text(&args["ident"], "作品不存在")?.to_owned();
            require_work(c, &ident)?;
            let name = args["name"].as_str().unwrap_or("");
            if name.trim().is_empty() || name.chars().count() > 120 {
                return invalid("请输入作品名");
            }
            character_names::put_name(
                c,
                &ident,
                text(&args["lang"], "缺少名字的语言")?,
                "user",
                name,
                "display",
                0,
                1,
            )?;
            ident
        }
        _ => return invalid("未知作品操作"),
    };
    ids.push(ident.clone());
    legacy_characters::mirror_state(c, &ids, "user")?;
    Ok(json!({"id": ident}))
}

/// `works.apply_evidence`（原 tools/reconcile_works.py --apply，#186 删除）。readable：新建作品的可读名（tag → 名字）；
/// ids：新建作品用的 id（双跑对齐用，可省）。
fn work_evidence(c: &Connection, args: &Value) -> Result<Value> {
    let evidence = &args["evidence"];
    let mut tags: BTreeSet<String> = BTreeSet::new();
    for t in evidence["tags"].as_array().into_iter().flatten() {
        if t.get("category").and_then(Value::as_i64) == Some(3) && !t.get("is_deprecated").map(truthy).unwrap_or(false)
        {
            if let Some(name) = t["name"].as_str() {
                tags.insert(name.to_owned());
            }
        }
    }
    let mut ids: BTreeMap<String, String> = c
        .prepare(
            "SELECT l.key,l.entity_id FROM links l JOIN entities e ON e.id=l.entity_id WHERE l.system='danbooru' AND l.current=1 AND e.kind='work'",
        )?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    // dict(...) 的顺序就是查询的行序：遍历 qualifier 保持原查询的行序。
    let qualifiers: Vec<(String, String)> = {
        let mut seen: Vec<(String, String)> = Vec::new();
        for row in c
            .prepare("SELECT key,entity_id FROM links WHERE system='qualifier'")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        {
            let (k, v): (String, String) = row?;
            match seen.iter_mut().find(|(key, _)| *key == k) {
                Some(entry) => entry.1 = v,
                None => seen.push((k, v)),
            }
        }
        seen
    };
    let (mut linked, mut pending, mut edges) = (Vec::new(), Vec::new(), Vec::new());
    let mut touched: Vec<String> = Vec::new();
    for (key, ident) in &qualifiers {
        let status: Option<String> =
            c.query_row("SELECT status FROM work_review WHERE entity_id=?", [ident], |r| r.get(0)).optional()?;
        if status.as_deref() == Some("confirmed") {
            continue;
        }
        let owner: Option<String> = c
            .query_row("SELECT entity_id FROM links WHERE system='danbooru' AND key=?", [key], |r| r.get(0))
            .optional()?;
        if !tags.contains(key) || owner.as_ref().is_some_and(|o| o != ident) {
            let note = if !tags.contains(key) { "需人工核对作品 tag" } else { "链接已属于另一个身份" };
            c.execute("UPDATE work_review SET note=? WHERE entity_id=?", [note, ident])?;
            pending.push(json!({"id": ident, "qualifier": key, "reason": note}));
            touched.push(ident.clone());
            continue;
        }
        c.execute(
            "INSERT OR IGNORE INTO links(system,key,entity_id,source) VALUES ('danbooru',?,?,'check')",
            [key, ident],
        )?;
        c.execute("UPDATE work_review SET status='verified',note='' WHERE entity_id=?", [ident])?;
        ids.insert(key.clone(), ident.clone());
        linked.push(key.clone());
        touched.push(ident.clone());
    }
    let mut parents: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for row in evidence["implications"].as_array().into_iter().flatten() {
        let (Some(a), Some(b)) = (row["antecedent_name"].as_str(), row["consequent_name"].as_str()) else { continue };
        if row.get("status").and_then(Value::as_str) == Some("active") && tags.contains(a) && tags.contains(b) {
            parents.entry(a.to_owned()).or_default().insert(b.to_owned());
        }
    }
    let mut needed: BTreeSet<String> = evidence["watch_tags"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|t| tags.contains(*t))
        .map(str::to_owned)
        .collect();
    let mut frontier: BTreeSet<String> = ids.keys().cloned().chain(needed.iter().cloned()).collect();
    while !frontier.is_empty() {
        let added: BTreeSet<String> = frontier
            .iter()
            .flat_map(|child| parents.get(child).into_iter().flatten().cloned())
            .filter(|p| !needed.contains(p) && !ids.contains_key(p))
            .collect();
        needed.extend(added.iter().cloned());
        frontier = added;
    }
    let new_ids = &args["ids"];
    let missing: Vec<String> = needed.iter().filter(|t| !ids.contains_key(*t)).cloned().collect();
    for tag in missing {
        if c.query_row("SELECT 1 FROM links WHERE system='danbooru' AND key=?", [&tag], |_| Ok(()))
            .optional()?
            .is_some()
        {
            continue;
        }
        // 调用方给了 ids（对象）时，缺的 id 向它要（Error::NeedId，指针 /ids/<tag>，按 JSON Pointer 转义）。
        let given = match new_ids.get(&tag) {
            Some(v) => v.clone(),
            None if new_ids.is_object() => Value::Bool(false),
            None => Value::Null,
        };
        let ident = new_id(&given).map_err(|e| match e {
            Error::NeedId(_) => Error::NeedId(format!("/ids/{}", tag.replace('~', "~0").replace('/', "~1"))),
            e => e,
        })?;
        c.execute("INSERT INTO entities(id,name,kind) VALUES (?,'','work')", [&ident])?;
        let readable =
            args["readable"][&tag].as_str().ok_or_else(|| Error::Invalid(format!("缺少作品 {tag} 的可读名")))?;
        character_names::put_name(c, &ident, "en", "index", readable, "display", 0, 0)?;
        c.execute("INSERT INTO links(system,key,entity_id,source) VALUES ('danbooru',?,?,'check')", [&tag, &ident])?;
        c.execute("INSERT INTO work_review(entity_id,status) VALUES (?,'verified')", [&ident])?;
        ids.insert(tag.clone(), ident.clone());
        touched.push(ident);
    }
    for (tag, choices) in &parents {
        let Some(ident) = ids.get(tag).cloned() else { continue };
        let lock: Option<i64> = c
            .query_row("SELECT parent_locked FROM work_review WHERE entity_id=?", [&ident], |r| r.get(0))
            .optional()?;
        if lock.unwrap_or(0) != 0 {
            continue;
        }
        let mut note = None;
        if choices.len() != 1 {
            note =
                Some(format!("多个作品上级，需人工选择：{}", choices.iter().cloned().collect::<Vec<_>>().join(", ")));
        } else {
            let parent_tag = choices.iter().next().expect("一个");
            let Some(parent) = ids.get(parent_tag).cloned() else { continue };
            if descendants(c, &ident)?.contains(&parent) {
                note = Some("蕴含关系成环，需人工确认".to_owned());
            } else {
                c.execute("UPDATE entities SET parent_id=? WHERE id=?", [&parent, &ident])?;
                edges.push(json!([tag, parent_tag]));
            }
        }
        if let Some(note) = note {
            c.execute("UPDATE work_review SET note=? WHERE entity_id=?", [&note, &ident])?;
            pending.push(json!({"id": ident, "tag": tag, "reason": note}));
        }
        touched.push(ident);
    }
    legacy_characters::mirror_state(c, &touched, "user")?;
    Ok(json!({"exact_links": linked.len(), "tree_edges": edges, "pending": pending}))
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

pub fn command(c: &Connection, op: &str, args: &Value) -> Result<Option<Value>> {
    Ok(Some(match op {
        "create_entity" => create_entity(c, args)?,
        "create_linked" => create_linked(c, args)?,
        "shared_oc" => shared_oc(c, args)?,
        "set_kind" => set_kind(c, args)?,
        "set_mascot" => set_mascot(c, args)?,
        "set_work" => set_character_work(c, args)?,
        // 新建作品要调用方给 id 时（`"id": false`），指针是 /id。
        "edit_work" => edit_work(c, args).map_err(|e| match e {
            Error::NeedId(p) if p.is_empty() => Error::NeedId("/id".into()),
            e => e,
        })?,
        "work_evidence" => work_evidence(c, args)?,
        "set_profile" => crate::character_profile::set_profile(c, args)?,
        "link_local" => crate::character_profile::link_local(c, args)?,
        _ => return Ok(None),
    }))
}
