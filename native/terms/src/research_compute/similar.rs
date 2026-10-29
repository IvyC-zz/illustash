//! 在显式的隔离快照上做相似计算。缓存交还给 App Research，走现有的校验和发布；这里不持有调度器。
//! 请求允许时（#31），每个缓存都以相对快照的变更列表返回。
use super::{
    features::Matrix,
    numeric::{pair_block_rows, sum32},
    relation_image, relations,
    snapshot::Snapshot,
};
use crate::{Error, Result};
use serde_json::{json, Value};
use std::{
    collections::{BTreeSet, HashMap, HashSet, VecDeque},
    path::Path,
    sync::Arc,
    time::Instant,
};
type Pair = (String, String);
fn ordered(a: &str, b: &str) -> Pair {
    if a < b {
        (a.into(), b.into())
    } else {
        (b.into(), a.into())
    }
}
pub fn grid_pairs(
    ids: &[String],
    ratios: &[f64],
    grids: &[Vec<u8>],
    query: Option<&HashSet<String>>,
) -> Result<BTreeSet<Pair>> {
    if ids.len() != grids.len() || ids.len() != ratios.len() || grids.iter().any(|g| g.len() != 256) {
        return Err(Error::Invalid("invalid relation signature matrix".into()));
    }
    let (mut keys, mut rows, mut logr) = (vec![], vec![], vec![]);
    for ((u, ratio), grid) in ids.iter().zip(ratios).zip(grids) {
        let mut row: Vec<_> = grid.iter().map(|x| f32::from(*x)).collect();
        let mean = sum32(&row) / 256.;
        for v in &mut row {
            *v -= mean;
        }
        let squares: Vec<_> = row.iter().map(|x| x * x).collect();
        let s = sum32(&squares);
        if (s / 256.).sqrt() < 6. {
            continue;
        }
        let norm = s.sqrt() + 1e-6f32;
        for v in &mut row {
            *v /= norm;
        }
        keys.push(u);
        rows.push(row);
        logr.push((*ratio as f32).ln());
    }
    let n = keys.len();
    let selected: Vec<_> = (0..n).filter(|i| query.is_none_or(|q| q.contains(keys[*i]))).collect();
    let right: Vec<_> = rows.iter().flatten().copied().collect();
    let mut pairs = BTreeSet::new();
    for block in selected.chunks(pair_block_rows(n)) {
        let left: Vec<_> = block.iter().flat_map(|i| rows[*i].iter().copied()).collect();
        let scores = illustash_platform::platform_research_math::cross(&left, block.len(), &right, n, 256)
            .map_err(Error::Invalid)?;
        for (ii, &i) in block.iter().enumerate() {
            let mut valid: Vec<_> = (0..n)
                .filter(|j| *j != i && scores[ii * n + j] >= 0.9f32 && (logr[i] - logr[*j]).abs() <= 0.03f32)
                .collect();
            if valid.len() > 12 {
                valid.sort_by(|a, b| {
                    let rounded = |j: usize| ((scores[ii * n + j] * 1e6f32).round_ties_even()) / 1e6f32;
                    rounded(*b).total_cmp(&rounded(*a)).then(keys[*a].cmp(keys[*b]))
                });
                valid.truncate(12);
            }
            for j in valid {
                pairs.insert(ordered(keys[i], keys[j]));
            }
        }
    }
    Ok(pairs)
}
pub fn ccip_pairs(features: &Matrix, ids: &[String], query: Option<&HashSet<String>>) -> Result<BTreeSet<Pair>> {
    let (keys, rows) = features.matrix(ids);
    let n = keys.len();
    let mut pairs = BTreeSet::new();
    if n == 0 {
        return Ok(pairs);
    }
    let selected: Vec<_> = (0..n).filter(|i| query.is_none_or(|q| q.contains(&keys[*i]))).collect();
    let right: Vec<_> = rows.iter().flatten().copied().collect();
    for block in selected.chunks(pair_block_rows(n)) {
        let left: Vec<_> = block.iter().flat_map(|i| rows[*i].iter().copied()).collect();
        let scores =
            illustash_platform::platform_research_math::cross(&left, block.len(), &right, n, features.dimension)
                .map_err(Error::Invalid)?;
        for (ii, &i) in block.iter().enumerate() {
            for j in 0..n {
                if i != j && scores[ii * n + j] > 0.988f32 {
                    pairs.insert(ordered(&keys[i], &keys[j]));
                }
            }
        }
    }
    Ok(pairs)
}
pub(crate) fn table(c: &rusqlite::Connection, name: &str) -> Result<Vec<Value>> {
    // 与现有表的形状完全一致，包括不在活动资产里的历史行。
    if !["image_sigs", "relations", "similar_paired", "similar_retry"].contains(&name) {
        return Err(Error::Invalid("unknown similarity table".into()));
    }
    let exists: bool =
        c.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name=? AND type='table')", [name], |r| r.get(0))?;
    if !exists {
        return Ok(vec![]);
    }
    let mut stmt = c.prepare(&format!("SELECT * FROM {name}"))?;
    let width = stmt.column_count();
    let rows = stmt
        .query_map([], |r| {
            let mut row = vec![];
            for i in 0..width {
                use rusqlite::types::ValueRef;
                row.push(match r.get_ref(i)? {
                    ValueRef::Null => Value::Null,
                    ValueRef::Integer(v) => v.into(),
                    ValueRef::Real(v) => v.into(),
                    ValueRef::Text(v) => String::from_utf8_lossy(v).into_owned().into(),
                    ValueRef::Blob(v) => v.iter().map(|b| format!("{b:02x}")).collect::<String>().into(),
                });
            }
            Ok(Value::Array(row))
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}
fn as_str(row: &Value, i: usize) -> Result<&str> {
    row[i].as_str().ok_or_else(|| Error::Invalid("invalid similarity cache row".into()))
}
fn decode_grid(value: &Value) -> Result<Vec<u8>> {
    let s = value.as_str().ok_or_else(|| Error::Invalid("invalid image signature".into()))?;
    if s.len() != 512 || !s.is_ascii() {
        return Err(Error::Invalid("invalid image signature".into()));
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|_| Error::Invalid("invalid image signature".into())))
        .collect()
}
fn replace(rows: &mut Vec<Value>, row: Value) {
    // INSERT OR REPLACE 先删掉旧行，再追加到末尾（SQLite 的 rowid 顺序）。
    rows.retain(|r| r[0] != row[0]);
    rows.push(row);
}
fn load_cached(
    cache: &mut HashMap<String, Arc<relation_image::Loaded>>,
    order: &mut VecDeque<String>,
    id: &str,
    path: &str,
) -> Result<Arc<relation_image::Loaded>> {
    let image =
        if let Some(image) = cache.get(id) { Arc::clone(image) } else { Arc::new(relations::load(Path::new(path))?) };
    order.retain(|u| u != id);
    order.push_back(id.into());
    cache.insert(id.into(), Arc::clone(&image));
    if order.len() > 48 {
        if let Some(old) = order.pop_front() {
            cache.remove(&old);
        }
    }
    Ok(image)
}
/// #31：`delete`（要去掉的快照行的主键）和 `append`（按顺序追加在保留行之后的行）把快照表变成 `new`，
/// rowid 顺序与整表重写得到的相同。没有主键的表整表返回。
pub(crate) fn changes(c: &rusqlite::Connection, name: &str, old: &[Value], new: Vec<Value>) -> Result<Value> {
    let mut columns: Vec<(i64, usize)> = c
        .prepare("SELECT pk,cid FROM pragma_table_info(?)")?
        .query_map([name], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    // 快照里还没有的表没有要删的行；App 会按它的键建表。
    let present = !columns.is_empty();
    columns.retain(|(pk, _)| *pk > 0);
    columns.sort();
    let key: Vec<usize> = columns.into_iter().map(|(_, i)| i).collect();
    if present && key.is_empty() {
        return Ok(Value::Array(new));
    }
    let plan = crate::table_delta::plan(
        old,
        &new,
        |r| key.iter().map(|&i| r[i].to_string()).collect::<Vec<_>>(),
        |a, b| a == b,
    );
    let delete: Vec<Value> =
        plan.delete.iter().map(|&i| Value::Array(key.iter().map(|&k| old[i][k].clone()).collect())).collect();
    let append: Vec<Value> = new.into_iter().skip(plan.kept.len()).collect();
    Ok(json!({"delete":delete,"append":append}))
}
pub fn scan(snapshot: &Snapshot, features: &Matrix, delta: bool) -> Result<(Value, Value)> {
    let started = Instant::now();
    let c = snapshot.read()?;
    let paths: Vec<(String, String)> = c
        .prepare(
            "SELECT a.uuid,a.src_path FROM assets a WHERE NOT EXISTS(SELECT 1 FROM excluded x WHERE x.uuid=a.uuid)",
        )?
        .query_map([], |r| Ok((r.get(0)?, r.get::<_, Option<String>>(1)?.unwrap_or_default())))?
        .collect::<rusqlite::Result<_>>()?;
    let path_map: HashMap<_, _> = paths.iter().cloned().collect();
    let mut sigs = table(&c, "image_sigs")?;
    let mut relations = table(&c, "relations")?;
    let mut paired = table(&c, "similar_paired")?;
    let mut retry = table(&c, "similar_retry")?;
    let before = delta.then(|| [sigs.clone(), relations.clone(), paired.clone(), retry.clone()]);
    let done: HashSet<_> =
        sigs.iter().filter(|r| r[3] == 1).map(|r| as_str(r, 0).map(str::to_owned)).collect::<Result<_>>()?;
    let todo: Vec<_> = paths.iter().filter(|(u, _)| !done.contains(u)).collect();
    super::progress_value(json!({"status":"reading","done":0,"total":todo.len()}))?;
    for (n, (u, path)) in todo.iter().enumerate() {
        let row = match relation_image::signature(Path::new(path)) {
            Ok((ratio, grid)) => json!([u, ratio, grid.iter().map(|b| format!("{b:02x}")).collect::<String>(), 1]),
            Err(_) => json!([u, Value::Null, Value::Null, 0]),
        };
        replace(&mut sigs, row);
        if (n + 1) % 200 == 0 || n + 1 == todo.len() {
            super::progress_value(json!({"status":"reading","done":n+1,"total":todo.len()}))?;
        }
    }
    super::progress_value(json!({"status":"pairing"}))?;
    let old: HashSet<_> = paired.iter().map(|r| as_str(r, 0).map(str::to_owned)).collect::<Result<_>>()?;
    let new: HashSet<_> = paths.iter().filter(|(u, _)| !old.contains(u)).map(|(u, _)| u.clone()).collect();
    let checked: HashSet<_> =
        relations.iter().map(|r| Ok((as_str(r, 0)?.to_owned(), as_str(r, 1)?.to_owned()))).collect::<Result<_>>()?;
    let mut pairs: BTreeSet<_> =
        retry.iter().map(|r| Ok((as_str(r, 0)?.to_owned(), as_str(r, 1)?.to_owned()))).collect::<Result<_>>()?;
    let (mut ids, mut ratios, mut grids) = (vec![], vec![], vec![]);
    for r in &sigs {
        let u = as_str(r, 0)?;
        if r[3] == 1 && path_map.contains_key(u) {
            ids.push(u.to_owned());
            ratios.push(r[1].as_f64().ok_or_else(|| Error::Invalid("invalid image signature ratio".into()))?);
            grids.push(decode_grid(&r[2])?);
        }
    }
    if !new.is_empty() {
        if !ids.is_empty() {
            pairs.extend(grid_pairs(&ids, &ratios, &grids, Some(&new))?);
        }
        pairs.extend(ccip_pairs(features, &paths.iter().map(|(u, _)| u.clone()).collect::<Vec<_>>(), Some(&new))?);
    }
    let copies: HashMap<String, String> = c
        .prepare("SELECT uuid,grp FROM duplicates")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let todo: Vec<_> = pairs
        .into_iter()
        .filter(|(a, b)| {
            !checked.contains(&(a.clone(), b.clone()))
                && path_map.contains_key(a)
                && path_map.contains_key(b)
                && !copies.get(a).is_some_and(|g| !g.is_empty() && copies.get(b) == Some(g))
        })
        .collect();
    super::progress_value(json!({"status":"comparing","done":0,"total":todo.len()}))?;
    let (mut cache, mut order) = (HashMap::new(), VecDeque::new());
    let mut found = 0;
    for (n, (a, b)) in todo.iter().enumerate() {
        let result = (|| {
            let aa = load_cached(&mut cache, &mut order, a, &path_map[a])?;
            let bb = load_cached(&mut cache, &mut order, b, &path_map[b])?;
            relations::relate(&aa, &bb)
        })();
        match result {
            Ok(r) => {
                found += usize::from(GROUPING.contains(&r["kind"].as_str().unwrap_or("")));
                relations.retain(|r| !(r[0] == *a && r[1] == *b));
                // SQLite 的 CURRENT_TIMESTAMP 用 UTC 秒；checked_at 仍是缓存时间戳。
                relations.push(json!([
                    a,
                    b,
                    r["kind"],
                    r.get("changed").cloned().unwrap_or(Value::Null),
                    r["note"],
                    chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string()
                ]));
                retry.retain(|r| !(r[0] == *a && r[1] == *b));
            }
            Err(_) => {
                if !retry.iter().any(|r| r[0] == *a && r[1] == *b) {
                    retry.push(json!([a, b]));
                }
            }
        }
        if (n + 1) % 25 == 0 || n + 1 == todo.len() {
            super::progress_value(json!({"status":"comparing","done":n+1,"total":todo.len(),"found":found}))?;
        }
    }
    let readable: HashSet<_> = ids.into_iter().collect();
    for (u, _) in &paths {
        if new.contains(u) && readable.contains(u) {
            paired.push(json!([u]));
        }
    }
    let tables = match before {
        Some([s, r, p, q]) => json!({
            "image_sigs": changes(&c, "image_sigs", &s, sigs)?,
            "relations": changes(&c, "relations", &r, relations)?,
            "similar_paired": changes(&c, "similar_paired", &p, paired)?,
            "similar_retry": changes(&c, "similar_retry", &q, retry)?,
        }),
        None => json!({"image_sigs":sigs,"relations":relations,"similar_paired":paired,"similar_retry":retry}),
    };
    Ok((
        json!({"status":"idle","compared":todo.len(),"found":found,"seconds":started.elapsed().as_secs_f64().round_ties_even() as u64,"at":chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()}),
        tables,
    ))
}
use relations::GROUPING;
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn flat_is_ignored_query_is_incremental_and_uuid_wins_six_digit_ties() {
        let ids: Vec<_> = (0..15).rev().map(|i| format!("{i:02}")).collect();
        let grid: Vec<_> = (0..256).map(|i| i as u8).collect();
        let mut grids = vec![grid; 15];
        grids.push(vec![100; 256]);
        let mut ids = ids;
        ids.push("flat".into());
        let query = HashSet::from(["14".into(), "flat".into()]);
        let pairs = grid_pairs(&ids, &[1.; 16], &grids, Some(&query)).unwrap();
        assert_eq!(pairs, (0..12).map(|i| (format!("{i:02}"), "14".into())).collect());
    }
}
