//! 文件夹来源盘点：只读来源文件；source 提交与应用刷新是两个可重试边界。
use crate::{Error, Result};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::Path,
};

use crate::app_folder_identity::Earlier;
use crate::platform_folder::{describe, hash, walk};
#[derive(Debug)]
pub(crate) struct Scan {
    pub counts: Value,
    pub changed: Vec<String>,
}
/// 图片的文件变了：从旧文件识别出的结果删掉，重新识别。它的识别记录也删掉，否则导入会把新结果和旧结果比较并拒绝：
/// 不同的图片，或同一张图片换了执行后端（#270），从不会得到相同的字节。
fn forget_recognition(tx: &rusqlite::Transaction<'_>, asset: &str, recognised: bool) -> Result<()> {
    tx.execute("DELETE FROM labels WHERE uuid=?", [asset])?;
    tx.execute("DELETE FROM embeddings WHERE uuid=?", [asset])?;
    if recognised {
        tx.execute("DELETE FROM recognition_assets WHERE asset=?", [asset])?;
    }
    Ok(())
}
/// 首次扫描旧源库时扩展链接主键，保留已经存在的所有链接。调用方持有源库事务。
pub(super) fn ensure_links(c: &Connection) -> Result<()> {
    c.execute_batch(include_str!("app_source_schema.sql"))?;
    let old: bool =
        c.query_row("SELECT pk=0 FROM pragma_table_info('asset_links') WHERE name='asset'", [], |r| r.get(0))?;
    if old {
        c.execute_batch(
            "ALTER TABLE asset_links RENAME TO old_asset_links;
            DROP INDEX asset_links_asset;
            CREATE TABLE asset_links(asset TEXT NOT NULL,system TEXT NOT NULL,key TEXT NOT NULL,
                source_id TEXT NOT NULL,current INTEGER NOT NULL DEFAULT 1,PRIMARY KEY(system,key,source_id,asset));
            CREATE INDEX asset_links_asset ON asset_links(asset);
            CREATE UNIQUE INDEX asset_links_location ON asset_links(system,key,source_id) WHERE system<>'md5';
            CREATE INDEX IF NOT EXISTS files_asset ON files(asset);
            INSERT INTO asset_links SELECT asset,system,key,source_id,current FROM old_asset_links;
            DROP TABLE old_asset_links;",
        )?;
    }
    Ok(())
}

/// 包含未变和已经离线的旧文件。只清理已经失效的内容链接。
pub(super) fn update_links(c: &Connection, sid: &str) -> Result<()> {
    c.execute("DELETE FROM asset_links WHERE system='md5' AND source_id=? AND NOT EXISTS(
        SELECT 1 FROM files f WHERE f.asset=asset_links.asset AND f.source_id=asset_links.source_id AND f.md5=asset_links.key)", [sid])?;
    c.execute(
        "INSERT OR IGNORE INTO asset_links(asset,system,key,source_id)
        SELECT asset,'md5',md5,source_id FROM files WHERE source_id=? AND md5<>''",
        [sid],
    )?;
    Ok(())
}

