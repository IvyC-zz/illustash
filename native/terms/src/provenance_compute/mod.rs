//! Native local provenance computation. The installed macOS/Windows adapter runs
//! every local step (pixel_hash, iqdb signatures, local_match, verify, groups).
//! OCR computation was removed (#177); its stored evidence stays readable.
//! App retains authorization, Task cleanup, snapshot receipt and two-library publication.
mod groups;
mod iqdb;
mod local_match;
mod pixel_batch;
pub mod pixel_hash;
mod pixel_icc;
mod verify;
use crate::{Error, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Deserialize)]
pub struct Request {
    pub format: String,
    pub identity: Value,
    pub owner_pid: u32,
    pub library: PathBuf,
    pub cache: PathBuf,
    pub mailbox: PathBuf,
    pub step: String,
    pub only: Vec<String>,
    pub artists: Vec<String>,
    pub network: bool,
}
impl Request {
    pub fn read(path: &Path) -> Result<Self> {
        let value = crate::strict_json::parse(&fs::read_to_string(path)?).map_err(Error::Invalid)?;
        let request: Self = serde_json::from_value(value).map_err(|e| Error::Invalid(e.to_string()))?;
        if request.format != "illustash-provenance/1" || !request.identity.is_object() {
            return Err(Error::Invalid("溯源 worker 请求格式无效".into()));
        }
        Ok(request)
    }
    pub fn response(&self, result: Value) -> Value {
        json!({"format":self.format,"identity":self.identity,"step":self.step,"result":result})
    }
}
pub fn write(path: &Path, value: &Value) -> Result<()> {
    let temporary = path.with_extension(format!("{}.tmp", crate::store::new_key()?));
    let bytes = serde_json::to_vec(value).map_err(|e| Error::Invalid(e.to_string()))?;
    fs::write(&temporary, bytes)?;
    let result = fs::rename(&temporary, path);
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result.map_err(Error::from)
}
/// Steps the installed adapter runs natively.
pub fn handles(step: &str) -> bool {
    matches!(step, "pixel_hash" | "iqdb" | "local_match" | "verify" | "groups")
}

/// Installed readiness: the IQDB signature program and the pinned colour engine.
pub fn check() -> Result<()> {
    iqdb::signature_program().ok_or_else(|| Error::Invalid("未找到本地 IQDB 签名计算程序".into()))?;
    pixel_hash::lcms_version()?;
    Ok(())
}

/// The task owner must still be our parent; otherwise stop before touching caches.
pub(crate) fn check_owner(owner_pid: u32) -> Result<()> {
    #[cfg(any(unix, windows))]
    if illustash_platform::platform_process::parent_pid()? != owner_pid {
        return Err(Error::Conflict("本地计算所属任务进程已退出".into()));
    }
    let _ = owner_pid;
    Ok(())
}

/// The run's staged copy of the machine cache (`request.cache`): a scratch directory the host imports only after the
/// run succeeds. Transaction boundaries stay as they are; `synchronous=NORMAL` (WAL) only drops the flush on every
/// commit, which a scratch copy lost to a crash never needed. Thousands of per-match commits otherwise each wait
/// for a flush, past 30 s on a loaded Windows disk (#143).
pub(crate) fn staged_cache(dir: &Path) -> Result<rusqlite::Connection> {
    let c = crate::app_provenance::cache::connect(&dir.join("provenance.sqlite"))?;
    c.execute_batch("PRAGMA synchronous=NORMAL")?;
    Ok(c)
}

/// Request entry for the installed adapter and the explicit example.
pub fn run_files(input: &Path, output: &Path) -> Result<()> {
    let request = Request::read(input)?;
    if !handles(&request.step) {
        return Err(Error::Invalid("此步骤不由原生本地计算执行".into()));
    }
    check_owner(request.owner_pid)?;
    let mut sequence = 1u64;
    write(&request.mailbox.join("progress.json"), &json!({"log":"开始本地计算","sequence":sequence}))?;
    let result = match request.step.as_str() {
        "pixel_hash" => pixel_batch::run(&request, &mut sequence)?,
        "iqdb" => iqdb::run(&request, &mut sequence)?,
        "verify" => verify::run(&request, &mut sequence)?,
        "local_match" => local_match::run(&request, &mut sequence)?,
        _ => groups::run(&request, &mut sequence)?,
    };
    write(output, &request.response(result))
}
