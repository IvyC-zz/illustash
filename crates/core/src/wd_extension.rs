//! 独立角色证据批次。扩展头只给尚无主归属的图补一个机器归属，全部命中仍作为候选可读。
use crate::character_projection;
use crate::store::{self, Claim, NewEvidence, LIBRARY};
use crate::strict_json::sha256_hex;
use crate::{Error, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Map};
use std::collections::{hash_map::Entry, BTreeMap, BTreeSet, HashMap};

/// 线性扩展头的证据来源。
pub const SOURCE: &str = "wd_extension";
/// 线性扩展头的证据方法。
pub const METHOD: &str = "linear_head";
const METRIC: &str = "wd_extension_probability";

/// 某张图库图片上，一条越过包内阈值的扩展头命中。
pub struct Hit {
    /// 图库内部图片 ID。
    pub uuid: String,
    /// 冻结词表中的 Danbooru 角色键。
    pub tag: String,
    /// 线性头的 sigmoid 概率。
    pub probability: f64,
}

type Active = BTreeMap<i64, (String, BTreeSet<i64>)>;

fn current(c: &Connection, scope: Option<&[String]>) -> Result<Active> {
    let mut found = Active::new();
    let mut stmt = c.prepare_cached(
        "SELECT a.id,a.key,r.evidence_id FROM term_active_evidence x
         JOIN term_assets a ON a.id=x.asset_id AND a.namespace='library'
         JOIN term_evidence_set_rows r ON r.set_id=x.set_id
         JOIN term_evidence e ON e.id=r.evidence_id AND e.asset_id=x.asset_id
         WHERE x.source=?1 AND x.method=?2
           AND (?3 IS NULL OR a.key IN (SELECT value FROM json_each(?3)))",
    )?;
    let rows = stmt.query_map(params![SOURCE, METHOD, scope.map(|ids| json!(ids).to_string())], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?))
    })?;
    for row in rows {
        let (id, uuid, evidence) = row?;
        found.entry(id).or_insert_with(|| (uuid, BTreeSet::new())).1.insert(evidence);
    }
    Ok(found)
}

fn replace(c: &Connection, producer: &str, old: &Active, new: &Active) -> Result<usize> {
    let changed: BTreeMap<i64, &String> = old
        .iter()
        .filter(|(id, rows)| new.get(id) != Some(*rows))
        .chain(new.iter().filter(|(id, rows)| old.get(id) != Some(*rows)))
        .map(|(id, (uuid, _))| (*id, uuid))
        .collect();
    if changed.is_empty() {
        return Ok(0);
    }
    let cover = changed.keys().copied().collect::<Vec<_>>();
    let rows = cover.iter().filter_map(|id| new.get(id)).flat_map(|(_, ids)| ids.iter().copied()).collect();
    store::publish_asset_batch(c, producer, (SOURCE, METHOD), &cover, &rows, None)?;
    let uuids = changed.into_values().cloned().collect::<Vec<_>>();
    character_projection::refresh_candidates(c, &uuids)?;
    character_projection::memberships(c, Some(&uuids))?;
    Ok(uuids.len())
}

/// 以给定冻结包的命中更新整个图库的扩展候选。调用方持有核心写事务。
pub fn apply(c: &Connection, package: &str, hits: &[Hit]) -> Result<usize> {
    apply_inner(c, package, None, hits)
}

/// 只替换给定图片的扩展候选，范围外的命中会被拒绝。调用方持有核心写事务。
pub fn apply_scoped(c: &Connection, package: &str, scope: &[String], hits: &[Hit]) -> Result<usize> {
    apply_inner(c, package, Some(scope), hits)
}

fn apply_inner(c: &Connection, package: &str, scope: Option<&[String]>, hits: &[Hit]) -> Result<usize> {
    let before = current(c, scope)?;
    let scope = scope.map(|ids| ids.iter().collect::<BTreeSet<_>>());
    let mut after = Active::new();
    let mut terms = HashMap::new();
    let mut registered = false;
    for hit in hits {
        if scope.as_ref().is_some_and(|ids| !ids.contains(&hit.uuid)) {
            return Err(Error::invalid("WD 扩展候选超出扫描范围"));
        }
        if !hit.probability.is_finite() || !(0.0..=1.0).contains(&hit.probability) {
            return Err(Error::invalid("WD 扩展候选分数不合法"));
        }
        let live: bool = c
            .prepare_cached("SELECT EXISTS(SELECT 1 FROM assets WHERE uuid=?1)")?
            .query_row([&hit.uuid], |r| r.get(0))?;
        if !live {
            continue;
        }
        let asset = c
            .prepare_cached("SELECT id FROM term_assets WHERE namespace=?1 AND key=?2 AND removed_at IS NULL")?
            .query_row(params![LIBRARY, hit.uuid], |r| r.get(0))
            .optional()?;
        let asset = match asset {
            Some(id) => id,
            None => {
                registered = true;
                store::register_asset(c, LIBRARY, &hit.uuid, None)?
            }
        };
        let (key, term) = &*match terms.entry(hit.tag.as_str()) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => {
                let key = crate::character_import::extension_entity(c, &hit.tag)?;
                let term: i64 =
                    c.prepare_cached("SELECT id FROM terms WHERE key=?")?.query_row([&key], |r| r.get(0))?;
                entry.insert((key, term))
            }
        };
        // 同一个包重跑同一输入复用证据行。底座重算后分数变化时记录新行。
        let fingerprint = sha256_hex(format!("{}:{:016x}", hit.tag, hit.probability.to_bits()).as_bytes());
        let version = format!("{package}/{}", &fingerprint[..16]);
        let claim = Claim::Image { asset, term: *term, target: json!({"asset": [LIBRARY, hit.uuid], "term": key}) };
        let mut data = Map::new();
        data.insert("tag".into(), json!(hit.tag));
        store::add_evidence(
            c,
            &NewEvidence {
                claim: &claim,
                source: SOURCE,
                method: METHOD,
                version: &version,
                score: Some((hit.probability, METRIC)),
                data: &data,
                observed_at: None,
                package: None,
            },
        )?;
        let evidence = c.prepare_cached(
            "SELECT id FROM term_evidence WHERE asset_id=?1 AND term_id=?2 AND source=?3 AND method=?4 AND version=?5",
        )?.query_row(params![asset, term, SOURCE, METHOD, version], |r| r.get(0))?;
        after.entry(asset).or_insert_with(|| (hit.uuid.clone(), BTreeSet::new())).1.insert(evidence);
    }
    if registered {
        store::rebuild_active_evidence(c)?;
    }
    if !terms.is_empty() && character_projection::policy(c)? {
        crate::character_graph::baseline(c)?;
    }
    replace(c, &format!("extension:{package}"), &before, &after)
}

