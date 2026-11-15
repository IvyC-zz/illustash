//! 本地分组推荐。core/source 只读；回执仍只写机器缓存。
use super::{check_owner, write, Request};
use crate::{Error, Result};
use chrono::{DateTime, Local, NaiveDateTime, TimeZone};
use rusqlite::params;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

type Groups = Vec<(String, Vec<String>)>;
const RULES: [(&str, &str, &str, usize, f64); 5] = [
    ("duplicate", "same_picture", "auto", 1, 1.),
    ("similar", "same_picture", "auto", 1, 1.),
    ("oc", "group_oc", "candidate", 1, 0.9),
    ("cluster", "group_cluster", "candidate", 2, 0.4),
    ("run", "group_run", "candidate", 2, 0.3),
];

pub(super) fn timestamp(text: &str) -> Result<f64> {
    if let Ok(date) = DateTime::parse_from_rfc3339(text) {
        return Ok(date.timestamp() as f64 + date.timestamp_subsec_micros() as f64 / 1e6);
    }
    let date = NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S%.f")
        .or_else(|_| NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M:%S%.f"))
        .map_err(|e| Error::Invalid(format!("保存时间无效：{e}")))?;
    let date = Local
        .from_local_datetime(&date)
        .earliest()
        .ok_or_else(|| Error::Invalid("保存时间无法解析为本地时间".into()))?;
    Ok(date.timestamp() as f64 + date.timestamp_subsec_micros() as f64 / 1e6)
}

pub(super) fn library_groups(request: &Request) -> Result<[Groups; 5]> {
    let mut groups: [Groups; 5] = std::array::from_fn(|_| Vec::new());
    if request.library.join("library.sqlite").is_file() {
        let c = crate::app_library::read_connection(&request.library.join("library.sqlite"))?;
        // 沿用旧实现：这一整段按顺序查询，只在第一个 SQLite 操作错误处停下。
        // 已读的表保留结果；遇到缺失的表后，后面的表不再查询。
        let originals = format!(
            "SELECT m.entity,m.uuid
             FROM memberships m
             JOIN {ENTITIES} e ON e.id=m.entity
             WHERE e.original=1 AND e.label_only=0",
            ENTITIES = crate::character_entities::ENTITIES
        );
        for (index, sql) in [
            "SELECT grp,uuid FROM duplicates",
            "SELECT gid,uuid FROM similar_groups",
            originals.as_str(),
            "SELECT cid,uuid FROM clusters",
        ]
        .iter()
        .enumerate()
        {
            let loaded = (|| -> rusqlite::Result<()> {
                let mut statement = c.prepare(sql)?;
                let mut rows = statement.query([])?;
                while let Some(row) = rows.next()? {
                    let key = match row.get_ref(0)? {
                        rusqlite::types::ValueRef::Null => "null".into(),
                        rusqlite::types::ValueRef::Integer(n) => format!("i:{n}"),
                        rusqlite::types::ValueRef::Text(s) => format!("s:{}", String::from_utf8_lossy(s)),
                        _ => return Err(rusqlite::Error::InvalidQuery),
                    };
                    let asset: String = row.get(1)?;
                    if let Some((_, members)) = groups[index].iter_mut().find(|(k, _)| *k == key) {
                        members.push(asset);
                    } else {
                        groups[index].push((key, vec![asset]));
                    }
                }
                Ok(())
            })();
            if let Err(error) = loaded {
                if matches!(error, rusqlite::Error::SqliteFailure(_, _)) {
                    break;
                }
                return Err(error.into());
            }
        }
    }
    groups[4] = save_runs(request, &["pixiv", "x"])?;
    Ok(groups)
}

pub(super) fn save_runs(request: &Request, presets: &[&str]) -> Result<Groups> {
    let mut groups = Vec::new();
    let source = crate::app_library::read_connection(&request.library.join("source.sqlite"))?;
    for preset in presets {
        let mut pictures = source.prepare("SELECT uuid,added_at FROM assets WHERE in_scope=1 AND coalesce(trashed,0)=0 AND preset=? AND added_at IS NOT NULL AND added_at!=''")?
            .query_map([preset], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        // 有意用稳定的字典序排序：旧实现在转换时间戳之前就对编码后的字符串排序。
        pictures.sort_by(|a, b| a.1.cmp(&b.1));
        let mut last = None;
        let mut run = 0usize;
        for (asset, added) in pictures {
            let time = timestamp(&added)?;
            if last.is_none_or(|previous| time - previous > 300.) {
                groups.push((format!("{preset}:{run}"), Vec::new()));
                run += 1;
            }
            if let Some((_, members)) = groups.last_mut() {
                members.push(asset);
            }
            last = Some(time);
        }
    }
    Ok(groups)
}

pub(super) fn run(request: &Request, sequence: &mut u64) -> Result<Value> {
    let groups = library_groups(request)?;
    let mut c = super::staged_cache(&request.cache)?;
    let mut owners = BTreeMap::<String, BTreeSet<String>>::new();
    for row in c
        .prepare("SELECT asset,artist_id FROM image_artists WHERE status='auto'")?
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
    {
        let (asset, artist) = row?;
        owners.entry(asset).or_default().insert(artist);
    }
    let tx = c.transaction()?;
    tx.execute("DELETE FROM image_artists WHERE status='candidate' AND method LIKE 'group_%'", [])?;
    let (mut auto, mut candidates) = (0usize, 0usize);
    for (index, (_, method, status, least, score)) in RULES.iter().enumerate() {
        for (_, members) in &groups[index] {
            check_owner(request.owner_pid)?;
            let solved: Vec<_> = members.iter().filter(|u| owners.contains_key(*u)).collect();
            if solved.is_empty() || solved.len() == members.len() {
                continue;
            }
            let mut votes = BTreeMap::<&String, usize>::new();
            for asset in &solved {
                for artist in &owners[*asset] {
                    *votes.entry(artist).or_default() += 1;
                }
            }
            if votes.len() != 1 {
                continue;
            }
            let (artist, count) = votes.into_iter().next().ok_or_else(|| Error::Invalid("分组票数为空".into()))?;
            if count < *least || count != solved.len() {
                continue;
            }
            let proof = json!({"group":RULES[index].0,"count":count,"from":solved.iter().take(5).collect::<Vec<_>>()})
                .to_string();
            for asset in members {
                if owners.contains_key(asset) {
                    continue;
                }
                let n = tx.execute("INSERT INTO image_artists(asset,artist_id,method,score,evidence,status) VALUES (?,?,?,?,?,?) ON CONFLICT(asset,artist_id) DO NOTHING",
                    params![asset,artist,method,score,proof,status])?;
                if *status == "auto" {
                    auto += n;
                } else {
                    candidates += n;
                }
            }
        }
    }
    check_owner(request.owner_pid)?;
    tx.commit()?;
    *sequence += 1;
    write(
        &request.mailbox.join("progress.json"),
        &json!({"log":format!("分组推荐：同一张图直接认定 {auto} 张，推荐候选 {candidates} 张"),"sequence":*sequence}),
    )?;
    Ok(json!({"auto":auto,"candidates":candidates,"stopped":""}))
}
