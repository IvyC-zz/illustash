//! 角色的机器候选写成证据批次（方案第 7 节）。一次成功运行 = 每个可替换来源一个批次：覆盖范围内每张图、这个来源
//! 当前有效的证据就是本批次的行，覆盖到但没有行 = 成功的空结果。失败的运行整事务回滚，旧的有效集不动。
//! 版本按行取内容摘要（沿用画师的 `v4/<摘要>`）：同样的结果复用原证据行，不同的另起一行。
use crate::store::LIBRARY;
use crate::store::{add_evidence, Claim, NewEvidence};
use crate::strict_json::{canonical, sha256_hex};
use crate::{Error, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Map};
use std::collections::{BTreeMap, HashMap};

/// 旧 `candidates.source` → (证据来源, 方法, 量纲)。
pub fn source_of(legacy: &str) -> Result<(&'static str, &'static str, &'static str)> {
    match legacy {
        "WD" => Ok(("wd", "wd_character", "wd_probability")),
        "CCIP" => Ok(("ccip_index", "char_ext", "ccip_score")),
        other => Err(Error::Invalid(format!("候选来源只能是 WD / CCIP：{other}"))),
    }
}

fn asset(c: &Connection, uuid: &str) -> Result<i64> {
    if let Some(id) = crate::store::library_asset(c, uuid)? {
        return Ok(id);
    }
    if let Some(id) = c
        .prepare_cached("SELECT id FROM term_assets WHERE namespace=?1 AND key=?2")?
        .query_row([LIBRARY, uuid], |r| r.get(0))
        .optional()?
    {
        return Ok(id);
    }
    c.prepare_cached("INSERT INTO term_assets(namespace,key,removed_at) VALUES (?1,?2,CURRENT_TIMESTAMP)")?
        .execute([LIBRARY, uuid])?;
    Ok(c.last_insert_rowid())
}

pub enum Scope<'a> {
    All,
    Assets(&'a [String]),
}

