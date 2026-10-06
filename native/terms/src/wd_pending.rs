//! 本地 WD 重复交付预检：只读两个快照，按与证据导入完全相同的内容指纹判断。
//! 有任何新目标、新版本、分数 / 方法变化或无效字段，都保留原证据包交付与拒绝路径。
use crate::store::{evidence_fingerprint, Claim, NewEvidence};
use crate::{Error, Result};
use rusqlite::{types::Value as Sql, Connection, OpenFlags};
use serde_json::{json, Map, Value};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;

type Key = (String, String, String, String, String);

pub(crate) fn pending(c: &mut Connection, source: &Path, uuids: &[String], library_id: &Value) -> Result<Value> {
    crate::schema::require_current(c)?;
    // 跳过交付也不能绕过包的绑定字段校验；无效字段交回原包路径产生原拒绝。
    if !library_id.as_str().is_some_and(|s| crate::package::valid_text(s, 128)) {
        return Ok(json!({"pending": uuids}));
    }
    let tx = c.transaction()?;
    let mut src = crate::sqlite::open_with_flags(source, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    src.busy_timeout(std::time::Duration::from_secs(20))?;
    let src = src.transaction()?;
    let have = src
        .prepare("PRAGMA table_info(labels)")?
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<rusqlite::Result<BTreeSet<_>>>()?;
    let method = if have.contains("source") { "source" } else { "'wd'" };
    let version = if have.contains("model_ver") { "model_ver" } else { "NULL" };
    let mut statement = src.prepare(&format!(
        "SELECT uuid,dim,value,conf,{method},{version} FROM labels
         WHERE uuid IN (SELECT value FROM json_each(?)) AND (+dim='rating' OR (+dim='general' AND +conf>=0.2))"
    ))?;
    let mut pending = BTreeSet::new();
    let data = Map::new();
    // 与原证据包相同的图片分块上限；两个读事务覆盖全部块，不把全库证据常驻内存。
    for chunk in uuids.chunks(1000) {
        let scope = serde_json::to_string(chunk).map_err(|e| Error::Invalid(e.to_string()))?;
        let existing: HashMap<Key, (String, Vec<u8>)> = tx
            .prepare_cached(
                "SELECT a.key,t.kind,l.key,e.method,e.version,t.key,e.fingerprint
         FROM term_assets a JOIN term_evidence e ON e.asset_id=a.id AND e.source='wd'
         JOIN terms t ON t.id=e.term_id AND t.kind IN ('topic','rating')
         JOIN term_links l ON l.term_id=t.id AND l.system=CASE t.kind WHEN 'topic' THEN 'wd' ELSE 'wd_rating' END
         WHERE a.namespace='library' AND a.removed_at IS NULL AND NOT EXISTS(SELECT 1 FROM meta WHERE key='folder_stale_wd:'||e.id) AND a.key IN (SELECT value FROM json_each(?))",
            )?
            .query_map([&scope], |r| {
                Ok(((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?), (r.get(5)?, r.get(6)?)))
            })?
            .collect::<rusqlite::Result<_>>()?;
        let mut rows = statement.query([&scope])?;
        let mut row_ids = HashSet::new();
        while let Some(r) = rows.next()? {
            let uuid: String = r.get(0)?;
            if pending.contains(&uuid) {
                continue;
            }
            let dim: String = r.get(1)?;
            let kind = if dim == "general" { "topic" } else { "rating" };
            let value = match r.get::<_, Sql>(2)? {
                Sql::Text(v) => v,
                _ => {
                    pending.insert(uuid);
                    continue;
                }
            };
            let score: Sql = r.get(3)?;
            let method: Sql = r.get(4)?;
            let version: Sql = r.get(5)?;
            let method = match method {
                Sql::Text(m) if !m.is_empty() => m,
                _ => {
                    pending.insert(uuid);
                    continue;
                }
            };
            let version = match version {
                Sql::Null => "unknown".to_owned(),
                Sql::Text(v) => {
                    if v.is_empty() {
                        "unknown".to_owned()
                    } else {
                        v
                    }
                }
                _ => {
                    pending.insert(uuid);
                    continue;
                }
            };
            let score = match score {
                Sql::Null => continue,
                Sql::Real(v) => v,
                Sql::Integer(v) => v as f64,
                _ => {
                    pending.insert(uuid);
                    continue;
                }
            };
            if !score.is_finite() || !(0.0..=1.0).contains(&score) {
                pending.insert(uuid);
                continue;
            }
            let row_id = format!("{uuid}:{dim}:{value}:{method}");
            if !crate::package::valid_token(&method)
                || !crate::package::valid_text(&version, 128)
                || !crate::package::valid_text(&uuid, 256)
                || !crate::package::valid_text(&value, 256)
                || !crate::package::valid_text(&row_id, 128)
                || !row_ids.insert(row_id)
            {
                pending.insert(uuid);
                continue;
            }
            let key = (uuid.clone(), kind.to_owned(), value, method.clone(), version.clone());
            let Some((term_key, fingerprint)) = existing.get(&key) else {
                pending.insert(uuid);
                continue;
            };
            let claim =
                Claim::Image { asset: 0, term: 0, target: json!({"asset": ["library", uuid], "term": term_key}) };
            let wanted = evidence_fingerprint(&NewEvidence {
                claim: &claim,
                source: "wd",
                method: &method,
                version: &version,
                score: Some((score, "wd_probability")),
                data: &data,
                observed_at: None,
                package: None,
            });
            if &wanted != fingerprint {
                pending.insert(uuid);
            }
        }
    }
    Ok(json!({"pending": pending}))
}
