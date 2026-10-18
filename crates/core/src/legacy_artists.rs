//! 画师收编（docs/TERMS.md 第 4 节第 2 步）：把溯源的旧表同步成词条核心里的画师词条、链接、名字、证据和裁决。
//!
//! 「先投影后收编」的第一段：旧表仍是写入方，这里只读它们（provenance.sqlite 以只读方式挂上，library.sqlite 的
//! `assets`、`actions`、`artist_verdicts` 只查不改），可以反复运行，每次把核心补到与旧表一致；一个事务，失败不留部分写入。
//!
//! - `artists` → `terms`（kind=artist，key 沿用画师 UUID，界面上 `#/?artist=<id>` 不变），另加一条 `provenance:<id>` 链接。
//! - `artist_links` → `term_links`；`artist_names` → `term_names`（显示名）；`merged_into` → `same_as` 关系证据。
//! - `image_artists` → 证据：来源 `provenance`，方法照旧。旧表没有算法版本，版本用内容摘要（`v4/<摘要>`）：
//!   行被改写后另起一条，旧的留着。`status` 记在证据 JSON 里，是否算画师由画师判定器按方法决定。
//! - `artist_verdicts` → 裁决：按 `actions` 历史重建，每条挂在原来那一步上；已撤销的步骤不收。重建后逐对核对，
//!   核心的有效裁决必须和 `artist_verdicts` 当前内容完全一致，否则整次回滚。
//! - 资产：命名空间 `library`，键是 `assets.uuid`。不在 `assets` 里的记为已移除，证据和裁决照留，不投影。

#[cfg(test)]
use crate::legacy_artists::cutover;
use crate::schema::{Kind, RelationKind};
use crate::store::{add_evidence, create_term, project, relation, AssetId, Claim, NewEvidence, TermId};
use crate::strict_json::{canonical, sha256_hex};
use crate::{Error, Result};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

/// 本图库资产的命名空间：键是 library.sqlite 的 `assets.uuid`，不区分来源是 Photos 还是文件夹。
pub const LIBRARY: &str = "library";
const ORIGIN: &str = "legacy:provenance";
const SOURCE: &str = "provenance";

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub assets_added: usize,
    pub assets_removed: usize,
    pub assets_restored: usize,
    pub terms_created: usize,
    pub links_changed: usize,
    pub names_changed: usize,
    pub evidence_added: usize,
    pub evidence_unchanged: usize,
    pub verdicts_added: usize,
    pub verdict_pairs_checked: usize,
    pub projected: usize,
}

impl Report {
    pub fn to_json(&self) -> Value {
        json!({
            "assets_added": self.assets_added, "assets_removed": self.assets_removed, "assets_restored": self.assets_restored,
            "terms_created": self.terms_created, "links_changed": self.links_changed, "names_changed": self.names_changed,
            "evidence_added": self.evidence_added, "evidence_unchanged": self.evidence_unchanged,
            "verdicts_added": self.verdicts_added, "verdict_pairs_checked": self.verdict_pairs_checked,
            "projected": self.projected,
        })
    }
}

/// 把 `provenance` 指向的 provenance.sqlite 同步进 `conn`（library.sqlite）。切换之后拒绝。
pub fn sync(conn: &mut Connection, provenance: &Path) -> Result<Report> {
    sync_with_attachment(conn, provenance, provenance)
}

/// The caller retains the requested/display path while supplying the final SQLite operand.
/// Validation and errors use the original request; only ATTACH uses platform normalization.
pub fn sync_with_attachment(conn: &mut Connection, provenance: &Path, attachment: &Path) -> Result<Report> {
    crate::schema::require_current(conn)?;
    if !provenance.is_file() {
        return Err(Error::Invalid(format!("找不到 {path}", path = provenance.display())));
    }
    attached(conn, Some(attachment), false)
}

