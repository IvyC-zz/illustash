//! 文件位置的变更与撤销。文件账目、资产位置和撤销凭据在同一个源库事务中提交。
use crate::{app_folder_identity::Earlier, app_library::Library, Error, Result};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::HashSet, path::Path};

const HISTORY: &str = "CREATE TABLE IF NOT EXISTS folder_changes(
    token TEXT NOT NULL UNIQUE, sid TEXT NOT NULL, old_root TEXT, root TEXT NOT NULL,
    state TEXT NOT NULL, before_files TEXT, after_files TEXT, kept INTEGER, added INTEGER)";
const SELECT_FILES: &str = "SELECT f.source_id,f.path,f.size,f.mtime_ns,f.md5,f.asset,a.filename,a.src_path,
    coalesce(a.trashed,0),a.preset FROM files f JOIN assets a ON a.uuid=f.asset";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct FileRow {
    sid: String,
    path: String,
    size: i64,
    mtime: i64,
    hash: String,
    id: String,
    name: String,
    full: String,
    trashed: i64,
    preset: Option<String>,
}

impl FileRow {
    fn read(r: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            sid: r.get(0)?,
            path: r.get(1)?,
            size: r.get(2)?,
            mtime: r.get(3)?,
            hash: r.get(4)?,
            id: r.get(5)?,
            name: r.get(6)?,
            full: r.get(7)?,
            trashed: r.get(8)?,
            preset: r.get(9)?,
        })
    }

    fn detach(&self, c: &Connection) -> Result<()> {
        c.execute("DELETE FROM files WHERE source_id=? AND path=? AND asset=?", params![self.sid, self.path, self.id])?;
        c.execute(
            "DELETE FROM asset_links WHERE asset=? AND system IN ('file','md5') AND source_id=?",
            params![self.id, self.sid],
        )?;
        c.execute("UPDATE assets SET trashed=1 WHERE uuid=?", [&self.id])?;
        Ok(())
    }

    fn attach(&self, c: &Connection) -> Result<()> {
        c.execute(
            "INSERT INTO files(source_id,path,size,mtime_ns,md5,asset) VALUES (?,?,?,?,?,?)",
            params![self.sid, self.path, self.size, self.mtime, self.hash, self.id],
        )?;
        c.execute(
            "INSERT INTO asset_links(asset,system,key,source_id) VALUES (?,'file',?,?)",
            params![self.id, self.path, self.sid],
        )?;
        if !self.hash.is_empty() {
            c.execute(
                "INSERT INTO asset_links(asset,system,key,source_id) VALUES (?,'md5',?,?)",
                params![self.id, self.hash, self.sid],
            )?;
        }
        if c.execute(
            "UPDATE assets SET source_id=?,filename=?,src_path=?,trashed=?,preset=? WHERE uuid=?",
            params![self.sid, self.name, self.full, self.trashed, self.preset, self.id],
        )? != 1
        {
            return Err(changed());
        }
        Ok(())
    }
}

