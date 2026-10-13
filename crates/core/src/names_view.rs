//! 名字的显示投影（docs/TERMS_NAMES_PLAN.md N1）：逐条移植 tests/reference/web/names.py（#9 已删除） 的 `View`（`entity_rows`、`load`、`choose`、
//! `entity`、`base`、`own_name`）和词表（`PRIORITY`、`SKINS`、`WORK_NAMES`、`readable`、`skin_zh`）。
//! 输入是名字的兼容表（`entities`、当前 `links`、`names`、`display_names`），输出两张投影表：
//! - `term_name_display(entity, skin_only, …)`：`View.entity(id, skin_only)`；
//! - `term_name_choice(kind, key, …)`：`View.choose(kind, key)`，键的范围是人物的 tag、作品文字（`entities.work` 与
//!   qualifier 链接）、皮肤 variant。
//!
//! 结果曾与旧代码全量逐字对照；现由 native/terms/tests/replay.rs 的 test_terms_names_projection 重放钉住（#9）。
use crate::{Error, Result};
use rusqlite::{params, Connection};
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// `names.PRIORITY`：小的优先。
pub const PRIORITY: [&str; 7] = ["user", "bangumi", "index", "danbooru", "wiki", "builtin", "model"];

/// `names.SKINS`，保持原来的顺序（`skin_zh` 按长度排序时，同长的词按这个顺序）。
pub const SKINS: [(&str, &str); 15] = [
    ("swimsuit", "泳装"),
    ("new_year", "新年"),
    ("bunny", "兔女郎"),
    ("school_uniform", "校服"),
    ("first_ascension", "第一再临"),
    ("second_ascension", "第二再临"),
    ("third_ascension", "第三再临"),
    ("casual", "便服"),
    ("dress", "礼服"),
    ("idol", "偶像"),
    ("maid", "女仆"),
    ("kimono", "和服"),
    ("halloween", "万圣节"),
    ("christmas", "圣诞"),
    ("festival_outfit", "祭典服"),
];

/// `names.WORK_NAMES`。
pub const WORK_NAMES: [(&str, &str); 22] = [
    ("touhou", "东方Project"),
    ("hololive", "hololive"),
    ("nijisanji", "彩虹社"),
    ("vocaloid", "VOCALOID"),
    ("fate", "Fate"),
    ("idolmaster", "偶像大师"),
    ("kancolle", "舰队Collection"),
    ("azur_lane", "碧蓝航线"),
    ("girls'_frontline", "少女前线"),
    ("pokemon", "宝可梦"),
    ("genshin_impact", "原神"),
    ("arknights", "明日方舟"),
    ("blue_archive", "蔚蓝档案"),
    ("honkai:_star_rail", "崩坏：星穹铁道"),
    ("zenless_zone_zero", "绝区零"),
    ("wuthering_waves", "鸣潮"),
    ("honkai_impact_3rd", "崩坏3"),
    ("umamusume", "赛马娘"),
    ("love_live!", "LoveLive!"),
    ("bang_dream!", "BanG Dream!"),
    ("project_sekai", "世界计划"),
    ("princess_connect!", "公主连结"),
];

fn priority(source: &str) -> Result<usize> {
    PRIORITY
        .iter()
        .position(|p| *p == source)
        .ok_or_else(|| Error::Invalid(format!("名字来源不在优先级表里：{source}")))
}

pub fn work_name(key: &str) -> Option<&'static str> {
    WORK_NAMES.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

/// Python 的 `str.title()`：前一个字符不是有大小写的字母时，本字符转成首字母大写，否则转小写。
fn title(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut previous_cased = false;
    for ch in s.chars() {
        if ch.is_uppercase() {
            if previous_cased {
                out.extend(ch.to_lowercase());
            } else {
                out.push(ch);
            }
            previous_cased = true;
        } else if ch.is_lowercase() {
            if previous_cased {
                out.push(ch);
            } else {
                out.extend(ch.to_uppercase());
            }
            previous_cased = true;
        } else {
            out.push(ch);
            previous_cased = false;
        }
    }
    out
}

/// `names.readable`：人物的 tag 先去掉结尾的 `_(qualifier)`。
pub fn readable(tag: &str, character: bool) -> String {
    let mut t = tag;
    if character && t.ends_with(')') {
        // re.sub(r'_\([^()]+\)$', '', tag)
        if let Some(open) = t.rfind("_(") {
            let inner = &t[open + 2..t.len() - 1];
            if !inner.is_empty() && !inner.contains(['(', ')']) {
                t = &t[..open];
            }
        }
    }
    title(&t.replace('_', " "))
}