/// 切换：建表或升级、最后一次同步，并在同一个事务里写下 `terms_artists=core`、删掉 `artist_verdicts`（第 4 版起不留
/// 冻结副本，回退靠切换前的自动备份）。此后「是 / 不是」由核心写，溯源结果只经证据包进来。已经切过就什么都不做，返回 None。
/// 没有 provenance.sqlite（从没跑过溯源）也可以切：按空的溯源库处理，只收人工判断。
pub fn cutover_with_backup(
    conn: &mut Connection,
    provenance: Option<&Path>,
    backup: &crate::backup::RequiredBackup,
) -> Result<Option<Report>> {
    crate::schema_rules::migrate_with_backup(conn, backup)?;
    if crate::store::artists_in_core(conn)? {
        return Ok(None);
    }
    crate::schema_rules::backup_before_cutover(conn, "artists", backup)?;
    attached(conn, provenance.filter(|p| p.is_file()), true).map(Some)
}

fn attached(conn: &mut Connection, provenance: Option<&Path>, cutover: bool) -> Result<Report> {
    // ATTACH 不能在事务里做；只读 URI，绝不写溯源库。没有溯源库时挂一个内存里的空库，走同一条路径。
    match provenance {
        Some(path) => {
            conn.execute("ATTACH DATABASE ?1 AS prov", [file_uri(path)])?;
        }
        None => {
            conn.execute("ATTACH DATABASE ':memory:' AS prov", [])?;
            conn.execute_batch(EMPTY_PROVENANCE)?;
        }
    }
    let result = sync_attached(conn, cutover);
    let detached = conn.execute("DETACH DATABASE prov", []);
    let report = result?;
    detached?;
    Ok(report)
}

/// 本模块读的四张溯源表（provenance/db.py v4），空的。
const EMPTY_PROVENANCE: &str = "
CREATE TABLE prov.artists(id TEXT PRIMARY KEY, merged_into TEXT);
CREATE TABLE prov.artist_links(system TEXT, key TEXT, artist_id TEXT, url TEXT, source TEXT);
CREATE TABLE prov.artist_names(artist_id TEXT, source TEXT, name TEXT);
CREATE TABLE prov.image_artists(asset TEXT, artist_id TEXT, method TEXT, score REAL, evidence TEXT, status TEXT, updated_at TEXT);
";