pub(crate) fn scan(
    source: &Value,
    db: &Path,
    check: &dyn Fn() -> Result<()>,
    progress: &dyn Fn(usize, usize),
) -> Result<Scan> {
    scan_with(source, db, check, progress, None)
}
/// 抢救先盘点所有来源，避免先扫描的文件夹抢走另一个副本的 ID。
pub(crate) struct RescueInput {
    pub files: Vec<crate::platform_folder::File>,
    pub ids: HashMap<String, String>,
}
/// 使用普通遍历，或使用抢救对所有来源做完内容校验的结果。
pub(crate) fn scan_with(
    source: &Value,
    db: &Path,
    check: &dyn Fn() -> Result<()>,
    progress: &dyn Fn(usize, usize),
    rescue: Option<RescueInput>,
) -> Result<Scan> {
    let root = Path::new(source["path"].as_str().ok_or_else(|| Error::Invalid("来源缺少路径".into()))?);
    if !root.is_dir() {
        return Err(Error::Invalid(format!("文件夹不存在或无法访问：{path}", path = root.display())));
    }
    let sid = source["id"].as_str().ok_or_else(|| Error::Invalid("来源缺少 id".into()))?;
    let preset = source["preset"].as_str().unwrap_or("other");
    let mut c = crate::sqlite::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    c.busy_timeout(crate::app_library::TASK_WRITE_WAIT)?;
    let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    tx.execute_batch(include_str!("app_source_schema.sql"))?;
    let relocation = if rescue.is_none() { crate::app_relocation::begin(&tx, source)? } else { None };
    let earlier = relocation.as_ref().map(|op| &op.earlier);
    // 由第一次识别导入创建（app_recognition.rs）；从没识别过的图库没有这张表。
    let recognised: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='recognition_assets')",
        [],
        |r| r.get(0),
    )?;
    type Old = (i64, i64, String, String);
    let known_order = tx
        .prepare("SELECT path,size,mtime_ns,md5,asset FROM files WHERE source_id=?")?
        .query_map([sid], |r| {
            Ok((
                r.get::<_, String>(0)?,
                (r.get::<_, i64>(1)?, r.get::<_, i64>(2)?, r.get::<_, String>(3)?, r.get::<_, String>(4)?),
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let known: HashMap<String, Old> = known_order.iter().cloned().collect();
    // 某个来源仍有文件的图片保留它在那里的 ID；只有其余的图片才能拿原来的 ID。
    let taken = match earlier {
        Some(_) => tx
            .prepare("SELECT asset FROM files")?
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<HashSet<_>>>()?,
        None => HashSet::new(),
    };
    let trashed = tx
        .prepare("SELECT uuid FROM assets WHERE source_id=? AND coalesce(trashed,0)=1")?
        .query_map([sid], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<HashSet<_>>>()?;
    let (mut files, mut unreadable) = (Vec::new(), Vec::new());
    let rescued = rescue.is_some();
    let rescue_ids = if let Some(input) = rescue {
        files = input.files;
        input.ids
    } else {
        walk(root, root, &mut files, &mut unreadable, check)?;
        HashMap::new()
    };
    let mut seen = Vec::new();
    let mut hashed = Vec::new();
    let total = files.len();
    for (i, mut f) in files.into_iter().enumerate() {
        check()?;
        if rescued {
            hashed.push(f);
        } else if let Some(old) = known.get(&f.rel).filter(|old| old.0 == f.size && old.1 == f.mtime) {
            seen.push((f.rel, old.3.clone()));
        } else {
            match hash(&f.path, check) {
                Ok(h) => {
                    f.hash = h;
                    hashed.push(f);
                }
                Err(Error::Io(_)) => unreadable.push(f.rel.clone()),
                Err(e) => return Err(e),
            }
        }
        progress(i + 1, total);
    }
    if relocation.is_some() && !unreadable.is_empty() {
        return crate::error::invalid("新位置有文件无法读取，请恢复访问后重试或撤销搬家");
    }
    let present = seen.iter().map(|r| r.0.clone()).chain(hashed.iter().map(|f| f.rel.clone())).collect::<HashSet<_>>();
    // 读不了的路径不能确定已经消失：它的 asset 保持原样，直到某次扫描读到它。
    let unknown = |p: &str| unreadable.iter().any(|u| u.is_empty() || p == u || p.starts_with(&format!("{u}/")));
    let mut vanished = known_order
        .iter()
        .filter(|(p, _)| !present.contains(p) && !unknown(p))
        .map(|(p, _)| p.clone())
        .collect::<HashSet<_>>();
    let mut missing = Earlier::default();
    let mut old_paths = HashMap::new();
    for (p, old) in &known_order {
        if vanished.contains(p) {
            missing.record(&old.3, None, Some(&old.2));
            old_paths.insert(old.3.as_str(), p.as_str());
        }
    }
    let candidates: Vec<_> =
        hashed.iter().filter(|f| !known.contains_key(&f.rel)).map(|f| (f.rel.clone(), f.hash.clone())).collect();
    let movements = missing.matches(&candidates, &HashSet::new());
    let identities = earlier
        .map(|history| {
            let candidates: Vec<_> = hashed
                .iter()
                .filter(|f| !known.contains_key(&f.rel) && !movements.contains_key(&f.rel))
                .map(|f| (f.path.to_string_lossy().into_owned(), f.hash.clone()))
                .collect();
            history.matches(&candidates, &taken)
        })
        .unwrap_or(rescue_ids);
    let (mut new, mut moved, mut kept, mut changed) = (0, 0, 0, Vec::new());
    let journal_path = db.with_extension("scan-pending.json");
    let mut journal: serde_json::Map<String, Value> = if journal_path.exists() {
        serde_json::from_str(&fs::read_to_string(&journal_path)?).map_err(|e| Error::Invalid(e.to_string()))?
    } else {
        serde_json::Map::new()
    };
    let mut back = seen.iter().map(|r| r.1.clone()).filter(|u| trashed.contains(u)).collect::<Vec<_>>();
    let mut planned = Vec::new();
    for f in hashed {
        let movement = movements.get(&f.rel).map(|u| (old_paths[u.as_str()].to_owned(), u.clone()));
        let kind = if known.contains_key(&f.rel) {
            2
        } else if movement.is_some() {
            1
        } else {
            0
        };
        planned.push((kind, f, movement));
    }
    planned.sort_by_key(|p| p.0);
    for (_, f, movement) in planned {
        check()?;
        let asset;
        if let Some(old) = known.get(&f.rel) {
            asset = old.3.clone();
            // mtime/size 只是重算哈希的提示；内容没变时，识别结果和应用投影仍然有效。
            if old.2 != f.hash {
                changed.push(asset.clone());
                journal.insert(asset.clone(), json!(f.hash));
                let (w, h, _) = describe(&f);
                tx.execute(
                    "UPDATE assets SET width=?,height=?,stable_hash=?,orig_size=? WHERE uuid=?",
                    params![w, h, f.hash, f.size, asset],
                )?;
                forget_recognition(&tx, &asset, recognised)?;
            }
            tx.execute(
                "UPDATE files SET size=?,mtime_ns=?,md5=? WHERE source_id=? AND path=?",
                params![f.size, f.mtime, f.hash, sid, f.rel],
            )?;
            // 文件写回到它被删掉的那个路径，会在这次扫描回来，而不是下一次。
            if trashed.contains(&asset) {
                back.push(asset.clone());
            }
            continue;
        } else if let Some((old_path, u)) = movement {
            asset = u;
            moved += 1;
            vanished.remove(&old_path);
            tx.execute(
                "UPDATE assets SET filename=?,src_path=? WHERE uuid=?",
                params![
                    f.path.file_name().expect("app_folder internal invariant").to_string_lossy(),
                    f.path.to_string_lossy(),
                    asset
                ],
            )?;
            tx.execute(
                "DELETE FROM asset_links WHERE system='file' AND key=? AND source_id=?",
                params![old_path, sid],
            )?;
            tx.execute("DELETE FROM files WHERE source_id=? AND path=?", params![sid, old_path])?;
            if trashed.contains(&asset) {
                back.push(asset.clone());
            }
        } else if let Some(u) = identities.get(f.path.to_string_lossy().as_ref()).cloned() {
            asset = u;
            kept += 1;
            let name = f.path.file_name().expect("app_folder internal invariant").to_string_lossy().into_owned();
            let (w, h, added) = describe(&f);
            let had: Option<Option<String>> =
                tx.query_row("SELECT stable_hash FROM assets WHERE uuid=?", [&asset], |r| r.get(0)).optional()?;
            let before = match had {
                Some(hash) => {
                    tx.execute("UPDATE assets SET filename=?,width=?,height=?,trashed=0,stable_hash=?,orig_size=?,src_kind='orig',src_path=?,in_scope=1,source_id=?,preset=? WHERE uuid=?",params![name,w,h,f.hash,f.size,f.path.to_string_lossy(),sid,preset,asset])?;
                    hash
                }
                None => {
                    tx.execute("INSERT INTO assets(uuid,filename,width,height,added_at,hidden,trashed,stable_hash,orig_size,src_kind,src_path,in_scope,source_id,preset) VALUES (?,?,?,?,?,0,0,?,?,'orig',?,1,?,?)",params![asset,name,w,h,added,f.hash,f.size,f.path.to_string_lossy(),sid,preset])?;
                    Some(f.hash.clone())
                }
            };
            // 其他内容的结果不再描述这张图片：重新识别它。
            if before.as_deref() != Some(f.hash.as_str()) {
                forget_recognition(&tx, &asset, recognised)?;
            }
        } else {
            asset = crate::store::new_key()?.to_uppercase();
            new += 1;
            let (w, h, added) = describe(&f);
            tx.execute("INSERT INTO assets(uuid,filename,width,height,added_at,hidden,trashed,stable_hash,orig_size,src_kind,src_path,in_scope,source_id,preset) VALUES (?,?,?,?,?,0,0,?,?,'orig',?,1,?,?)",params![asset,f.path.file_name().expect("app_folder internal invariant").to_string_lossy(),w,h,added,f.hash,f.size,f.path.to_string_lossy(),sid,preset])?;
        }
        tx.execute(
            "INSERT OR REPLACE INTO asset_links(asset,system,key,source_id) VALUES (?,'file',?,?)",
            params![asset, f.rel, sid],
        )?;
        tx.execute(
            "INSERT OR REPLACE INTO files VALUES (?,?,?,?,?,?)",
            params![sid, f.rel, f.size, f.mtime, f.hash, asset],
        )?;
    }
    for p in &vanished {
        tx.execute("UPDATE assets SET trashed=1 WHERE uuid=?", [&known[p].3])?;
    }
    for u in &back {
        tx.execute("UPDATE assets SET trashed=0 WHERE uuid=?", [u])?;
    }
    let total: i64 = tx.query_row("SELECT count(*) FROM files WHERE source_id=?", [sid], |r| r.get(0))?;
    check()?;
    if !journal.is_empty() {
        use std::io::Write;
        let temp = db.with_extension(format!("scan-{}.tmp", crate::store::new_key()?));
        let publish = (|| -> Result<()> {
            let mut file = fs::OpenOptions::new().write(true).create_new(true).open(&temp)?;
            file.write_all(serde_json::to_string(&journal).expect("internal JSON value is serializable").as_bytes())?;
            file.sync_all()?;
            fs::rename(&temp, &journal_path)?;
            Ok(())
        })();
        if publish.is_err() {
            let _ = fs::remove_file(&temp);
        }
        publish?;
    }
    if total > 0 {
        ensure_links(&tx)?;
        update_links(&tx, sid)?;
    }
    if let Some(op) = &relocation {
        op.finish(&tx, kept, new)?;
    }
    check()?;
    tx.commit()?;
    let name = crate::origin::get(preset).label();
    let mut counts = json!({"new":new,"returned":back.len(),"gone":vanished.len(),"moved":moved,"changed":changed.len(),"sources":{sid:{"name":format!("{}（{}）",name,root.file_name().unwrap_or_default().to_string_lossy()),"photos":total-vanished.len() as i64,"collected":total-vanished.len() as i64}}});
    if !unreadable.is_empty() {
        counts["unreadable"] = json!(unreadable.len());
    }
    if rescued || earlier.is_some() {
        counts["kept"] = json!(kept);
    }
    Ok(Scan { counts, changed })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    #[test]
    fn interrupted_hashing_rolls_back_source_and_retry_can_commit() {
        let root = std::env::temp_dir().join(format!("illustash-folder-cancel-{}", crate::store::new_key().unwrap()));
        fs::create_dir(&root).unwrap();
        let images = root.join("images");
        fs::create_dir(&images).unwrap();
        image::RgbImage::new(4, 3).save(images.join("a.png")).unwrap();
        let db = root.join("source.sqlite");
        let c = crate::sqlite::open(&db).unwrap();
        c.execute_batch("CREATE TABLE assets(uuid TEXT PRIMARY KEY,filename TEXT,width INTEGER,height INTEGER,added_at TEXT,hidden INTEGER,trashed INTEGER,stable_hash TEXT,orig_size INTEGER,src_kind TEXT,src_path TEXT,in_scope INTEGER,source_id TEXT,preset TEXT); CREATE TABLE labels(uuid TEXT); CREATE TABLE embeddings(uuid TEXT);").unwrap();
        c.execute_batch(include_str!("app_source_schema.sql")).unwrap();
        let source = json!({"id":"s1","path":images,"preset":"other"});
        let cancel = AtomicBool::new(false);
        let result =
            scan(&source, &db, &|| crate::app_sync::check(&cancel), &|_, _| cancel.store(true, Ordering::SeqCst));
        assert!(matches!(result, Err(Error::Conflict(_))));
        assert_eq!(c.query_row("SELECT count(*) FROM assets", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        assert_eq!(c.query_row("SELECT count(*) FROM files", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        cancel.store(false, Ordering::SeqCst);
        assert_eq!(scan(&source, &db, &|| crate::app_sync::check(&cancel), &|_, _| {}).unwrap().counts["new"], 1);
        assert_eq!(scan(&source, &db, &|| crate::app_sync::check(&cancel), &|_, _| {}).unwrap().counts["new"], 0);
        drop(c);
        fs::remove_dir_all(root).unwrap();
    }
}