/// `names.skin_zh`：最长的词先匹配，拆不完就不翻译。
pub fn skin_zh(tag: &str) -> Option<String> {
    if tag.is_empty() {
        return None;
    }
    let mut words: Vec<(&str, &str)> = SKINS.to_vec();
    words.sort_by_key(|(w, _)| std::cmp::Reverse(w.chars().count())); // 稳定排序：同长保持原顺序
    for (word, zh) in words {
        if tag == word {
            return Some(zh.to_owned());
        }
        if let Some(rest) = tag.strip_prefix(word).and_then(|r| r.strip_prefix('_')) {
            if let Some(rest) = skin_zh(rest) {
                return Some(format!("{zh} · {rest}"));
            }
        }
    }
    None
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Choice {
    pub name: String,
    pub source: String,
    pub names: [Option<String>; 3], // zh, ja, en
}

const LANGS: [&str; 3] = ["zh", "ja", "en"];

#[derive(Clone, Debug, PartialEq, Eq)]
struct NameRow {
    lang: String,
    value: String,
    source: String,
    preferred: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Entity {
    id: String,
    kind: String,
    parent: Option<String>,
    variant: String,
    merged: Option<String>,
    original: bool,
    keys: BTreeMap<String, String>,
    linked: bool,
    tag: String,
    /// 在 `entities` 里的 rowid 顺序（人物键下的名字按这个顺序拼接）。u32 落在已有的对齐空隙里，不增大快照。
    seq: u32,
}

/// `View` 的一个快照。
pub struct View {
    entities: BTreeMap<String, Entity>,
    merged: HashMap<String, Vec<String>>,
    names: HashMap<(String, String), Vec<NameRow>>,
    own: HashMap<String, Vec<NameRow>>,
    /// `entities.work`，按 `seq` 排；只有 `load_with_work` 载入，否则为空。
    works: Vec<String>,
    with_work: bool,
}

/// 一个身份在快照里的样子（`/api/names` 的统计用，免得再读一遍 `entities` 和 `links`）。
pub struct Identity<'a> {
    pub id: &'a str,
    pub kind: &'a str,
    pub parent: Option<&'a str>,
    pub merged_into: Option<&'a str>,
    pub original: bool,
    /// 有当前链接。
    pub linked: bool,
    pub tag: &'a str,
    pub work: &'a str,
    pub variant: &'a str,
}

/// 快照里一个条目被替换之前的样子（None = 原来没有）。
type Replaced<K> = Vec<(K, Option<Vec<NameRow>>)>;

/// `View::absorb` 之前被替换掉的快照条目，`View::restore` 用它撤回。
pub struct Absorbed {
    own: Replaced<String>,
    names: Replaced<(String, String)>,
}

pub fn linked_tag(keys: &BTreeMap<String, String>) -> Result<String> {
    for system in ["danbooru", "wd", "photos_album", "ccip"] {
        if let Some(k) = keys.get(system).filter(|k| !k.is_empty()) {
            return Ok(k.clone());
        }
    }
    Ok(match keys.get("ccip_base") {
        Some(base) => serde_json::from_str::<serde_json::Value>(base)
            .map_err(|e| Error::Invalid(format!("ccip_base 链接不是 JSON：{e}")))?
            .get(0)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned(),
        None => String::new(),
    })
}

impl View {
    pub fn load(c: &Connection) -> Result<View> {
        View::load_inner(c, false)
    }

    /// 同 `load`，另带上 `entities.work`（`identities` 里的 `work`）。
    pub fn load_with_work(c: &Connection) -> Result<View> {
        View::load_inner(c, true)
    }

    fn load_inner(c: &Connection, with_work: bool) -> Result<View> {
        let mut entities: BTreeMap<String, Entity> = BTreeMap::new();
        let mut order: Vec<String> = Vec::new();
        let mut works: Vec<String> = Vec::new();
        let sql = if with_work {
            "SELECT id,kind,parent_id,variant,merged_into,coalesce(original,0),work FROM entities ORDER BY rowid"
        } else {
            "SELECT id,kind,parent_id,variant,merged_into,coalesce(original,0) FROM entities ORDER BY rowid"
        };
        for row in c.prepare(sql)?.query_map([], |r| {
            Ok((
                Entity {
                    id: r.get(0)?,
                    kind: r.get(1)?,
                    parent: r.get(2)?,
                    variant: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                    merged: r.get(4)?,
                    original: r.get::<_, i64>(5)? != 0,
                    keys: BTreeMap::new(),
                    linked: false,
                    tag: String::new(),
                    seq: 0,
                },
                if with_work { r.get::<_, Option<String>>(6)?.unwrap_or_default() } else { String::new() },
            ))
        })? {
            let (mut e, work) = row?;
            e.seq = u32::try_from(order.len()).map_err(|_| Error::Invalid("身份太多".into()))?;
            if with_work {
                works.push(work);
            }
            order.push(e.id.clone());
            entities.insert(e.id.clone(), e);
        }
        for row in c
            .prepare("SELECT system,key,entity_id FROM links WHERE current=1 ORDER BY added_at,key")?
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?
        {
            let (system, key, ident) = row?;
            if let Some(e) = entities.get_mut(&ident) {
                e.keys.entry(system).or_insert(key);
            }
        }
        for e in entities.values_mut() {
            e.linked = !e.keys.is_empty();
            e.tag = linked_tag(&e.keys)?;
        }
        let mut merged: HashMap<String, Vec<String>> = HashMap::new();
        for e in entities.values() {
            // sorted by id (BTreeMap)
            if let Some(m) = e.merged.as_ref().filter(|m| !m.is_empty()) {
                if e.linked {
                    merged.entry(m.clone()).or_default().push(e.id.clone());
                }
            }
        }
        let mut names: HashMap<(String, String), Vec<NameRow>> = HashMap::new();
        let has_display: bool =
            c.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='display_names')", [], |r| r.get(0))?;
        if has_display {
            for row in c
                .prepare("SELECT kind,key,lang,source,value FROM display_names WHERE source<>'danbooru'")?
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        NameRow { lang: r.get(2)?, source: r.get(3)?, value: r.get(4)?, preferred: false },
                    ))
                })?
            {
                let (kind, key, n) = row?;
                names.entry((kind, key)).or_default().push(n);
            }
        }
        let mut own: HashMap<String, Vec<NameRow>> = HashMap::new();
        for row in c
            .prepare(
                "SELECT entity_id,lang,value,source,preferred FROM names WHERE role='display' ORDER BY preferred DESC,value",
            )?
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    NameRow { lang: r.get(1)?, value: r.get(2)?, source: r.get(3)?, preferred: r.get::<_, i64>(4)? != 0 },
                ))
            })?
        {
            let (ident, n) = row?;
            own.entry(ident).or_default().push(n);
        }
        for ident in &order {
            let e = &entities[ident];
            if e.kind != "work" && e.linked && e.parent.is_none() {
                if let Some(rows) = own.get(ident) {
                    names.entry(("character".into(), e.tag.clone())).or_default().extend(rows.iter().cloned());
                }
            }
        }
        Ok(View { entities, merged, names, own, works, with_work })
    }

    pub fn ids(&self) -> impl Iterator<Item = &String> {
        self.entities.keys()
    }

    /// 全部身份，按 id 排序。
    pub fn identities(&self) -> impl Iterator<Item = Identity<'_>> {
        self.entities.values().map(|e| Identity {
            id: &e.id,
            kind: &e.kind,
            parent: e.parent.as_deref(),
            merged_into: e.merged.as_deref(),
            original: e.original,
            linked: e.linked,
            tag: &e.tag,
            work: self.works.get(e.seq as usize).map(String::as_str).unwrap_or(""),
            variant: &e.variant,
        })
    }

    /// 名字更新器在一批任务里复用同一个整库快照（#102 A5a）：快照恰好对应本事务开始时的库（调用方用
    /// `names_version` 保证）、本事务只改了名字行时，按 `legacy_names::track` 记下的身份和键把这些行重新读进快照，
    /// 结果与整库重新载入逐项相同（行的先后也相同：`load` 的排序在 SQLite 里是稳定的，并列时按 rowid）。本事务改了
    /// 身份或链接、或者没有追踪时返回 None，调用方应丢弃快照、改为整库载入。返回值交给 `restore` 可以撤回。
    pub fn absorb(&mut self, c: &Connection) -> Result<Option<Absorbed>> {
        let tracked: bool =
            c.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_temp_master WHERE name='names_dirty_full')", [], |r| {
                r.get(0)
            })?;
        if !tracked
            || c.query_row("SELECT EXISTS(SELECT 1 FROM names_dirty_full WHERE value=1)", [], |r| r.get::<_, bool>(0))?
        {
            return Ok(None);
        }
        // 先把要换的行全部读出来，读完再改快照：读的过程中出错时快照保持原样。
        let ids: Vec<String> = c
            .prepare("SELECT id FROM names_dirty_entities ORDER BY id")?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        let mut keys: BTreeSet<(String, String)> = c
            .prepare("SELECT kind,key FROM names_dirty_keys")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let mut own = c.prepare_cached(
            "SELECT lang,value,source,preferred FROM names WHERE entity_id=? AND role='display'
             ORDER BY preferred DESC,value,rowid",
        )?;
        let mut owned: HashMap<String, Vec<NameRow>> = HashMap::new();
        for id in &ids {
            let rows: Vec<NameRow> = own
                .query_map([id], |r| {
                    Ok(NameRow {
                        lang: r.get(0)?,
                        value: r.get(1)?,
                        source: r.get(2)?,
                        preferred: r.get::<_, i64>(3)? != 0,
                    })
                })?
                .collect::<rusqlite::Result<_>>()?;
            owned.insert(id.clone(), rows);
            if let Some(e) = self.entities.get(id).filter(|e| Self::spoken(e)) {
                keys.insert(("character".into(), e.tag.clone()));
            }
        }
        let tags: BTreeSet<&str> = keys.iter().filter(|(k, _)| k == "character").map(|(_, t)| t.as_str()).collect();
        let mut members: HashMap<&str, Vec<(u32, &str)>> = HashMap::new();
        if !tags.is_empty() {
            for e in self.entities.values().filter(|e| Self::spoken(e) && tags.contains(e.tag.as_str())) {
                members.entry(e.tag.as_str()).or_default().push((e.seq, e.id.as_str()));
            }
        }
        let mut display = c.prepare_cached(
            "SELECT lang,source,value FROM display_names WHERE kind=? AND key=? AND source<>'danbooru' ORDER BY rowid",
        )?;
        let mut named: Vec<((String, String), Vec<NameRow>)> = Vec::with_capacity(keys.len());
        for (kind, key) in keys {
            let mut rows: Vec<NameRow> = display
                .query_map([&kind, &key], |r| {
                    Ok(NameRow { lang: r.get(0)?, source: r.get(1)?, value: r.get(2)?, preferred: false })
                })?
                .collect::<rusqlite::Result<_>>()?;
            if kind == "character" {
                if let Some(m) = members.get_mut(key.as_str()) {
                    m.sort();
                    for (_, id) in m.iter() {
                        if let Some(r) = owned.get(*id).or_else(|| self.own.get(*id)) {
                            rows.extend(r.iter().cloned());
                        }
                    }
                }
            }
            named.push(((kind, key), rows));
        }
        let mut undo = Absorbed { own: Vec::with_capacity(owned.len()), names: Vec::with_capacity(named.len()) };
        for (id, rows) in owned {
            let old = if rows.is_empty() { self.own.remove(&id) } else { self.own.insert(id.clone(), rows) };
            undo.own.push((id, old));
        }
        for (k, rows) in named {
            let old = if rows.is_empty() { self.names.remove(&k) } else { self.names.insert(k.clone(), rows) };
            undo.names.push((k, old));
        }
        Ok(Some(undo))
    }

    /// 撤回一次 `absorb`（模型预演在回滚的事务里读待提交的名字，读完把快照还原）。
    pub fn restore(&mut self, undo: Absorbed) {
        for (k, old) in undo.names.into_iter().rev() {
            match old {
                Some(v) => self.names.insert(k, v),
                None => self.names.remove(&k),
            };
        }
        for (id, old) in undo.own.into_iter().rev() {
            match old {
                Some(v) => self.own.insert(id, v),
                None => self.own.remove(&id),
            };
        }
    }

    /// 对照核对（`legacy_names::verifying`）：吸收增量后的快照与同一事务里整库重新载入的结果逐项相同。
    pub fn verify_absorbed(&self, c: &Connection) -> Result<()> {
        let full = View::load_inner(c, self.with_work)?;
        if self.entities != full.entities
            || self.merged != full.merged
            || self.names != full.names
            || self.own != full.own
            || self.works != full.works
        {
            return Err(Error::Conflict("增量名字快照与整库重新载入不一致".into()));
        }
        crate::legacy_names::record_verified("view")
    }

    /// 自己的显示名会并进人物键（`load` 里同样的条件）。
    fn spoken(e: &Entity) -> bool {
        e.kind != "work" && e.linked && e.parent.is_none()
    }

    /// `View.choose`（已装载快照的那条路径）。
    pub fn choose(&self, kind: &str, key: &str) -> Result<Choice> {
        let mut rows: Vec<&NameRow> =
            self.names.get(&(kind.to_owned(), key.to_owned())).map(|v| v.iter().collect()).unwrap_or_default();
        let mut keyed = Vec::with_capacity(rows.len());
        for r in rows.drain(..) {
            keyed.push(((!r.preferred, priority(&r.source)?, r.value.clone()), r));
        }
        keyed.sort_by(|a, b| a.0.cmp(&b.0));
        let mut best: BTreeMap<String, (String, String)> = BTreeMap::new();
        for (_, r) in keyed {
            best.entry(r.lang.clone()).or_insert((r.value.clone(), r.source.clone()));
        }
        if kind == "skin" {
            if let Some(zh) = skin_zh(key) {
                let replace = match best.get("zh") {
                    None => true,
                    Some(old) => priority(&old.1)? > priority("builtin")?,
                };
                if replace {
                    best.insert("zh".into(), (zh, "builtin".into()));
                }
            }
        }
        if kind == "work" {
            if let Some(w) = work_name(key) {
                if best.get("zh").map(|v| v.1.as_str()) != Some("user") {
                    best.insert("zh".into(), (w.to_owned(), "builtin".into()));
                }
            }
        }
        let langs: &[&str] = if kind == "skin" { &["zh", "en"] } else { &LANGS };
        // min 取第一个最小的；wiki 的中文按 bangumi 排。
        let mut chosen: Option<(usize, (String, String))> = None;
        for lang in langs.iter().filter(|l| **l != "en") {
            if let Some(v) = best.get(*lang) {
                let rank = if v.1 == "wiki" && *lang == "zh" { priority("bangumi")? } else { priority(&v.1)? };
                if chosen.as_ref().is_none_or(|(r, _)| rank < *r) {
                    chosen = Some((rank, v.clone()));
                }
            }
        }
        let mut chosen =
            chosen.map(|(_, v)| v).unwrap_or_else(|| (readable(key, kind == "character"), "readable".into()));
        if kind == "work" {
            if let Some(w) = work_name(key) {
                if chosen.1 != "user" {
                    chosen = (w.to_owned(), "builtin".into());
                }
            }
        }
        Ok(Choice { name: chosen.0, source: chosen.1, names: LANGS.map(|l| best.get(l).map(|v| v.0.clone())) })
    }

    fn own_name(&self, e: &Entity) -> Choice {
        let rows = self.own.get(&e.id).map(Vec::as_slice).unwrap_or(&[]);
        let chosen = rows.iter().find(|r| r.source == "user").or(rows.first());
        Choice {
            name: chosen.map(|r| r.value.clone()).unwrap_or_else(|| "未命名角色".into()),
            source: chosen.map(|r| r.source.clone()).unwrap_or_else(|| "readable".into()),
            names: [None, None, None],
        }
    }

    fn base(&self, e: &Entity) -> Result<Choice> {
        let own_user = self.own.get(&e.id).is_some_and(|rows| rows.iter().any(|r| r.source == "user"));
        if !e.linked || e.original || e.kind == "oc" || own_user {
            return Ok(self.own_name(e));
        }
        let result = self.choose("character", &e.tag)?;
        if result.source == "readable" {
            for m in self.merged.get(&e.id).map(Vec::as_slice).unwrap_or(&[]) {
                let named = self.choose("character", &self.entities[m].tag)?;
                if named.source != "readable" {
                    return Ok(named);
                }
            }
        }
        Ok(result)
    }

    /// `View.entity`：返回（选名结果，tag）；身份不存在时 None。
    pub fn entity(&self, ident: &str, skin_only: bool) -> Result<Option<(Choice, String)>> {
        let Some(e) = self.entities.get(ident) else { return Ok(None) };
        if e.kind == "work" {
            let rows = self.own.get(ident).map(Vec::as_slice).unwrap_or(&[]);
            let user = rows.iter().find(|r| r.source == "user");
            let key = e.keys.get("qualifier").cloned().unwrap_or_default();
            let result = if let Some(u) = user {
                Choice {
                    name: u.value.clone(),
                    source: "user".into(),
                    names: LANGS.map(|l| rows.iter().find(|r| r.lang == l).map(|r| r.value.clone())),
                }
            } else if !key.is_empty() {
                self.choose("work", &key)?
            } else {
                self.own_name(e)
            };
            return Ok(Some((result, e.keys.get("danbooru").cloned().unwrap_or_default())));
        }
        let local = !e.linked || e.original || e.kind == "oc";
        let mut result;
        if local {
            result = self.own_name(e);
        } else if let Some(parent_id) = &e.parent {
            result = self.choose("skin", &e.variant)?;
            let parent = self.entities.get(parent_id);
            if result.source == "readable" {
                let mut rows: Vec<&NameRow> = self
                    .own
                    .get(&e.id)
                    .map(|v| v.iter().filter(|r| r.lang == "zh" || r.lang == "ja").collect())
                    .unwrap_or_default();
                let mut keyed = Vec::with_capacity(rows.len());
                for r in rows.drain(..) {
                    keyed.push(((r.lang != "zh", !r.preferred, priority(&r.source)?, r.value.clone()), r));
                }
                keyed.sort_by(|a, b| a.0.cmp(&b.0));
                let rows: Vec<&NameRow> = keyed.into_iter().map(|(_, r)| r).collect();
                if let (Some(first), Some(parent)) = (rows.first(), parent) {
                    if first.value != self.base(parent)?.name {
                        result = Choice {
                            name: first.value.clone(),
                            source: first.source.clone(),
                            names: LANGS.map(|l| rows.iter().find(|r| r.lang == l).map(|r| r.value.clone())),
                        };
                    }
                }
            }
            if !skin_only {
                let (base_name, base_source) = match parent {
                    Some(p) => {
                        let b = self.base(p)?;
                        (b.name, Some(b.source))
                    }
                    None => (String::new(), None),
                };
                result.name = format!("{base_name} · {}", result.name);
                if base_source.as_deref() == Some("model") {
                    result.source = "model".into();
                }
            }
        } else {
            result = self.base(e)?;
        }
        let tag = if skin_only {
            e.variant.clone()
        } else if !e.linked {
            String::new()
        } else {
            e.tag.clone()
        };
        Ok(Some((result, tag)))
    }

    /// 投影的文字键范围：人物（非皮肤、非作品）的 tag、作品文字（`entities.work` 与 qualifier 链接）、皮肤 variant。
    pub fn choice_keys(&self, c: &Connection) -> Result<BTreeSet<(String, String)>> {
        let mut keys = BTreeSet::new();
        for e in self.entities.values() {
            if !e.tag.is_empty() {
                keys.insert(("character".to_owned(), e.tag.clone()));
            }
            if !e.variant.is_empty() {
                keys.insert(("skin".to_owned(), e.variant.clone()));
            }
        }
        for row in c
            .prepare("SELECT work FROM entities WHERE work<>'' UNION SELECT key FROM links WHERE system='qualifier'")?
            .query_map([], |r| r.get::<_, String>(0))?
        {
            keys.insert(("work".to_owned(), row?));
        }
        Ok(keys)
    }
}