fn rows(c: &Connection, sid: &str) -> Result<Vec<FileRow>> {
    Ok(c.prepare(&format!("{SELECT_FILES} WHERE f.source_id=? ORDER BY f.path"))?
        .query_map([sid], FileRow::read)?
        .collect::<rusqlite::Result<_>>()?)
}
fn encode<T: Serialize>(v: &T) -> String {
    serde_json::to_string(v).expect("文件位置可序列化")
}
fn decode(s: &str) -> Result<Vec<FileRow>> {
    serde_json::from_str(s).map_err(|e| Error::Schema(format!("文件位置记录损坏：{e}")))
}
fn changed() -> Error {
    Error::Conflict("文件位置记录已变化，请重新查看来源".into())
}
pub(super) fn writer(library: &Library) -> Result<Connection> {
    let c = crate::sqlite::open_with_flags(library.source(), OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    c.busy_timeout(crate::app_library::TASK_WRITE_WAIT)?;
    Ok(c)
}
pub(super) fn prepare(c: &Connection, sid: &str, from: &str, to: &str) -> Result<String> {
    c.execute_batch(HISTORY)?;
    let token = crate::store::new_key()?;
    c.execute(
        "INSERT INTO folder_changes(token,sid,old_root,root,state) VALUES (?,?,?,?,'pending')",
        params![token, sid, from, to],
    )?;
    Ok(token)
}

pub(super) struct Move {
    pub earlier: Earlier,
    before: Vec<FileRow>,
    token: String,
    sid: String,
}

pub(super) fn begin(c: &Connection, source: &Value) -> Result<Option<Move>> {
    let Some(token) = source["folder_move"].as_str() else { return Ok(None) };
    let sid = source["id"].as_str().ok_or_else(changed)?;
    let root = source["path"].as_str().ok_or_else(changed)?;
    let state: String = c
        .query_row(
            "SELECT state FROM folder_changes WHERE token=? AND sid=? AND root=?",
            params![token, sid, root],
            |r| r.get(0),
        )
        .optional()?
        .ok_or_else(changed)?;
    if state == "done" {
        return Ok(None);
    }
    if state != "pending" {
        return Err(changed());
    }
    let before = rows(c, sid)?;
    let mut earlier = Earlier::default();
    for f in &before {
        let path = Path::new(root).join(&f.path);
        earlier.record(&f.id, path.to_str(), Some(&f.hash));
        f.detach(c)?;
    }
    Ok(Some(Move { earlier, before, token: token.into(), sid: sid.into() }))
}
impl Move {
    pub fn finish(&self, c: &Connection, kept: i64, added: i64) -> Result<()> {
        let after = rows(c, &self.sid)?;
        c.execute(
            "UPDATE folder_changes SET state='done',before_files=?,after_files=?,kept=?,added=? WHERE token=?",
            params![encode(&self.before), encode(&after), kept, added, self.token],
        )?;
        Ok(())
    }
}

fn has_history(c: &Connection) -> Result<bool> {
    Ok(c.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='folder_changes')",
        [],
        |r| r.get(0),
    )?)
}

/// 每个启用来源的最近一次认回。写配置之前中止的请求没有生效。
pub(super) fn reports(library: &Library) -> Result<Vec<Value>> {
    let c = crate::app_library::read_connection(&library.source())?;
    if !has_history(&c)? {
        return Ok(Vec::new());
    }
    let mut result = Vec::new();
    for s in library.config()["sources"].as_array().into_iter().flatten().filter(|s| s["kind"] == "folder") {
        let row = c
            .query_row(
                "SELECT token,old_root,root,state,kept,added FROM folder_changes WHERE sid=?
            AND (state='done' OR (state='pending' AND token=?)) ORDER BY rowid DESC LIMIT 1",
                params![s["id"].as_str(), s["folder_move"].as_str()],
                |r| {
                    Ok(json!({"source":s["id"],
                "token":r.get::<_,String>(0)?,"from":r.get::<_,Option<String>>(1)?,"to":r.get::<_,String>(2)?,
                "state":r.get::<_,String>(3)?,"kept":r.get::<_,Option<i64>>(4)?,"new":r.get::<_,Option<i64>>(5)?}))
                },
            )
            .optional()?;
        result.extend(row);
    }
    Ok(result)
}

