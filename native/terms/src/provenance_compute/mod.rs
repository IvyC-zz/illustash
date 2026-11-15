//! 原生本地溯源计算。安装版的 macOS/Windows 适配器执行全部本地步骤
//! （pixel_hash、iqdb 签名、local_match、verify、groups）。
//! OCR 计算已删除（#177），已存的证据仍可读取。
//! 授权、Task 清理、快照回执和两个图库的发布仍由 App 负责。
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
        use crate::diagnostics::logs;
        let text = fs::read_to_string(path).inspect_err(|e| logs::worker_io(logs::IoPhase::ReadRequest, e))?;
        let value =
            crate::strict_json::parse(&text).inspect_err(|_| logs::worker_invalid_request()).map_err(Error::Invalid)?;
        let request: Self = serde_json::from_value(value)
            .inspect_err(|_| logs::worker_invalid_fields())
            .map_err(|e| Error::Invalid(e.to_string()))?;
        if request.format != "illustash-provenance/1" || !request.identity.is_object() {
            logs::worker_invalid_fields();
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
/// 安装版适配器原生执行的步骤。
pub fn handles(step: &str) -> bool {
    matches!(step, "pixel_hash" | "iqdb" | "local_match" | "verify" | "groups")
}

/// 安装版就绪检查：IQDB 签名程序和固定版本的色彩引擎。
pub fn check() -> Result<()> {
    iqdb::signature_program().ok_or_else(|| Error::Invalid("未找到本地 IQDB 签名计算程序".into()))?;
    pixel_hash::lcms_version()?;
    Ok(())
}

/// 任务所属进程必须仍是我们的父进程；否则在动缓存之前停下。
pub(crate) fn check_owner(owner_pid: u32) -> Result<()> {
    #[cfg(any(unix, windows))]
    if illustash_platform::platform_process::parent_pid()? != owner_pid {
        return Err(Error::Conflict("本地计算所属任务进程已退出".into()));
    }
    let _ = owner_pid;
    Ok(())
}

/// 本次运行的机器缓存暂存副本（`request.cache`）：一个临时目录，运行成功后宿主才导入。事务边界不变；
/// `synchronous=NORMAL`（WAL）只是省掉每次提交时的刷盘，而临时副本因崩溃丢失本来就无所谓。否则几千次逐条匹配的提交
/// 每次都要等刷盘，在负载高的 Windows 磁盘上超过 30 秒（#143）。
pub(crate) fn staged_cache(dir: &Path) -> Result<rusqlite::Connection> {
    let c = crate::app_provenance::cache::connect(&dir.join("provenance.sqlite"))?;
    c.execute_batch("PRAGMA synchronous=NORMAL")?;
    Ok(c)
}

/// 安装版适配器和显式 example 的请求入口。
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