fn file_uri(path: &Path) -> String {
    let mut out = String::from("file:");
    for b in path.to_string_lossy().bytes() {
        if b.is_ascii_alphanumeric() || b"/-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out + "?mode=ro"
}

fn sync_attached(conn: &mut Connection, cutover: bool) -> Result<Report> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if crate::store::artists_in_core(&tx)? {
        return Err(Error::Conflict("画师已切到词条核心，旧表不再同步；溯源结果走证据包".into()));
    }
    let mut r = Report::default();
    let mut touched: BTreeSet<AssetId> = BTreeSet::new();

    // 资产：跟随 assets；重新出现的恢复，不在了的记为移除。
    let restored = ids(&tx, "SELECT id FROM term_assets WHERE namespace='library' AND removed_at IS NOT NULL AND key IN (SELECT uuid FROM main.assets)")?;
    let removed = ids(&tx, "SELECT id FROM term_assets WHERE namespace='library' AND removed_at IS NULL AND key NOT IN (SELECT uuid FROM main.assets WHERE uuid IS NOT NULL)")?;
    r.assets_added = tx.execute(
        "INSERT INTO term_assets(namespace,key) SELECT 'library',uuid FROM main.assets WHERE uuid IS NOT NULL ON CONFLICT(namespace,key) DO NOTHING",
        [],
    )?;
    tx.execute("UPDATE term_assets SET removed_at=NULL WHERE namespace='library' AND removed_at IS NOT NULL AND key IN (SELECT uuid FROM main.assets)", [])?;
    tx.execute(
        "UPDATE term_assets SET removed_at=CURRENT_TIMESTAMP WHERE namespace='library' AND removed_at IS NULL AND key NOT IN (SELECT uuid FROM main.assets WHERE uuid IS NOT NULL)",
        [],
    )?;
    (r.assets_restored, r.assets_removed) = (restored.len(), removed.len());
    touched.extend(restored);
    touched.extend(removed);
    let mut assets: HashMap<String, AssetId> = HashMap::new();
    for row in tx
        .prepare("SELECT key,id FROM term_assets WHERE namespace='library'")?
        .query_map([], |x| Ok((x.get(0)?, x.get(1)?)))?
    {
        let (k, v) = row?;
        assets.insert(k, v);
    }

    // 画师词条与 provenance 链接。
    r.terms_created = tx.execute(
        "INSERT INTO terms(key,kind,origin) SELECT id,'artist',?1 FROM prov.artists a WHERE NOT EXISTS(SELECT 1 FROM terms t WHERE t.key=a.id)",
        [ORIGIN],
    )?;
    let mut terms: HashMap<String, TermId> = HashMap::new();
    for row in tx
        .prepare("SELECT t.key,t.id FROM terms t JOIN prov.artists a ON a.id=t.key")?
        .query_map([], |x| Ok((x.get(0)?, x.get(1)?)))?
    {
        let (k, v) = row?;
        terms.insert(k, v);
    }
    let artist_ids: Vec<String> = tx
        .prepare("SELECT id FROM prov.artists ORDER BY id")?
        .query_map([], |x| x.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for id in &artist_ids {
        r.links_changed += link(&tx, terms[id], "provenance", id, "", SOURCE)?;
    }
    let links: Vec<(String, String, String, String, String)> = tx
        .prepare("SELECT system,key,artist_id,url,source FROM prov.artist_links ORDER BY system,key")?
        .query_map([], |x| Ok((x.get(0)?, x.get(1)?, x.get(2)?, x.get(3)?, x.get(4)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (system, key, artist, url, source) in links {
        let term = term_for(&tx, &mut terms, &artist, &mut r)?;
        r.links_changed += link(&tx, term, &system, &key, &url, &source)?;
    }

    // 显示名：每个 (画师, 来源) 一个，跟随旧表。
    let names: Vec<(String, String, String)> = tx
        .prepare("SELECT artist_id,source,name FROM prov.artist_names ORDER BY artist_id,source")?
        .query_map([], |x| Ok((x.get(0)?, x.get(1)?, x.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (artist, source, name) in names {
        let term = term_for(&tx, &mut terms, &artist, &mut r)?;
        let stale = tx.execute(
            "DELETE FROM term_names WHERE term_id=?1 AND role='display' AND lang='' AND source=?2 AND value<>?3",
            params![term, source, name],
        )?;
        let added = tx.execute(
            "INSERT OR IGNORE INTO term_names(term_id,lang,value,role,source) VALUES (?1,'',?2,'display',?3)",
            params![term, name, source],
        )?;
        r.names_changed += stale + added;
    }

    // 合并：same_as 关系证据（b 是保留的那位）。
    let merges: Vec<(String, String)> = tx
        .prepare("SELECT id,merged_into FROM prov.artists WHERE merged_into IS NOT NULL ORDER BY id")?
        .query_map([], |x| Ok((x.get(0)?, x.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (a, b) in merges {
        let (ta, tb) = (term_for(&tx, &mut terms, &a, &mut r)?, term_for(&tx, &mut terms, &b, &mut r)?);
        let rel = relation(&tx, ta, tb, RelationKind::SameAs)?;
        let claim = Claim::Relation { relation: rel, target: json!({"relation": ["same_as", a, b]}) };
        let data = Map::new();
        let e = NewEvidence {
            claim: &claim,
            source: SOURCE,
            method: "merge",
            version: "v4",
            score: None,
            data: &data,
            observed_at: None,
            package: None,
        };
        if add_evidence(&tx, &e)? {
            r.evidence_added += 1;
        } else {
            r.evidence_unchanged += 1;
        }
    }

    // 归属 → 证据。image_artists 是溯源的全量：同步后它就是溯源来源的有效证据（schema 第 3 版，整个来源按批次生效）。
    let mut snapshot: BTreeSet<i64> = BTreeSet::new();
    type Row = (String, String, String, Option<f64>, String, String, String);
    let rows: Vec<Row> = tx
        .prepare("SELECT asset,artist_id,method,score,evidence,status,updated_at FROM prov.image_artists ORDER BY asset,artist_id")?
        .query_map([], |x| Ok((x.get(0)?, x.get(1)?, x.get(2)?, x.get(3)?, x.get(4)?, x.get(5)?, x.get(6)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (asset_key, artist, method, score, evidence, status, updated_at) in rows {
        let asset = asset_for(&tx, &mut assets, &asset_key)?;
        let term = term_for(&tx, &mut terms, &artist, &mut r)?;
        let mut data = match serde_json::from_str::<Value>(&evidence) {
            Ok(Value::Object(m)) => m,
            _ => Map::from_iter([("raw".to_string(), Value::String(evidence))]),
        };
        data.insert("status".into(), Value::String(status));
        let metric = metric_of(&method);
        let score = score.filter(|s| s.is_finite()).map(|s| (s, metric.as_str()));
        let version = format!(
            "v4/{}",
            &sha256_hex(canonical(&json!({"score": score.map(|s| s.0), "data": data})).as_bytes())[..12]
        );
        let observed = observed_at(&updated_at);
        let claim = Claim::Image { asset, term, target: json!({"asset": [LIBRARY, asset_key], "term": artist}) };
        let e = NewEvidence {
            claim: &claim,
            source: SOURCE,
            method: &method,
            version: &version,
            score,
            data: &data,
            observed_at: observed.as_deref(),
            package: None,
        };
        if add_evidence(&tx, &e)? {
            r.evidence_added += 1;
            touched.insert(asset);
        } else {
            r.evidence_unchanged += 1;
        }
        snapshot.insert(tx.query_row(
            "SELECT id FROM term_evidence WHERE asset_id=?1 AND term_id=?2 AND source=?3 AND method=?4 AND version=?5",
            params![asset, term, SOURCE, method, version],
            |x| x.get(0),
        )?);
    }
    touched.extend(publish_all(&tx, &snapshot)?);

    // 人工裁决：按历史重建。
    let (added, checked, verdict_assets) = sync_verdicts(&tx, &mut assets, &mut terms, &mut r)?;
    r.verdicts_added = added;
    r.verdict_pairs_checked = checked;
    touched.extend(verdict_assets);

    let touched: Vec<AssetId> = touched.into_iter().collect();
    project(&tx, &touched)?;
    r.projected = touched.len();
    if cutover {
        tx.execute("INSERT OR REPLACE INTO meta(key,value) VALUES (?1,'core')", [crate::store::ARTISTS_IN_CORE])?;
        tx.execute_batch("DROP TABLE IF EXISTS artist_verdicts")?;
    }
    tx.commit()?;
    Ok(r)
}

type Pair = (String, String); // (asset uuid, artist id)

fn sync_verdicts(
    tx: &Connection,
    assets: &mut HashMap<String, AssetId>,
    terms: &mut HashMap<String, TermId>,
    r: &mut Report,
) -> Result<(usize, usize, BTreeSet<AssetId>)> {
    // 每一对 (图, 画师) 在未撤销历史里的步骤，按历史顺序；每步记下它之前的值（had）。
    let mut steps: BTreeMap<Pair, Vec<(i64, Option<String>)>> = BTreeMap::new();
    let actions: Vec<(i64, String)> = tx
        .prepare("SELECT id,before_json FROM main.actions WHERE undone=0 AND before_json LIKE '%artist_verdict%' ORDER BY id")?
        .query_map([], |x| Ok((x.get(0)?, x.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (action, before) in actions {
        let Ok(Value::Array(items)) = serde_json::from_str::<Value>(&before) else {
            return Err(Error::Conflict(format!("历史第 {action} 步的记录不是数组")));
        };
        for item in items {
            let Some(v) = item.get("artist_verdict") else { continue };
            let (Some(uuid), Some(artist)) = (v["uuid"].as_str(), v["artist"].as_str()) else {
                return Err(Error::Conflict(format!("历史第 {action} 步的画师判断缺少图或画师")));
            };
            let had = v["had"].get("verdict").and_then(Value::as_str).map(str::to_owned);
            steps.entry((uuid.to_owned(), artist.to_owned())).or_default().push((action, had));
        }
    }
    let mut current: BTreeMap<Pair, String> = BTreeMap::new();
    let has_table: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM main.sqlite_master WHERE type='table' AND name='artist_verdicts')",
        [],
        |x| x.get(0),
    )?;
    let sql =
        if has_table { "SELECT uuid,artist,verdict FROM main.artist_verdicts" } else { "SELECT '','','' WHERE 0" };
    for row in tx.prepare(sql)?.query_map([], |x| Ok((x.get(0)?, x.get(1)?, x.get(2)?)))? {
        let (u, a, v): (String, String, String) = row?;
        current.insert((u, a), v);
    }
    let orphans: Vec<&Pair> = current.keys().filter(|p| !steps.contains_key(*p)).collect();
    if !orphans.is_empty() {
        return Err(Error::Conflict(format!(
            "有 {n} 条画师判断找不到对应的历史步骤（例如图 {image} 的画师 {artist}），不能确定挂在哪一步",
            n = orphans.len(),
            image = orphans[0].0,
            artist = orphans[0].1
        )));
    }
    let mut added = 0;
    let mut touched = BTreeSet::new();
    for (pair, list) in &steps {
        let asset = asset_for(tx, assets, &pair.0)?;
        let term = term_for(tx, terms, &pair.1, r)?;
        for (i, (action, _)) in list.iter().enumerate() {
            // 这一步设成的值 = 下一步之前的值；最后一步设成的就是表里现在的值。
            let value = match list.get(i + 1) {
                Some((_, had)) => had.clone(),
                None => current.get(pair).cloned(),
            };
            let verdict = value.unwrap_or_else(|| "clear".into());
            let exists: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM term_verdicts WHERE action_id=?1 AND asset_id=?2 AND term_id=?3)",
                params![action, asset, term],
                |x| x.get(0),
            )?;
            if !exists {
                tx.execute(
                    "INSERT INTO term_verdicts(action_id,anchor,asset_id,term_id,verdict,origin,reason) VALUES (?1,?1,?2,?3,?4,'legacy','artist_verdicts')",
                    params![action, asset, term, verdict],
                )?;
                added += 1;
                touched.insert(asset);
            }
        }
    }
    // Python 撤销旧步骤只改 actions.undone：所有带旧裁决的图都要重投。
    touched
        .extend(ids(tx, "SELECT DISTINCT asset_id FROM term_verdicts WHERE origin='legacy' AND asset_id IS NOT NULL")?);
    // 全体核对：核心里画师的有效裁决必须与 artist_verdicts 完全相等（有人绕过历史改表，就在这里发现）。
    let mut effective: BTreeMap<Pair, String> = BTreeMap::new();
    for row in tx
        .prepare(
            "SELECT a.key,t.key,v.verdict FROM term_effective_verdicts v
             JOIN term_assets a ON a.id=v.asset_id AND a.namespace='library' JOIN terms t ON t.id=v.term_id AND t.kind='artist'",
        )?
        .query_map([], |x| Ok((x.get(0)?, x.get(1)?, x.get(2)?)))?
    {
        let (u, a, v): (String, String, String) = row?;
        effective.insert((u, a), v);
    }
    if effective != current {
        let differ = current
            .keys()
            .chain(effective.keys())
            .find(|p| current.get(*p) != effective.get(*p))
            .expect("两边不等就有一对不同");
        return Err(Error::Conflict(format!(
            "图 {image} 的画师 {artist} 在核心里是 {core}，artist_verdicts 里是 {table}（表被绕过历史改过？）",
            image = differ.0,
            artist = differ.1,
            core = format!("{:?}", effective.get(differ)),
            table = format!("{:?}", current.get(differ))
        )));
    }
    Ok((added, steps.len(), touched))
}

/// 全库覆盖的溯源批次；与最近一个溯源批次内容相同时不新建。返回有效集变了的图。
fn publish_all(tx: &Connection, rows: &BTreeSet<i64>) -> Result<Vec<AssetId>> {
    let latest: Option<(i64, String)> = tx
        .query_row(
            "SELECT id,scope FROM term_evidence_sets WHERE source=?1 AND method='*' ORDER BY id DESC LIMIT 1",
            [SOURCE],
            |x| Ok((x.get(0)?, x.get(1)?)),
        )
        .optional()?;
    if let Some((set, scope)) = latest {
        let have: BTreeSet<i64> =
            ids(tx, &format!("SELECT evidence_id FROM term_evidence_set_rows WHERE set_id={set}"))?
                .into_iter()
                .collect();
        if scope == "all" && have == *rows {
            return Ok(vec![]);
        }
    }
    tx.execute(
        "INSERT INTO term_evidence_sets(producer,source,method,scope) VALUES (?1,?2,'*','all')",
        [ORIGIN, SOURCE],
    )?;
    let set = tx.last_insert_rowid();
    let mut add = tx.prepare_cached("INSERT INTO term_evidence_set_rows(set_id,evidence_id) VALUES (?1,?2)")?;
    for &id in rows {
        add.execute([set, id])?;
    }
    crate::store::rebuild_active_evidence(tx)
}

pub(crate) fn ids(conn: &Connection, sql: &str) -> Result<Vec<i64>> {
    Ok(conn.prepare(sql)?.query_map([], |x| x.get(0))?.collect::<rusqlite::Result<_>>()?)
}

/// 资产 id；不在本图库 `assets` 里的登记为已移除（证据和裁决照留）。
pub(crate) fn asset_for(conn: &Connection, assets: &mut HashMap<String, AssetId>, key: &str) -> Result<AssetId> {
    if let Some(&id) = assets.get(key) {
        return Ok(id);
    }
    conn.execute("INSERT INTO term_assets(namespace,key,removed_at) VALUES ('library',?1,CURRENT_TIMESTAMP)", [key])?;
    let id = conn.last_insert_rowid();
    assets.insert(key.to_owned(), id);
    Ok(id)
}

/// 画师词条；旧表里引用了但 `artists` 里没有的（外键未强制）也建一个，保证证据和裁决有处可挂。
fn term_for(conn: &Connection, terms: &mut HashMap<String, TermId>, artist: &str, r: &mut Report) -> Result<TermId> {
    if let Some(&id) = terms.get(artist) {
        return Ok(id);
    }
    let id = match crate::store::term_by_key(conn, artist)? {
        Some((id, _)) => id,
        None => {
            r.terms_created += 1;
            let id = create_term(conn, Kind::Artist, Some(artist), ORIGIN)?;
            link(conn, id, "provenance", artist, "", SOURCE)?;
            id
        }
    };
    terms.insert(artist.to_owned(), id);
    Ok(id)
}

/// 链接跟随旧表：没有就加，url 变了就改，归到了另一位（旧表里的）画师就移过去。挂在别的类型词条上则拒绝。
fn link(conn: &Connection, term: TermId, system: &str, key: &str, url: &str, source: &str) -> Result<usize> {
    let existing: Option<(TermId, String, String)> = conn
        .query_row(
            "SELECT l.term_id,l.url,t.kind FROM term_links l JOIN terms t ON t.id=l.term_id WHERE l.system=?1 AND l.key=?2",
            [system, key],
            |x| Ok((x.get(0)?, x.get(1)?, x.get(2)?)),
        )
        .optional()?;
    match existing {
        None => {
            conn.execute(
                "INSERT INTO term_links(system,key,term_id,url,source) VALUES (?1,?2,?3,?4,?5)",
                params![system, key, term, url, source],
            )?;
            Ok(1)
        }
        Some((owner, old_url, _)) if owner == term => {
            if old_url == url {
                return Ok(0);
            }
            conn.execute("UPDATE term_links SET url=?1 WHERE system=?2 AND key=?3", params![url, system, key])?;
            Ok(1)
        }
        Some((_, _, kind)) if kind == "artist" => {
            conn.execute(
                "UPDATE term_links SET term_id=?1,url=?2 WHERE system=?3 AND key=?4",
                params![term, url, system, key],
            )?;
            Ok(1)
        }
        Some((_, _, kind)) => {
            Err(Error::Conflict(format!("链接 {system}:{key} 已属于一个「{kind}」词条，不能归给画师")))
        }
    }
}

fn metric_of(method: &str) -> String {
    match method {
        "pixel_hash" => "exact_match".into(),
        "gallery_match" => "gallery_similarity".into(),
        "ocr" => "ocr_confidence".into(),
        other => format!("{}_score", other.to_ascii_lowercase()),
    }
}

/// SQLite CURRENT_TIMESTAMP（UTC，「YYYY-MM-DD HH:MM:SS」）转成带时区的 ISO 8601；认不出来就不记。
fn observed_at(value: &str) -> Option<String> {
    let iso = value.replacen(' ', "T", 1) + "Z";
    crate::package::is_timestamp(&iso).then_some(iso)
}

#[cfg(test)]
#[path = "legacy_artists/tests.rs"]
mod tests;
