//! 用户标签、分级、WD 题材切到词条核心（docs/TERMS.md 第 4 节第 3 步，docs/TERMS_LABELS.md）。
//!
//! - 分级、WD 题材：机器证据由 tests/reference/web/wd_export.py 按证据包交来（链接 `wd_rating:<值>`、`wd:<标签>`），判定器见 schema.rs。
//! - 用户标签：用户自建的概念（kind=concept，显示名来源 `user`）；「加上」是裁决 yes，「去掉」是 clear（人收回，不是否定）。
//!   旧的 `asset_tags` 按 `actions` 历史重建成裁决，每条挂在原来那一步上，与画师收编同一个办法。
//!
//! 切换（`cutover`）在一个事务里：确认每张有分级的图都已交来 WD 证据；收编用户标签并全体核对；
//! 删掉 `asset_tags`（第 4 版起不留冻结副本，回退靠切换前的自动备份）和可从 source.sqlite 重建的
//! `general_hits`、`asset_rating`；换上三个同名同形的视图，Python 现有的读查询一行不改；写下 `terms_labels=core`。

use crate::legacy_artists::{asset_for, ids, LIBRARY};
#[cfg(test)]
use crate::legacy_labels::cutover;
use crate::schema::Kind;
use crate::store::{create_term, project, AssetId, TermId, LABELS_IN_CORE};
use crate::{Error, Result};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap};

const ORIGIN: &str = "legacy:tags";

/// Python 读的三张表换成同名同形的视图（列名、含义不变）。只显示仍在 `assets` 里的图。
pub const VIEWS: &str = "
CREATE VIEW general_hits(uuid,value) AS
 SELECT s.uuid,l.key FROM term_projection p
 JOIN terms t ON t.id=p.term_id AND t.kind='topic'
 JOIN term_links l ON l.term_id=t.id AND l.system='wd'
 JOIN term_assets a ON a.id=p.asset_id AND a.namespace='library'
 JOIN assets s ON s.uuid=a.key
 WHERE p.state IN ('machine','confirmed');
CREATE VIEW asset_rating(uuid,rating) AS
 SELECT s.uuid,l.key FROM term_projection p
 JOIN terms t ON t.id=p.term_id AND t.kind='rating'
 JOIN term_links l ON l.term_id=t.id AND l.system='wd_rating'
 JOIN term_assets a ON a.id=p.asset_id AND a.namespace='library'
 JOIN assets s ON s.uuid=a.key
 WHERE p.state IN ('machine','confirmed');
CREATE VIEW asset_tags(uuid,tag,origin,created_at) AS
 SELECT s.uuid,n.value,'review',v.created_at FROM term_effective_verdicts v
 JOIN terms t ON t.id=v.term_id AND t.kind='concept'
 JOIN term_names n ON n.term_id=t.id AND n.role='display' AND n.source='user' AND n.lang=''
 JOIN term_assets a ON a.id=v.asset_id AND a.namespace='library'
 JOIN assets s ON s.uuid=a.key
 WHERE v.verdict='yes';
";

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub concepts_created: usize,
    pub verdicts_added: usize,
    pub tag_pairs_checked: usize,
    pub rated_assets: usize,
}

impl Report {
    pub fn to_json(&self) -> Value {
        json!({"concepts_created": self.concepts_created, "verdicts_added": self.verdicts_added,
               "tag_pairs_checked": self.tag_pairs_checked, "rated_assets": self.rated_assets})
    }
}

/// 用户概念（标签）按名字找；`create` 时没有就建（显示名来源 `user`）。
pub fn concept_by_name(conn: &Connection, name: &str, create: Option<&str>) -> Result<Option<TermId>> {
    let found = conn
        .query_row(
            "SELECT t.id FROM terms t JOIN term_names n ON n.term_id=t.id AND n.role='display' AND n.source='user' AND n.lang=''
             WHERE t.kind='concept' AND n.value=?1 ORDER BY t.id LIMIT 1",
            [name],
            |r| r.get(0),
        )
        .optional()?;
    match (found, create) {
        (Some(id), _) => Ok(Some(id)),
        (None, Some(origin)) => {
            let id = create_term(conn, Kind::Concept, None, origin)?;
            crate::store::add_name(conn, id, "", name, "display", "user")?;
            Ok(Some(id))
        }
        (None, None) => Ok(None),
    }
}

/// 切换。已经切过返回 None。之前必须已把 WD 证据交齐（tests/reference/web/wd_export.py），否则拒绝并说还差几张。
pub fn cutover_with_backup(conn: &mut Connection, backup: &crate::backup::RequiredBackup) -> Result<Option<Report>> {
    crate::schema_rules::migrate_with_backup(conn, backup)?;
    if crate::store::labels_in_core(conn)? {
        return Ok(None);
    }
    crate::schema_rules::backup_before_cutover(conn, "labels", backup)?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if crate::store::labels_in_core(&tx)? {
        return Ok(None);
    }
    let missing: i64 = tx.query_row(
        "SELECT count(*) FROM asset_rating r WHERE NOT EXISTS(
           SELECT 1 FROM term_assets a JOIN term_evidence e ON e.asset_id=a.id JOIN terms t ON t.id=e.term_id AND t.kind='rating'
           WHERE a.namespace='library' AND a.key=r.uuid)",
        [],
        |r| r.get(0),
    )?;
    if missing > 0 {
        return Err(Error::Conflict(format!("还有 {missing} 张图的 WD 结果没交给词条核心，先交齐再切换")));
    }
    let mut report = Report {
        rated_assets: tx.query_row("SELECT count(*) FROM asset_rating", [], |r| r.get(0))?,
        ..Report::default()
    };
    sync_tags(&tx, &mut report)?;
    tx.execute_batch(
        "DROP TABLE asset_tags;
         DROP TABLE general_hits;
         DROP TABLE asset_rating;",
    )?;
    tx.execute_batch(VIEWS)?;
    tx.execute("INSERT OR REPLACE INTO meta(key,value) VALUES (?1,'core')", [LABELS_IN_CORE])?;
    tx.commit()?;
    Ok(Some(report))
}