/// 发布一次运行的候选。rows 的键是 (图, 身份 key, 旧来源 WD / CCIP)，值是分数（同键已取最大）。
pub fn publish(
    c: &Connection,
    producer: &str,
    scope: Scope,
    rows: &BTreeMap<(String, String, String), f64>,
) -> Result<()> {
    // 全库发布只改变这两个来源。没有新资产时，其他来源的有效集保持原样。
    let before_assets: i64 = c.query_row("SELECT count(*) FROM term_assets", [], |r| r.get(0))?;
    // 收编的整表版本已存着完全相同的候选，不再为第一次 refresh 复制一版证据。
    // 仅复用当前有效、无附加数据且量纲相同的迁移行；c1 行仍按原指纹冲突规则检查。
    let migrated: HashMap<(String, String, String), (f64, i64)> = if matches!(scope, Scope::All) {
        c.prepare_cached(
            "SELECT a.key,t.key,CASE e.source WHEN 'wd' THEN 'WD' ELSE 'CCIP' END,e.score,e.id
             FROM term_current_evidence e JOIN term_assets a ON a.id=e.asset_id AND a.namespace='library'
             JOIN terms t ON t.id=e.term_id JOIN term_tree r ON r.term_id=t.id
             WHERE e.version LIKE 'migration/%' AND e.data='{}' AND
               ((e.source='wd' AND e.method='wd_character' AND e.metric='wd_probability') OR
                (e.source='ccip_index' AND e.method='char_ext' AND e.metric='ccip_score'))",
        )?
        .query_map([], |r| Ok(((r.get(0)?, r.get(1)?, r.get(2)?), (r.get(3)?, r.get(4)?))))?
        .collect::<rusqlite::Result<_>>()?
    } else {
        HashMap::new()
    };
    // 全量发布只读一次已登记图片，避免为每条候选重复查询同一图片。
    // 未登记或已移除的图片仍走原登记逻辑，保留恢复与其他来源 all 批次的覆盖规则。
    let mut asset_ids: HashMap<String, i64> = if matches!(scope, Scope::All) {
        c.prepare_cached("SELECT key,id FROM term_assets WHERE namespace='library' AND removed_at IS NULL")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?
    } else {
        HashMap::new()
    };
    let mut versions: HashMap<u64, String> = HashMap::new();
    let mut published = Vec::new();
    for legacy in ["WD", "CCIP"] {
        let (source, method, metric) = source_of(legacy)?;
        c.prepare_cached("INSERT INTO term_evidence_sets(producer,source,method,scope) VALUES (?,?,?,?)")?
            .execute(params![producer, source, method, if matches!(scope, Scope::All) { "all" } else { "assets" }])?;
        let set = c.last_insert_rowid();
        published.push((source, method, set));
        if let Scope::Assets(uuids) = &scope {
            let mut put =
                c.prepare_cached("INSERT OR IGNORE INTO term_evidence_set_assets(set_id,asset_id) VALUES (?,?)")?;
            for u in uuids.iter() {
                put.execute(params![set, asset(c, u)?])?;
            }
        }
        for ((uuid, entity, _), score) in rows.iter().filter(|((_, _, s), _)| s == legacy) {
            let a = match asset_ids.get(uuid) {
                Some(id) => *id,
                None => {
                    let id = asset(c, uuid)?;
                    asset_ids.insert(uuid.clone(), id);
                    id
                }
            };
            if let Some((_, id)) = migrated
                .get(&(uuid.clone(), entity.clone(), legacy.to_owned()))
                .filter(|(old_score, _)| old_score == score)
            {
                c.prepare_cached("INSERT OR IGNORE INTO term_evidence_set_rows(set_id,evidence_id) VALUES (?,?)")?
                    .execute([set, *id])?;
                continue;
            }
            let term: i64 = c
                .prepare_cached("SELECT t.id FROM terms t JOIN term_tree r ON r.term_id=t.id WHERE t.key=?")?
                .query_row([entity], |r| r.get(0))
                .optional()?
                .ok_or_else(|| Error::Conflict(format!("候选的身份 {entity} 不在词条核心里")))?;
            let version = versions
                .entry(score.to_bits())
                .or_insert_with(|| format!("c1/{}", &sha256_hex(canonical(&json!([score])).as_bytes())[..12]));
            let claim = Claim::Image { asset: a, term, target: json!({"asset": [LIBRARY, uuid], "term": entity}) };
            let data = Map::new();
            add_evidence(
                c,
                &NewEvidence {
                    claim: &claim,
                    source,
                    method,
                    version,
                    score: Some((*score, metric)),
                    data: &data,
                    observed_at: None,
                    package: None,
                },
            )?;
            let id: i64 = c.prepare_cached("SELECT id FROM term_evidence WHERE asset_id=? AND term_id=? AND source=? AND method=? AND version=?")?.query_row(
                params![a, term, source, method, version.as_str()],
                |r| r.get(0),
            )?;
            c.prepare_cached("INSERT OR IGNORE INTO term_evidence_set_rows(set_id,evidence_id) VALUES (?,?)")?
                .execute([set, id])?;
        }
        match &scope {
            Scope::All => {} // No reader needs the intermediate one-source effective set.
            Scope::Assets(uuids) => {
                let mut put = c.prepare_cached(
                    "INSERT OR REPLACE INTO term_active_evidence(asset_id,source,method,set_id) VALUES (?,?,?,?)",
                )?;
                for u in uuids.iter() {
                    put.execute(params![asset(c, u)?, source, method, set])?;
                }
            }
        }
    }
    if matches!(scope, Scope::All) {
        let after_assets: i64 = c.query_row("SELECT count(*) FROM term_assets", [], |r| r.get(0))?;
        if before_assets != after_assets {
            // 新资产还必须继承其他来源先前的 all 批次，保留完整重建。
            crate::store::rebuild_active_evidence(c)?;
        } else {
            let mut update = c.prepare_cached(
                "INSERT OR REPLACE INTO term_active_evidence(asset_id,source,method,set_id)
                 SELECT id,?1,?2,?3 FROM term_assets",
            )?;
            for (source, method, set) in published {
                update.execute(params![source, method, set])?;
            }
            // WD 交付可能已在本事务之前登记新资产；也要补上此前 provenance 全库批次的覆盖。
            c.execute(
                "INSERT OR REPLACE INTO term_active_evidence(asset_id,source,method,set_id)
                 SELECT a.id,s.source,s.method,max(s.id) FROM term_evidence_sets s JOIN term_assets a
                 ON s.scope='all' OR EXISTS(SELECT 1 FROM term_evidence_set_assets x WHERE x.set_id=s.id AND x.asset_id=a.id)
                 WHERE s.source='provenance' GROUP BY a.id,s.source,s.method",
                [],
            )?;
        }
    }
    Ok(())
}

/// 把这些图的候选换成 rows（撤销旧合并时恢复旧版的结果）。
pub fn replace(
    c: &Connection,
    producer: &str,
    uuids: &[String],
    rows: &BTreeMap<(String, String, String), f64>,
) -> Result<()> {
    publish(c, producer, Scope::Assets(uuids), rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_publication_matches_full_active_rebuild_and_keeps_other_sources() {
        let mut c = Connection::open_in_memory().unwrap();
        crate::configure(&c).unwrap();
        crate::migrate(&mut c).unwrap();
        crate::store::register_asset(&c, "library", "a", None).unwrap();
        crate::store::register_asset(&c, "library", "b", None).unwrap();
        c.execute(
            "INSERT INTO term_evidence_sets(producer,source,method,scope) VALUES ('provenance','provenance','*','all')",
            [],
        )
        .unwrap();
        crate::store::rebuild_active_evidence(&c).unwrap();
        let other: i64 =
            c.query_row("SELECT set_id FROM term_active_evidence WHERE method='*' LIMIT 1", [], |r| r.get(0)).unwrap();
        for i in 0..3 {
            crate::store::register_asset(&c, "library", &format!("new-{i}"), None).unwrap();
            publish(&c, "测试空结果", Scope::All, &BTreeMap::new()).unwrap();
            assert!(crate::store::rebuild_active_evidence(&c).unwrap().is_empty());
            let kept: i64 = c
                .query_row("SELECT count(*) FROM term_active_evidence WHERE method='*' AND set_id=?", [other], |r| {
                    r.get(0)
                })
                .unwrap();
            assert_eq!(kept, 3 + i);
        }
    }
}
