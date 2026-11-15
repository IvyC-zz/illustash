//! 人工结论文件的内容核验与宿主事务；不调用识别器，不改变资产编号。
use crate::{Error, Result};
use illustash_core::human_conclusions::{
    export,
    format::{self, Header, Policy, Snapshot},
    import::{self, Report},
    matching::{Content, SourceHash, TargetAsset, TargetHash},
    revision,
};
use rusqlite::{Connection, OpenFlags, TransactionBehavior};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{BufReader, Read},
    path::{Path, PathBuf},
    time::SystemTime,
};

mod backups;
mod exports;
mod files;
mod hashes;
mod reports;
pub(crate) use backups::Backups;
pub(crate) use exports::Exports;
use hashes::{bindings, observe, verify, Mode};

/// 在同一个数据库读快照内导出人工状态并读取关联图片的完整 SHA-256。
/// 缺失或不可读取的图片保留记录且报告 missing，不使用路径、文件名或资产 ID 猜测内容。
pub fn capture(db: &Path, library_id: &str, check: &dyn Fn() -> Result<()>) -> Result<Snapshot> {
    Ok(capture_mode(db, library_id, Mode::Fill, check)?.snapshot)
}

struct Captured {
    snapshot: Snapshot,
    revision: i64,
}

fn capture_mode(db: &Path, library_id: &str, mode: Mode, check: &dyn Fn() -> Result<()>) -> Result<Captured> {
    capture_guarded(db, library_id, mode, check, None)
}
fn capture_guarded(
    db: &Path,
    library_id: &str,
    mode: Mode,
    check: &dyn Fn() -> Result<()>,
    context: Option<&crate::app_tasks::TaskContext>,
) -> Result<Captured> {
    let mut c = crate::app_library::read_connection(db)?;
    let tx = c.transaction()?;
    let captured_revision = revision::read(&tx)?;
    let policy = if crate::character_projection::policy(&tx)? { Policy::Hybrid } else { Policy::Default };
    let header = Header::new(
        crate::store::new_key()?,
        library_id.into(),
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        policy,
    );
    let snapshot = export::capture(&tx, header)?;
    let needed: std::collections::BTreeSet<_> = snapshot.asset_keys().collect();
    let mut hashes = BTreeMap::new();
    let mut removed = BTreeMap::new();
    let mut observations = Vec::new();
    let bindings = bindings(&tx)?;
    let live_keys: std::collections::BTreeSet<_> = bindings.iter().map(|b| b.asset.as_str()).collect();
    for key in &needed {
        if !live_keys.contains(key) {
            let hash = hashes::removed(&tx, key)?;
            if let Some(hash) = &hash {
                hashes.insert((*key).to_owned(), hash.clone());
            }
            removed.insert((*key).to_owned(), hash);
        }
    }
    for binding in bindings.into_iter().filter(|b| needed.contains(b.asset.as_str())) {
        let observation = observe(&tx, binding, mode, check)?;
        if let Some(hash) = observation.source_hash() {
            hashes.insert(observation.binding.asset.clone(), hash);
        }
        observations.push(observation);
    }
    let snapshot = snapshot.with_hashes(&hashes)?;
    tx.commit()?;
    drop(c);
    let mut c = crate::open(db)?;
    let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
    crate::schema::begin_writing(&tx)?;
    verify(&tx, &observations, false, check)?;
    let live = hashes::bindings(&tx)?;
    for (key, hash) in removed {
        if live.iter().any(|b| b.asset == key) || hashes::removed(&tx, &key)? != hash {
            return Err(Error::Conflict("生成快照期间历史图片来源已变化，请重试".into()));
        }
    }
    let unchanged = revision::read(&tx)? == captured_revision;
    hashes::publish(&tx, &observations)?;
    let protected = if unchanged { revision::read(&tx)? } else { captured_revision };
    check()?;
    if let Some(context) = context {
        context.receive_part(context.identity(), || crate::schema::commit_writing(tx))?;
    } else {
        crate::schema::commit_writing(tx)?;
    }
    Ok(Captured { snapshot, revision: protected })
}