type Pair = (String, String); // (asset uuid, tag)

/// 按历史重建用户标签的裁决：某一对 (图, 标签) 在未撤销历史里每一步设成的值，是下一步记下的 `had`（有 = 加上，
/// 无 = 去掉）；最后一步设成的就是 `asset_tags` 现在有没有。之后全体核对：核心里有效的「加上」必须与 `asset_tags` 相等。
fn sync_tags(tx: &Connection, report: &mut Report) -> Result<()> {
    let mut steps: BTreeMap<Pair, Vec<(i64, bool)>> = BTreeMap::new();
    let actions: Vec<(i64, String)> = tx
        .prepare("SELECT id,before_json FROM actions WHERE undone=0 AND before_json LIKE '%\"tag\"%' ORDER BY id")?
        .query_map([], |x| Ok((x.get(0)?, x.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (action, before) in actions {
        let Ok(Value::Array(items)) = serde_json::from_str::<Value>(&before) else { continue };
        for item in items {
            let Some(o) = item.as_object() else { continue };
            if o.keys().map(String::as_str).collect::<BTreeSet<_>>() != BTreeSet::from(["uuid", "tag", "had"]) {
                continue;
            }
            let (Some(uuid), Some(tag)) = (o["uuid"].as_str(), o["tag"].as_str()) else {
                return Err(Error::Conflict(format!("历史第 {action} 步的标签记录缺少图或标签")));
            };
            steps.entry((uuid.to_owned(), tag.to_owned())).or_default().push((action, !o["had"].is_null()));
        }
    }
    let current: BTreeSet<Pair> = tx
        .prepare("SELECT uuid,tag FROM asset_tags")?
        .query_map([], |x| Ok((x.get(0)?, x.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    if let Some(orphan) = current.iter().find(|p| !steps.contains_key(*p)) {
        let n = current.iter().filter(|p| !steps.contains_key(*p)).count();
        return Err(Error::Conflict(format!(
            "有 {n} 条用户标签找不到对应的历史步骤（例如图 {} 的标签 {}），不能确定挂在哪一步",
            orphan.0, orphan.1
        )));
    }
    let mut assets: HashMap<String, AssetId> = HashMap::new();
    for row in tx
        .prepare("SELECT key,id FROM term_assets WHERE namespace=?1")?
        .query_map([LIBRARY], |x| Ok((x.get(0)?, x.get(1)?)))?
    {
        let (k, v) = row?;
        assets.insert(k, v);
    }
    let mut concepts: HashMap<String, TermId> = HashMap::new();
    let mut touched = BTreeSet::new();
    for (pair, list) in &steps {
        let asset = asset_for(tx, &mut assets, &pair.0)?;
        let term = match concepts.get(&pair.1) {
            Some(&id) => id,
            None => {
                let before: i64 = tx.query_row("SELECT count(*) FROM terms", [], |r| r.get(0))?;
                let id = concept_by_name(tx, &pair.1, Some(ORIGIN))?.expect("create 时总有");
                let after: i64 = tx.query_row("SELECT count(*) FROM terms", [], |r| r.get(0))?;
                report.concepts_created += (after - before) as usize;
                concepts.insert(pair.1.clone(), id);
                id
            }
        };
        for (i, (action, _)) in list.iter().enumerate() {
            let applied = match list.get(i + 1) {
                Some((_, had)) => *had,
                None => current.contains(pair),
            };
            let exists: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM term_verdicts WHERE action_id=?1 AND asset_id=?2 AND term_id=?3)",
                params![action, asset, term],
                |x| x.get(0),
            )?;
            if !exists {
                tx.execute(
                    "INSERT INTO term_verdicts(action_id,anchor,asset_id,term_id,verdict,origin,reason) VALUES (?1,?1,?2,?3,?4,'legacy','asset_tags')",
                    params![action, asset, term, if applied { "yes" } else { "clear" }],
                )?;
                report.verdicts_added += 1;
                touched.insert(asset);
            }
        }
    }
    report.tag_pairs_checked = steps.len();
    touched
        .extend(ids(tx, "SELECT DISTINCT asset_id FROM term_verdicts WHERE origin='legacy' AND reason='asset_tags'")?);
    let effective: BTreeSet<Pair> = tx
        .prepare(
            "SELECT a.key,n.value FROM term_effective_verdicts v
             JOIN terms t ON t.id=v.term_id AND t.kind='concept'
             JOIN term_names n ON n.term_id=t.id AND n.role='display' AND n.source='user' AND n.lang=''
             JOIN term_assets a ON a.id=v.asset_id AND a.namespace='library' WHERE v.verdict='yes'",
        )?
        .query_map([], |x| Ok((x.get(0)?, x.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    if effective != current {
        let differ = current.symmetric_difference(&effective).next().expect("两边不等就有一对不同");
        return Err(Error::Conflict(format!("图 {} 的标签 {} 在核心和 asset_tags 里不一致", differ.0, differ.1)));
    }
    project(tx, &touched.into_iter().collect::<Vec<_>>())?;
    Ok(())
}

#[cfg(test)]
#[path = "legacy_labels/tests.rs"]
mod tests;
