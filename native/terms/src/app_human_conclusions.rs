//! 人工结论文件的内容核验与宿主事务；不调用识别器，不改变资产编号。
use crate::{Error, Result};
use illustash_core::human_conclusions::{
    export,
    format::{self, Header, Policy, Snapshot},
    import::{self, Report},
    matching::{Content, SourceHash, TargetAsset, TargetHash},
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

#[derive(Clone, Debug, Eq, PartialEq)]
struct Binding {
    asset: String,
    path: Option<PathBuf>,
    kind: Option<String>,
    added_at: Option<String>,
}

impl Binding {
    fn representation(&self) -> Option<&'static str> {
        match self.kind.as_deref() {
            Some("orig") => Some("original"),
            Some("deriv") => Some("derived"),
            _ => None,
        }
    }
}

fn bindings(c: &Connection) -> Result<Vec<Binding>> {
    Ok(c.prepare("SELECT uuid,src_path,src_kind,added_at FROM assets ORDER BY uuid")?
        .query_map([], |r| {
            Ok(Binding {
                asset: r.get(0)?,
                path: r.get::<_, Option<String>>(1)?.filter(|p| !p.is_empty()).map(PathBuf::from),
                kind: r.get(2)?,
                added_at: r.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Stamp {
    bytes: u64,
    modified: SystemTime,
}

fn stamp(path: &Path) -> Option<Stamp> {
    let metadata = fs::metadata(path).ok()?;
    metadata.is_file().then(|| Some(Stamp { bytes: metadata.len(), modified: metadata.modified().ok()? }))?
}

struct Observed {
    binding: Binding,
    stamp: Option<Stamp>,
    content: Option<Content>,
}

// 不把 size/mtime 当成摘要；每次目标认回都读取全部字节，时间与大小只用于发现读取期间的变化。
fn observe(binding: Binding, check: &dyn Fn() -> Result<()>) -> Result<Observed> {
    check()?;
    let initial = binding.path.as_deref().and_then(stamp);
    let mut observed = Observed { binding, stamp: initial, content: None };
    let (Some(path), Some(before), Some(representation)) =
        (observed.binding.path.as_deref(), &observed.stamp, observed.binding.representation())
    else {
        return Ok(observed);
    };
    let Ok(mut file) = File::open(path) else { return Ok(observed) };
    let Ok(opened) = file.metadata() else { return Ok(observed) };
    if !opened.is_file() || opened.len() != before.bytes || opened.modified().ok() != Some(before.modified) {
        return Err(Error::Conflict("读取图片时内容版本发生变化".into()));
    }
    let mut hash = Sha256::new();
    let mut length = 0;
    let mut buffer = vec![0; 1 << 20];
    loop {
        check()?;
        let Ok(n) = file.read(&mut buffer) else { return Ok(observed) };
        if n == 0 {
            break;
        }
        length += n as u64;
        hash.update(&buffer[..n]);
    }
    if length != before.bytes || stamp(path).as_ref() != Some(before) {
        return Err(Error::Conflict("读取图片时内容版本发生变化".into()));
    }
    observed.content = Some(Content {
        sha256: format!("{:x}", hash.finalize()),
        byte_length: length,
        representation: representation.into(),
    });
    Ok(observed)
}

fn verify(c: &Connection, observations: &[Observed], check: &dyn Fn() -> Result<()>) -> Result<()> {
    check()?;
    if bindings(c)? != observations.iter().map(|o| o.binding.clone()).collect::<Vec<_>>() {
        return Err(Error::Conflict("核验后图片来源已变化，请重试".into()));
    }
    for observation in observations {
        check()?;
        if observation.binding.path.as_deref().and_then(stamp) != observation.stamp {
            return Err(Error::Conflict("核验后图片内容已变化，请重试".into()));
        }
    }
    Ok(())
}

/// 在同一个数据库读快照内导出人工状态并读取关联图片的完整 SHA-256。
/// 缺失或不可读取的图片保留记录且报告 missing，不使用路径、文件名或资产 ID 猜测内容。
pub fn capture(db: &Path, library_id: &str, check: &dyn Fn() -> Result<()>) -> Result<Snapshot> {
    let mut c = crate::app_library::read_connection(db)?;
    let tx = c.transaction()?;
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
    let mut observations = Vec::new();
    for binding in bindings(&tx)?.into_iter().filter(|b| needed.contains(b.asset.as_str())) {
        let observation = observe(binding, check)?;
        if let Some(content) = &observation.content {
            let version = format!(
                "{:x}",
                Sha256::digest(format!("{:?}:{:?}", observation.binding, observation.stamp).as_bytes())
            );
            hashes
                .insert(observation.binding.asset.clone(), SourceHash::Verified { content: content.clone(), version });
        }
        observations.push(observation);
    }
    for observation in observations {
        check()?;
        if observation.binding.path.as_deref().and_then(stamp) != observation.stamp {
            return Err(Error::Conflict("生成快照期间图片内容已变化，请重试".into()));
        }
    }
    check()?;
    snapshot.with_hashes(&hashes)
}

/// 读取已存在的便携文件，在一次写事务内补齐可匹配的人工结论。
/// check 由会话传入，负责取消和 generation 校验；技术失败在提交前全部回滚。
pub fn import_file(db: &Path, library_id: &str, path: &Path, check: &dyn Fn() -> Result<()>) -> Result<Report> {
    check()?;
    let snapshot = format::read(&mut BufReader::new(File::open(path)?))?;
    let mut c = crate::sqlite::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    crate::configure(&c)?;
    crate::app_commands::initialized(&c)?;
    let rows = bindings(&c)?;
    let mut observations = Vec::with_capacity(rows.len());
    for binding in rows {
        observations.push(observe(binding, check)?);
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
    crate::schema::begin_writing(&tx)?;
    verify(&tx, &observations, check)?;
    let report = import::apply_in(&tx, &snapshot, library_id, &candidates, &mut || check())?;
    if report.action_id.is_some() {
        crate::app_curation::refresh_excluded(&tx, &report.touched)?;
        tx.execute("INSERT INTO backend_effects(kind) VALUES ('verdict')", [])?;
    }
    verify(&tx, &observations, check)?;
    crate::schema::commit_writing(tx)?;
    crate::app_effects::drain_after_commit(db);
    Ok(report)
}

#[cfg(test)]
mod tests;