/// 读取已存在的便携文件，在一次写事务内补齐可匹配的人工结论。
/// check 由会话传入，负责取消和 generation 校验；技术失败在提交前全部回滚。
pub fn import_file(db: &Path, library_id: &str, path: &Path, check: &dyn Fn() -> Result<()>) -> Result<Report> {
    Ok(import_run(db, library_id, path, check, &crate::app_imports::Silent, &crate::store::new_key()?, None)?.0)
}

pub(crate) fn import_observed(
    db: &Path,
    library_id: &str,
    path: &Path,
    progress: &crate::app_imports::Progress,
    expected: Option<(String, String)>,
) -> Result<crate::http_json::Json> {
    let id = &progress.context().identity().task_id;
    let (_, summary) =
        import_run(db, library_id, path, &|| progress.context().check(), progress, id, expected.as_ref())?;
    Ok(summary.into())
}

fn import_run(
    db: &Path,
    library_id: &str,
    path: &Path,
    check: &dyn Fn() -> Result<()>,
    observer: &dyn crate::app_imports::Observer,
    report_id: &str,
    expected: Option<&(String, String)>,
) -> Result<(Report, serde_json::Value)> {
    check()?;
    observer.stage("reading");
    let snapshot = files::read(path, check)?;
    if let Some((id, hash)) = expected {
        if snapshot.header().library_id != library_id
            || &snapshot.header().snapshot_id != id
            || snapshot.semantic_sha256()? != *hash
        {
            return Err(Error::Conflict("备份文件已改变，请刷新清单后重试".into()));
        }
    }
    let mut c = crate::sqlite::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    crate::configure(&c)?;
    crate::schema::require_current(&c).map_err(|e| match e {
        Error::Schema(m) => Error::Conflict(m),
        other => other,
    })?;
    crate::app_commands::initialized(&c)?;
    observer.stage("matching");
    let rows = bindings(&c)?;
    let mut observations = Vec::with_capacity(rows.len());
    let total = rows.len() as u64;
    for (index, binding) in rows.into_iter().enumerate() {
        observations.push(observe(&c, binding, Mode::Fill, check)?);
        observer.count(index as u64 + 1, total);
    }
    let candidates: Vec<_> = observations
        .iter()
        .map(|o| TargetAsset {
            key: o.binding.asset.clone(),
            hash: match &o.content {
                Some(content) => TargetHash::Verified(content.clone()),
                None => TargetHash::Unverified {
                    byte_length: o.stamp.as_ref().map(|s| s.bytes),
                    representation: o.binding.representation().map(str::to_owned),
                },
            },
        })
        .collect();
    let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
    observer.stage("validating");
    crate::schema::begin_writing(&tx)?;
    verify(&tx, &observations, true, check)?;
    let report = import::apply_in(&tx, &snapshot, library_id, &candidates, &mut || check())?;
    let effect = if report.action_id.is_some() {
        crate::app_curation::refresh_excluded(&tx, &report.touched)?;
        tx.execute("INSERT INTO backend_effects(kind) VALUES ('verdict')", [])?;
        Some(tx.last_insert_rowid())
    } else {
        None
    };
    verify(&tx, &observations, true, check)?;
    hashes::publish(&tx, &observations)?;
    let mut summary = reports::summary(report_id, &snapshot, &report);
    reports::save(&tx, report_id, effect, &summary, &report, check)?;
    check()?;
    observer.stage("committing");
    let mut tx = Some(tx);
    observer.publish(&mut || {
        let tx = tx.take().expect("人工导入事务只发布一次");
        verify(&tx, &observations, true, check)?;
        crate::schema::commit_writing(tx)
    })?;
    drop(tx);
    crate::app_effects::drain_after_commit(db);
    summary["pending_effects"] = serde_json::json!(reports::pending(&c, effect));
    Ok((report, summary))
}

#[cfg(test)]
mod tests;