pub(super) fn undo(c: &Connection, sid: &str, token: &str, root: &str) -> Result<Option<String>> {
    let (old, state, before, after): (Option<String>, String, Option<String>, Option<String>) = c
        .query_row(
            "SELECT old_root,state,before_files,after_files FROM folder_changes WHERE sid=? AND token=? AND root=?",
            params![sid, token, root],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?
        .ok_or_else(changed)?;
    if state == "undone" {
        return Ok(old);
    }
    if state == "done" {
        let before = decode(before.as_deref().ok_or_else(changed)?)?;
        let after = decode(after.as_deref().ok_or_else(changed)?)?;
        if old.is_some() && rows(c, sid)? != after {
            return Err(changed());
        }
        // 检查涉及的所有位置，也包括跨来源认回后已空出的旧位置。
        let locations: HashSet<_> = before.iter().chain(&after).map(|f| (&f.sid, &f.path)).collect();
        let mut actual = c.prepare(&format!("{SELECT_FILES} WHERE f.source_id=? AND f.path=?"))?;
        let expected: std::collections::HashMap<_, _> = after.iter().map(|f| ((&f.sid, &f.path), f)).collect();
        for (s, p) in locations {
            let row = actual.query_row(params![s, p], FileRow::read).optional()?;
            if row.as_ref() != expected.get(&(s, p)).copied() {
                return Err(changed());
            }
        }
        for f in &after {
            f.detach(c)?;
        }
        for f in &before {
            f.attach(c)?;
        }
    }
    c.execute("UPDATE folder_changes SET state='undone' WHERE token=?", [token])?;
    Ok(old)
}

/// library.json 已恢复旧路径而源库事务中断时，同步在检查旧磁盘是否在线之前完成撤销。
pub(super) fn resume_undo(source: &Value, library: &Library) -> Result<()> {
    let Some(token) = source["folder_undo"].as_str() else { return Ok(()) };
    let mut c = writer(library)?;
    let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let root: Option<String> = tx
        .query_row("SELECT root FROM folder_changes WHERE token=? AND state<>'undone'", [token], |r| r.get(0))
        .optional()?;
    if let Some(root) = root {
        undo(&tx, source["id"].as_str().ok_or_else(changed)?, token, &root)?;
    }
    tx.commit()?;
    Ok(())
}

/// 遍历同一哈希在文件账目里只有两个位置的组。旧位置须确实缺失，权限或其他读取错误不能说明原件已搬走。
pub(super) fn recognize(library: &Library, source: &Value, confirmation: Option<&str>) -> Result<Value> {
    let sid = source["id"].as_str().ok_or_else(changed)?;
    let root = source["path"].as_str().ok_or_else(changed)?;
    let mut c = writer(library)?;
    let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let pairs = {
        let mut query = tx.prepare(&format!(
            "{SELECT_FILES} WHERE f.md5 IN
            (SELECT md5 FROM files WHERE md5 IN (SELECT md5 FROM files WHERE source_id=?)
             AND md5<>'' GROUP BY md5 HAVING count(*)=2) ORDER BY f.md5,f.source_id,f.path"
        ))?;
        let rows = query.query_map([sid], FileRow::read)?.collect::<rusqlite::Result<Vec<_>>>()?;
        let mut pairs = Vec::new();
        for group in rows.chunk_by(|a, b| a.hash == b.hash) {
            let [a, b] = group else { continue };
            let (new, old) = if a.sid == sid { (a, b) } else { (b, a) };
            if old.sid == sid
                || new.id == old.id
                || new.trashed != 0
                || !Path::new(&new.full).is_file()
                || !std::fs::metadata(&old.full).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
            {
                continue;
            }
            if Path::new(root).join(&new.path) != Path::new(&new.full) {
                continue;
            }
            pairs.push((old.clone(), new.clone()));
        }
        pairs
    };
    let digest = crate::strict_json::sha256_hex(encode(&pairs).as_bytes());
    let Some(expected) = confirmation else {
        let files: Vec<_> = pairs.iter().map(|(old, new)| json!({"old":old.full,"new":new.full})).collect();
        return Ok(json!({"files":files,"digest":digest}));
    };
    if expected != digest || pairs.is_empty() {
        return Err(changed());
    }
    crate::app_folder::ensure_links(&tx)?;
    tx.execute_batch(HISTORY)?;
    let mut before = Vec::new();
    let mut after = Vec::new();
    for (old, new) in pairs {
        if crate::platform_folder::hash(Path::new(&new.full), &|| Ok(()))? != old.hash {
            return Err(changed());
        }
        old.detach(&tx)?;
        new.detach(&tx)?;
        let linked = FileRow { id: old.id.clone(), ..new.clone() };
        linked.attach(&tx)?;
        before.extend([old, new]);
        after.push(linked);
    }
    let token = crate::store::new_key()?;
    tx.execute(
        "INSERT INTO folder_changes(token,sid,root,state,before_files,after_files,kept,added)
        VALUES (?,?,?,'done',?,?,?,0)",
        params![token, sid, root, encode(&before), encode(&after), after.len()],
    )?;
    tx.commit()?;
    Ok(json!({"kept":after.len(),"token":token}))
}