/// 撤回整个扩展来源。调用方持有核心写事务；保留历史证据和仍可被人工引用的角色词条。
pub fn withdraw(c: &Connection) -> Result<usize> {
    replace(c, "extension:withdraw", &current(c, None)?, &Active::new())
}

/// 图片被移除或 WD 底座结果作废时，撤回这些图的扩展候选。调用方持有核心写事务。
pub fn invalidate(c: &Connection, uuids: &[String]) -> Result<()> {
    if uuids.is_empty() {
        return Ok(());
    }
    let old = current(c, Some(uuids))?;
    replace(c, "extension:stale_wd", &old, &Active::new())?;
    Ok(())
}

// 撤销旧合并时还要考虑合并之后接入的扩展证据。按原身份读有效证据，避免用旧快照重新激活已撤回的候选。
pub(crate) fn images_of<'a>(c: &Connection, terms: impl Iterator<Item = &'a str>) -> Result<Vec<String>> {
    let mut images = BTreeSet::new();
    let mut stmt = c.prepare_cached(
        "SELECT a.key FROM terms t JOIN term_current_evidence e ON e.term_id=t.id
         JOIN term_assets a ON a.id=e.asset_id AND a.namespace='library'
         WHERE t.key=?1 AND e.source=?2 AND e.method=?3",
    )?;
    for term in terms {
        for row in stmt.query_map([term, SOURCE, METHOD], |r| r.get::<_, String>(0))? {
            images.insert(row?);
        }
    }
    Ok(images.into_iter().collect())
}

pub(crate) fn fill_unassigned(c: &Connection, scoped: bool) -> Result<()> {
    let hybrid = character_projection::policy(c)?;
    let (entities, family, restrictions) = if hybrid {
        (
            "identity_nodes n ON n.id=v.entity",
            "n.character_id",
            // 图库里没有 effective_assertions 视图（它只在旧 Python 迁移 SQL 里），这里按它的定义直接查：
            // 人工否定落在这个角色本身（不是造型），且这条否定没有被挂起。
            "NOT EXISTS(SELECT 1 FROM assertions s JOIN identity_nodes x ON x.id=s.entity
                WHERE s.uuid=v.uuid AND x.character_id=n.character_id AND x.variant_id IS NULL AND s.status='rejected'
                  AND NOT EXISTS(SELECT 1 FROM identity_suspended h WHERE h.uuid=s.uuid AND h.entity=s.entity))
             AND NOT EXISTS(SELECT 1 FROM identity_uncertain u WHERE u.uuid=v.uuid)
             AND NOT EXISTS(SELECT 1 FROM identity_suspended h WHERE h.uuid=v.uuid)",
        )
    } else {
        (
            "terms t JOIN term_tree e ON t.id=e.term_id AND t.key=v.entity",
            "coalesce(e.parent_id,t.key)",
            "NOT EXISTS(SELECT 1 FROM assertions s WHERE s.uuid=v.uuid AND s.entity=coalesce(e.parent_id,t.key)
                         AND s.status='rejected')",
        )
    };
    let scope = if scoped { "AND v.uuid IN (SELECT uuid FROM touched)" } else { "" };
    // 旧候选与人工结论已投影完成。扩展分数只在同一张图的扩展候选中比较。
    c.execute(
        &format!(
            "WITH eligible AS (
               SELECT v.uuid,{family} AS entity,v.score,
                      row_number() OVER(PARTITION BY v.uuid ORDER BY v.score DESC,{family},v.entity) AS n
               FROM candidates v JOIN assets a ON a.uuid=v.uuid JOIN {entities}
               JOIN terms f ON f.key={family} JOIN term_tree r ON r.term_id=f.id AND r.mascot=0
               WHERE v.source='WD_EXT' {scope} AND {restrictions}
                 AND NOT EXISTS(SELECT 1 FROM memberships m WHERE m.uuid=v.uuid)
                 AND NOT EXISTS(SELECT 1 FROM family_confirmed m WHERE m.uuid=v.uuid))
             INSERT INTO memberships
             SELECT uuid,entity,'machine','machine','WD_EXT',score,1,0,printf('001%020.12f1',score)
             FROM eligible WHERE n=1"
        ),
        [],
    )?;
    Ok(())
}