pub const TABLES: &str = "
CREATE TABLE IF NOT EXISTS term_name_display(
 entity TEXT NOT NULL, skin_only INTEGER NOT NULL CHECK(skin_only IN (0,1)),
 name TEXT NOT NULL, name_source TEXT NOT NULL, zh TEXT, ja TEXT, en TEXT, tag TEXT NOT NULL,
 PRIMARY KEY(entity, skin_only)) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS term_name_choice(
 kind TEXT NOT NULL, key TEXT NOT NULL,
 name TEXT NOT NULL, name_source TEXT NOT NULL, zh TEXT, ja TEXT, en TEXT,
 PRIMARY KEY(kind, key)) WITHOUT ROWID;
";

/// 整表重建两张投影表（影子阶段：只读名字的兼容表，写核心自己的两张表）。
pub fn rebuild(c: &Connection) -> Result<(usize, usize)> {
    rebuild_from(c, &View::load(c)?)
}

/// 同 `rebuild`，用调用方给的快照（必须与这个事务此刻的库逐项相同，见 `View::absorb`）。
pub fn rebuild_from(c: &Connection, view: &View) -> Result<(usize, usize)> {
    c.execute_batch("DELETE FROM term_name_display; DELETE FROM term_name_choice;")?;
    let mut put = c.prepare(
        "INSERT INTO term_name_display(entity,skin_only,name,name_source,zh,ja,en,tag) VALUES (?,?,?,?,?,?,?,?)",
    )?;
    let mut shown = 0;
    let ids: Vec<String> = view.ids().cloned().collect();
    for id in &ids {
        for skin_only in [false, true] {
            if let Some((ch, tag)) = view.entity(id, skin_only)? {
                let [zh, ja, en] = &ch.names;
                put.execute(params![id, skin_only as i64, ch.name, ch.source, zh, ja, en, tag])?;
                shown += 1;
            }
        }
    }
    let mut put =
        c.prepare("INSERT INTO term_name_choice(kind,key,name,name_source,zh,ja,en) VALUES (?,?,?,?,?,?,?)")?;
    let keys = view.choice_keys(c)?;
    for (kind, key) in &keys {
        let ch = view.choose(kind, key)?;
        let [zh, ja, en] = &ch.names;
        put.execute(params![kind, key, ch.name, ch.source, zh, ja, en])?;
    }
    Ok((shown, keys.len()))
}

