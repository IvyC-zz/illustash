//! 本机运行诊断。只接受枚举和数值，不保存错误原文、请求参数、路径或 stderr。
//! 与业务数据库无关；记录失败只影响 receipt.stored，绝不改变业务 Result。
use crate::Error;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::OnceLock,
    time::{SystemTime, UNIX_EPOCH},
};

pub mod logs;

const LIMIT: usize = 128;
const DAYS: u64 = 14;
const MAX_BYTES: u64 = 2048;
static STORE: OnceLock<Store> = OnceLock::new();

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    Http,
    Library,
    Settings,
    Characters,
    Names,
    Sync,
    Index,
    Recognition,
    Archive,
    Export,
    Provenance,
    Research,
    Models,
    Accounts,
    Integrity,
    Updates,
    Host,
    Task,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Read,
    Write,
    Background,
    Startup,
    Exit,
    Publishing,
    Reading,
    Checking,
    Unpacking,
    Cleaning,
    Models,
    Verifying,
    Writing,
    Committing,
    Projecting,
    Preparing,
    Scanning,
    Recognising,
    Importing,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Publication {
    NotApplicable,
    NotCommitted,
    Committed,
    MayBePartial,
    Unknown,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Retry {
    Retry,
    AfterFix,
    CheckState,
    Restart,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Cause {
    InvalidInput,
    Conflict,
    Network,
    Database,
    Schema,
    PermissionDenied,
    NotFound,
    StorageFull,
    Io,
    Panic,
    HostExit,
    HostStartup,
    Timeout,
    Background,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    id: String,
    at: u64,
    operation: Operation,
    stage: Stage,
    cause: Cause,
    code: Option<i32>,
    publication: Publication,
    retry: Retry,
}
#[derive(Clone, Debug, Serialize)]
pub struct Receipt {
    pub id: String,
    pub stored: bool,
    pub publication: Publication,
    pub retry: Retry,
}
impl Receipt {
    pub fn value(&self) -> Value {
        serde_json::to_value(self).expect("diagnostic receipt contains only serializable fields")
    }
}
#[derive(Clone)]
struct Store {
    directory: PathBuf,
}
fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

/// 只有 HTTP 宿主和桌面外壳显式启用；库单测与离线计算不会触碰真实用户目录。
pub fn initialize() {
    if let Ok(home) = crate::app_paths::user_home() {
        let store = STORE.get_or_init(|| Store { directory: crate::app_paths::app_support(&home).join("diagnostics") });
        logs::initialize(&store.directory);
        let _ = store.read(now());
    }
}
fn private_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    options
}
fn create_private_directory(path: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)?;
    if fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err(io::Error::other("diagnostics directory is a symlink"));
    }
    Ok(())
}
fn lock_directory(path: &Path) -> io::Result<File> {
    let file = private_options().create(true).truncate(false).open(path.join(".lock"))?;
    crate::app_file_lock::acquire(&file)?;
    Ok(file)
}
impl Store {
    fn lock(&self) -> io::Result<File> {
        create_private_directory(&self.directory)?;
        lock_directory(&self.directory)
    }
    /// 在独占锁内清理。每个文件也有大小限制；坏的/过期的记录从不复制出去。
    fn retained(&self, at: u64, limit: usize) -> io::Result<Vec<Record>> {
        let mut kept = Vec::new();
        for entry in fs::read_dir(&self.directory)? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str().and_then(|s| s.strip_suffix(".json")) else { continue };
            if !valid_id(name) {
                continue;
            }
            let metadata = fs::symlink_metadata(entry.path())?;
            let record = if metadata.is_file() && metadata.len() <= MAX_BYTES {
                let mut bytes = Vec::new();
                File::open(entry.path())?.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
                serde_json::from_slice::<Record>(&bytes)
                    .ok()
                    .filter(|r| r.id == name && r.at <= at && at.saturating_sub(r.at) <= DAYS * 86400)
            } else {
                None
            };
            if let Some(record) = record {
                kept.push(record);
            } else {
                fs::remove_file(entry.path())?;
            }
        }
        kept.sort_by(|a, b| b.at.cmp(&a.at).then_with(|| b.id.cmp(&a.id)));
        for record in kept.drain(limit.min(kept.len())..) {
            fs::remove_file(self.directory.join(format!("{}.json", record.id)))?;
        }
        Ok(kept)
    }
    fn read(&self, at: u64) -> io::Result<Vec<Record>> {
        let _lock = self.lock()?;
        self.retained(at, LIMIT)
    }
    fn write(&self, record: &Record) -> io::Result<()> {
        let _lock = self.lock()?;
        self.retained(record.at, LIMIT - 1)?;
        let bytes = serde_json::to_vec(record)?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(io::Error::other("diagnostic size limit"));
        }
        let mut file = private_options().create_new(true).open(self.directory.join(format!("{}.json", record.id)))?;
        file.write_all(&bytes)?;
        file.sync_all()
    }
}
fn valid_id(s: &str) -> bool {
    s.len() == 34 && s.starts_with("D-") && s[2..].bytes().all(|c| c.is_ascii_hexdigit())
}
fn record_with(
    store: Option<&Store>,
    operation: Operation,
    stage: Stage,
    cause: Cause,
    code: Option<i32>,
    publication: Publication,
    retry: Retry,
) -> Receipt {
    let mut nonce = [0u8; 16];
    // 随机源失败仍要把业务错误返回；此时用时间、进程与进程内序号生成临时追踪编号。
    if getrandom::fill(&mut nonce).is_err() {
        use std::sync::atomic::{AtomicU32, Ordering};
        static NEXT: AtomicU32 = AtomicU32::new(0);
        nonce[..8].copy_from_slice(
            &SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos().to_le_bytes()[..8],
        );
        nonce[8..12].copy_from_slice(&std::process::id().to_le_bytes());
        nonce[12..].copy_from_slice(&NEXT.fetch_add(1, Ordering::Relaxed).to_le_bytes());
    }
    let id = format!("D-{}", nonce.iter().map(|b| format!("{b:02x}")).collect::<String>());
    let record = Record { id: id.clone(), at: now(), operation, stage, cause, code, publication, retry };
    let stored = store.is_some_and(|store| store.write(&record).is_ok());
    Receipt { id, stored, publication, retry }
}
pub fn record(
    operation: Operation,
    stage: Stage,
    cause: Cause,
    code: Option<i32>,
    publication: Publication,
    retry: Retry,
) -> Receipt {
    if matches!(operation, Operation::Host) {
        logs::host_failure(cause, code);
    }
    record_with(STORE.get(), operation, stage, cause, code, publication, retry)
}
pub fn failure(operation: Operation, stage: Stage, error: &Error, publication: Publication) -> Receipt {
    let (cause, code) = classify(error);
    let retry = match publication {
        Publication::Committed | Publication::MayBePartial | Publication::Unknown => Retry::CheckState,
        _ => match cause {
            Cause::Conflict | Cause::Network => Retry::Retry,
            _ => Retry::AfterFix,
        },
    };
    record(operation, stage, cause, code, publication, retry)
}
fn classify(error: &Error) -> (Cause, Option<i32>) {
    match error {
        Error::Sql(rusqlite::Error::SqliteFailure(code, _)) => (Cause::Database, Some(code.extended_code)),
        Error::Sql(_) => (Cause::Database, None),
        Error::Io(e) | Error::IoAt { source: e, .. } => {
            let cause = match e.kind() {
                io::ErrorKind::PermissionDenied => Cause::PermissionDenied,
                io::ErrorKind::NotFound => Cause::NotFound,
                _ if e.raw_os_error() == Some(if cfg!(windows) { 112 } else { libc::ENOSPC }) => Cause::StorageFull,
                io::ErrorKind::TimedOut => Cause::Timeout,
                _ => Cause::Io,
            };
            (cause, e.raw_os_error())
        }
        Error::Invalid(_) | Error::NeedId(_) => (Cause::InvalidInput, None),
        Error::Network(_) => (Cause::Network, None),
        Error::Conflict(_) => (Cause::Conflict, None),
        Error::Schema(_) => (Cause::Schema, None),
    }
}
/// 路由只映射固定分类，绝不保留 URL、query、用户 ID 或文件名。
pub(crate) fn operation(url: &str) -> Operation {
    match url.split('?').next().unwrap_or("").split('/').nth(2).unwrap_or("") {
        "libraries" | "library" => Operation::Library,
        "settings" | "preferences" | "about" => Operation::Settings,
        "characters" | "entities" | "verdict" | "labels" => Operation::Characters,
        "names" | "suggest" | "entity-preview" => Operation::Names,
        "sync" => Operation::Sync,
        "index" => Operation::Index,
        "recognition" => Operation::Recognition,
        "imports" => Operation::Archive,
        "provenance" | "artists" => Operation::Provenance,
        "similar" | "clusters" | "oc" | "learn" => Operation::Research,
        "models" => Operation::Models,
        "accounts" => Operation::Accounts,
        "integrity" => Operation::Integrity,
        "updates" => Operation::Updates,
        _ => Operation::Http,
    }
}
pub(crate) fn stage(value: &str) -> Stage {
    match value {
        "preparing" => Stage::Preparing,
        "scanning" => Stage::Scanning,
        "recognising" => Stage::Recognising,
        "importing" => Stage::Importing,
        "models" => Stage::Models,
        "verifying" => Stage::Verifying,
        "writing" => Stage::Writing,
        "committing" => Stage::Committing,
        "projecting" => Stage::Projecting,
        "publishing" => Stage::Publishing,
        "reading" => Stage::Reading,
        "checking" => Stage::Checking,
        "unpacking" => Stage::Unpacking,
        "cleaning" => Stage::Cleaning,
        _ => Stage::Background,
    }
}
/// 只导出白名单记录；源文件里的未知字段、无效枚举或超长字段使该条记录失效。
pub fn report() -> Value {
    let result = STORE.get().map(|s| s.read(now()));
    let available = matches!(result, Some(Ok(_)));
    let records = result.and_then(std::result::Result::ok).unwrap_or_default();
    json!({"format":1, "available":available, "retention_days":DAYS, "max_records":LIMIT,
        "version":env!("ILLUSTASH_VERSION"), "build":env!("ILLUSTASH_BUILD"),
        "os":std::env::consts::OS, "arch":std::env::consts::ARCH, "records":records})
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture(Store);
    impl Fixture {
        fn new() -> Self {
            let id = record_with(
                None,
                Operation::Http,
                Stage::Read,
                Cause::Io,
                None,
                Publication::NotApplicable,
                Retry::Retry,
            )
            .id;
            Self(Store { directory: std::env::temp_dir().join(format!("illustash-diagnostics-{id}")) })
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0.directory);
        }
    }
    #[test]
    fn retained_after_reopen_and_capped_by_count_age_and_record_size() {
        let f = Fixture::new();
        for _ in 0..LIMIT + 3 {
            assert!(
                record_with(
                    Some(&f.0),
                    Operation::Host,
                    Stage::Exit,
                    Cause::HostExit,
                    Some(9),
                    Publication::Unknown,
                    Retry::Restart
                )
                .stored
            );
        }
        let reopened = Store { directory: f.0.directory.clone() };
        assert_eq!(reopened.read(now()).unwrap().len(), LIMIT);
        assert!(reopened.read(now() + DAYS * 86400 + 1).unwrap().is_empty());
        assert_eq!(fs::read_dir(&f.0.directory).unwrap().count(), 1);
    }
    #[test]
    fn private_fields_never_enter_records_and_invalid_files_are_not_exported() {
        let f = Fixture::new();
        let err = Error::IoAt {
            source: io::Error::other("Cookie=secret /private/image.png"),
            path: "private-image.png".into(),
            destination: None,
        };
        let (cause, code) = classify(&err);
        let receipt = record_with(
            Some(&f.0),
            operation("/api/library/private-image.png?k=secret"),
            Stage::Write,
            cause,
            code,
            Publication::Unknown,
            Retry::CheckState,
        );
        let path = f.0.directory.join(format!("{}.json", receipt.id));
        let text = fs::read_to_string(&path).unwrap();
        for secret in ["secret", "Cookie", "private", "image.png"] {
            assert!(!text.contains(secret));
        }
        let mut value: Value = serde_json::from_str(&text).unwrap();
        value["raw_stderr"] = "secret".into();
        fs::write(&path, value.to_string()).unwrap();
        assert!(f.0.read(now()).unwrap().is_empty());
        fs::write(&path, vec![b'x'; MAX_BYTES as usize + 1]).unwrap();
        assert!(f.0.read(now()).unwrap().is_empty());
    }
    #[test]
    fn locked_or_unwritable_store_never_replaces_error_or_committed_outcome() {
        let f = Fixture::new();
        let lock = f.0.lock().unwrap();
        let receipt = record_with(
            Some(&f.0),
            Operation::Archive,
            Stage::Publishing,
            Cause::Io,
            None,
            Publication::Committed,
            Retry::CheckState,
        );
        assert!(!receipt.stored);
        assert!(valid_id(&receipt.id));
        assert_eq!(receipt.value()["publication"], "committed");
        drop(lock);
        let blocked = Fixture::new();
        fs::write(&blocked.0.directory, b"not a directory").unwrap();
        assert!(
            !record_with(
                Some(&blocked.0),
                Operation::Http,
                Stage::Read,
                Cause::Database,
                Some(11),
                Publication::NotApplicable,
                Retry::AfterFix
            )
            .stored
        );
        fs::remove_file(&blocked.0.directory).unwrap();
    }
}