/// 名字行变化时只写依赖闭包；实体与链接结构变更由调用方回退整表。
pub fn rebuild_changed(c: &Connection) -> Result<(usize, usize)> {
    rebuild_changed_from(c, &View::load(c)?)
}

/// 同 `rebuild_changed`，用调用方给的快照（同 `rebuild_from`）。
pub fn rebuild_changed_from(c: &Connection, view: &View) -> Result<(usize, usize)> {
    let mut ids: BTreeSet<String> = c
        .prepare("SELECT id FROM names_dirty_entities")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut keys: BTreeSet<(String, String)> = c
        .prepare("SELECT kind,key FROM names_dirty_keys")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    loop {
        let before = (ids.len(), keys.len());
        for e in view.entities.values() {
            if ids.contains(&e.id) && e.kind != "work" && e.parent.is_none() && !e.tag.is_empty() {
                keys.insert(("character".into(), e.tag.clone()));
            }
            if keys.contains(&("character".into(), e.tag.clone()))
                || keys.contains(&("skin".into(), e.variant.clone()))
                || e.keys.get("qualifier").is_some_and(|k| keys.contains(&("work".into(), k.clone())))
                || e.parent.as_ref().is_some_and(|p| ids.contains(p))
                || view.merged.get(&e.id).is_some_and(|members| members.iter().any(|m| ids.contains(m)))
            {
                ids.insert(e.id.clone());
            }
        }
        if before == (ids.len(), keys.len()) {
            break;
        }
    }
    let mut put = c.prepare(
        "INSERT OR REPLACE INTO term_name_display(entity,skin_only,name,name_source,zh,ja,en,tag)
        VALUES (?,?,?,?,?,?,?,?)",
    )?;
    let mut shown = 0;
    for id in &ids {
        for skin in [false, true] {
            if let Some((ch, tag)) = view.entity(id, skin)? {
                let [zh, ja, en] = &ch.names;
                put.execute(params![id, skin as i64, ch.name, ch.source, zh, ja, en, tag])?;
                shown += 1;
            }
        }
    }
    let existing = view.choice_keys(c)?;
    let mut put = c.prepare(
        "INSERT OR REPLACE INTO term_name_choice(kind,key,name,name_source,zh,ja,en) VALUES (?,?,?,?,?,?,?)",
    )?;
    let mut chosen = 0;
    for (kind, key) in keys.intersection(&existing) {
        let ch = view.choose(kind, key)?;
        let [zh, ja, en] = &ch.names;
        put.execute(params![kind, key, ch.name, ch.source, zh, ja, en])?;
        chosen += 1;
    }
    Ok((shown, chosen))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Name inputs only, names already in the core so `legacy_names::track` records changes.
    fn library() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE meta(key TEXT PRIMARY KEY,value TEXT NOT NULL);
             INSERT INTO meta VALUES ('terms_names','core'),('names_version','0'),('names_projected','0');
             CREATE TABLE entities(id TEXT PRIMARY KEY,kind TEXT NOT NULL,parent_id TEXT,variant TEXT NOT NULL DEFAULT '',
              merged_into TEXT,original INTEGER,work TEXT NOT NULL DEFAULT '');
             CREATE TABLE links(system TEXT,key TEXT,entity_id TEXT,current INTEGER NOT NULL DEFAULT 1,added_at TEXT,
              PRIMARY KEY(system,key));
             CREATE TABLE names(entity_id TEXT,lang TEXT,value TEXT,source TEXT,role TEXT,preferred INTEGER NOT NULL DEFAULT 0,
              position INTEGER NOT NULL DEFAULT 0,PRIMARY KEY(entity_id,role,lang,source,value));
             CREATE TABLE display_names(kind TEXT,key TEXT,lang TEXT,source TEXT,value TEXT,PRIMARY KEY(kind,key,lang,source,value));
             CREATE TABLE name_aliases(kind TEXT,key TEXT,value TEXT,position INTEGER,PRIMARY KEY(kind,key,value));
             INSERT INTO entities VALUES ('e1','character',NULL,'',NULL,0,'series_w'),('e2','character',NULL,'',NULL,0,''),
              ('s1','skin','e1','swimsuit_x',NULL,0,''),('w1','work',NULL,'',NULL,0,''),('m1','character',NULL,'','e1',0,'');
             INSERT INTO links VALUES ('danbooru','synth_(series_w)','e1',1,'2026-01-01'),('wd','synth_(series_w)','e2',1,'2026-01-02'),
              ('danbooru','synth_swimsuit','s1',1,'2026-01-03'),('qualifier','series_w','w1',1,'2026-01-04'),
              ('danbooru','merged_tag','m1',1,'2026-01-05');
             -- Rows tied on (preferred,value) whose rowid order differs from the primary key order.
             INSERT INTO names VALUES ('e1','zh','同名','index','display',0,0),('e1','ja','同名','wiki','display',0,0),
              ('e2','en','Other','wiki','display',0,0),('m1','zh','并入名','index','display',0,0);
             INSERT INTO display_names VALUES ('skin','swimsuit_x','zh','index','皮肤名'),('work','series_w','zh','bangumi','作品名');",
        )
        .unwrap();
        c
    }

    fn same(a: &View, b: &View) -> bool {
        a.entities == b.entities && a.merged == b.merged && a.names == b.names && a.own == b.own && a.works == b.works
    }

    #[test]
    fn absorbed_name_rows_read_as_a_fresh_load() {
        let mut c = library();
        let before = View::load(&c).unwrap();
        let mut view = View::load(&c).unwrap();
        let tx = c.transaction().unwrap();
        crate::legacy_names::track(&tx).unwrap();
        tx.execute_batch(
            "INSERT INTO names VALUES ('e2','zh','同名','bangumi','display',0,0),('e1','en','同名','user','display',0,0);
             UPDATE names SET preferred=1 WHERE entity_id='m1';
             DELETE FROM names WHERE entity_id='e2' AND lang='en';
             INSERT INTO display_names VALUES ('character','synth_(series_w)','ja','index','キャラ'),
              ('work','series_w','ja','wiki','サクヒン'),('skin','swimsuit_x','en','wiki','Swim');
             DELETE FROM display_names WHERE kind='skin' AND key='swimsuit_x' AND lang='zh';",
        )
        .unwrap();
        let undo = view.absorb(&tx).unwrap().expect("name rows only");
        view.verify_absorbed(&tx).unwrap();
        for id in ["e1", "e2", "s1", "w1", "m1"] {
            for skin in [false, true] {
                let fresh = View::load(&tx).unwrap().entity(id, skin).unwrap();
                assert_eq!(view.entity(id, skin).unwrap(), fresh);
            }
        }
        // The pending view is undone exactly (model preview), and a structure change is never absorbed.
        view.restore(undo);
        assert!(same(&view, &before));
        tx.execute("UPDATE entities SET variant='other' WHERE id='s1'", []).unwrap();
        assert!(view.absorb(&tx).unwrap().is_none());
        assert!(same(&view, &before));
    }

    #[test]
    fn vocabulary_helpers_follow_python() {
        assert_eq!(readable("en_only_(series_q)", true), "En Only");
        assert_eq!(readable("series_none", false), "Series None");
        assert_eq!(readable("2nd_form", false), "2Nd Form");
        assert_eq!(readable("a_(b)_(c)", true), "A (B)");
        assert_eq!(readable("x_()", true), "X ()");
        assert_eq!(readable("mika_(swimsuit)_(blue_archive)", true), "Mika (Swimsuit)");
        assert_eq!(readable("o'neill", false), "O'Neill");
        assert_eq!(skin_zh("new_year_kimono").as_deref(), Some("新年 · 和服"));
        assert_eq!(skin_zh("swimsuit_x"), None);
        assert_eq!(skin_zh("festival_outfit"), Some("祭典服".to_owned()));
        assert_eq!(skin_zh(""), None);
    }
}
