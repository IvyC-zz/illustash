//! 滚动日志只接受固定来源、原因、错误码和已核实的源码位置。原始 stderr 不进入写入器。
use super::{create_private_directory, lock_directory, private_options, Cause};
use details::PanicSite;
pub(crate) use details::{worker_invalid_fields, worker_ort, worker_runtime_abi, OrtPhase};
pub use details::{worker_invalid_request, worker_io, IoPhase};
use std::{
    fs,
    io::{self, BufRead, BufReader, Read, Write},
    panic,
    path::{Path, PathBuf},
    process::{Command, ExitStatus},
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        mpsc::{self, SyncSender, TrySendError},
        Arc, Once, OnceLock,
    },
    thread,
    time::{Duration, Instant},
};

mod details;

const MAX_BYTES: u64 = 2 * 1024 * 1024;
const MAX_LINE: usize = 4096;
const QUEUE_SIZE: usize = 256;
const STDERR_LIMIT: usize = QUEUE_SIZE / 2;
const WRITE_BATCH: usize = 32;
const LOCK_WAIT: Duration = Duration::from_millis(100);
const DEDUP_WINDOW: Duration = Duration::from_secs(1);
static DIAGNOSTICS: OnceLock<PathBuf> = OnceLock::new();
static WRITER: OnceLock<Option<Writer>> = OnceLock::new();
static WORKER_SOURCE: OnceLock<Source> = OnceLock::new();
static PANICS: Once = Once::new();

/// 桌面外壳启动宿主前传入的绝对诊断根。只影响滚动日志。
pub const DESKTOP_ROOT_ENV: &str = "ILLUSTASH_DESKTOP_LOG_ROOT";
const WORKER_ROOT_ENV: &str = "ILLUSTASH_WORKER_LOG_ROOT";

pub(super) fn initialize(diagnostics: &Path, desktop_child: bool) {
    let inherited = if desktop_child { std::env::var_os(DESKTOP_ROOT_ENV).map(PathBuf::from) } else { None };
    let root = inherited.filter(|path| path.is_absolute()).or_else(|| std::path::absolute(diagnostics).ok());
    if let Some(root) = root {
        DIAGNOSTICS.get_or_init(|| root);
        capture_panics(Source::Host);
    }
}

pub(crate) fn prepare_worker(command: &mut Command) {
    if let Some(root) = root() {
        command.env(WORKER_ROOT_ENV, root);
    } else {
        command.env_remove(WORKER_ROOT_ENV);
    }
}

/// 由本应用启动的适配器直接写同一日志目录。不从 stderr 接收程序错误码或 panic 位置。
pub fn initialize_worker(kind: &str, checking: bool) {
    let Some(root) = std::env::var_os(WORKER_ROOT_ENV).map(PathBuf::from).filter(|path| path.is_absolute()) else {
        return;
    };
    let source = Source::for_worker(kind, checking);
    DIAGNOSTICS.get_or_init(|| root);
    WORKER_SOURCE.get_or_init(|| source);
    capture_panics(source);
}

fn capture_panics(source: Source) {
    PANICS.call_once(|| {
        let original = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            if let Some(site) = info.location().and_then(details::panic_site) {
                record(source, "Rust 进程崩溃位置", None, Kind::Panic(site));
                flush();
            }
            original(info);
        }));
    });
}

/// 桌面外壳把这个绝对路径传给宿主，避免相对应用数据目录受工作目录切换影响。
pub fn root() -> Option<&'static Path> {
    DIAGNOSTICS.get().map(PathBuf::as_path)
}

/// 建好并返回应用自己的共享日志目录；调用方不提供路径。
pub fn directory() -> io::Result<PathBuf> {
    let parent = root().ok_or_else(|| io::Error::other("diagnostics not initialized"))?;
    create_private_directory(parent)?;
    let path = parent.join("logs");
    create_private_directory(&path)?;
    Ok(path)
}

#[derive(Clone, Copy)]
pub enum Source {
    Host,
    Recognition,
    RecognitionCheck,
    Index,
    Research,
    Provenance,
    Worker,
}
impl Source {
    pub(crate) fn worker(command: &Command) -> Self {
        let kind = command
            .get_envs()
            .find(|(name, _)| *name == crate::app_tasks::WORKER_KIND_ENV)
            .and_then(|(_, value)| value)
            .and_then(|value| value.to_str());
        Self::for_worker(kind.unwrap_or_default(), command.get_args().eq(["--check"]))
    }
    fn for_worker(kind: &str, checking: bool) -> Self {
        match kind {
            "recognition" if checking => Self::RecognitionCheck,
            "recognition" => Self::Recognition,
            "index" => Self::Index,
            "research" => Self::Research,
            "provenance" => Self::Provenance,
            _ => Self::Worker,
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Host => "host",
            Self::Recognition => "recognition",
            Self::RecognitionCheck => "recognition/check",
            Self::Index => "index",
            Self::Research => "research",
            Self::Provenance => "provenance",
            Self::Worker => "worker",
        }
    }
    pub(crate) fn worker_exit(self, status: ExitStatus, failure: Option<&'static str>) {
        if !status.success() {
            #[cfg(unix)]
            use std::os::unix::process::ExitStatusExt;
            let code = status.code();
            #[cfg(unix)]
            let code = code.or_else(|| status.signal().map(|signal| -signal));
            record(self, "worker 异常退出", code, Kind::WorkerExit(failure));
        }
    }
    pub(crate) fn worker_error(self, error: &crate::Error) {
        let (cause, code) = super::classify(error);
        record(self, cause_text(cause), code, Kind::Status);
    }
    pub(crate) fn worker_timeout(self) {
        record(self, "计算任务超时", None, Kind::Status);
    }
}
pub(super) fn host_failure(cause: Cause, code: Option<i32>) {
    record(Source::Host, cause_text(cause), code, Kind::Status);
}

fn cause_text(cause: Cause) -> &'static str {
    match cause {
        Cause::HostStartup => "本地服务启动失败",
        Cause::HostExit => "本地服务异常退出",
        Cause::Timeout => "等待服务或计算超时",
        Cause::PermissionDenied => "系统拒绝访问",
        Cause::NotFound => "系统找不到文件或程序",
        Cause::StorageFull => "磁盘空间不足",
        Cause::Io => "系统读写或进程操作失败",
        Cause::Database => "数据库操作失败",
        Cause::Schema => "数据库版本不兼容",
        Cause::Network => "网络请求失败",
        Cause::Conflict => "操作与当前状态冲突",
        Cause::InvalidInput => "输入或请求无效",
        Cause::Panic => "进程内部崩溃",
        Cause::Background => "后台操作失败",
    }
}

enum Kind {
    Stderr,
    Status,
    WorkerExit(Option<&'static str>),
    WorkerDetail(&'static str),
    Panic(PanicSite),
}

fn record(source: Source, reason: &'static str, code: Option<i32>, kind: Kind) {
    if root().is_none() {
        return;
    }
    let Some(writer) = WRITER.get_or_init(|| Writer::start().ok()) else { return };
    let stderr = matches!(kind, Kind::Stderr);
    // 给超时和退出等终态保留队列空间。任何一个 stderr 流刷屏都不能占满它。
    if stderr && writer.stderr.fetch_add(1, Ordering::Relaxed) >= STDERR_LIMIT {
        writer.stderr.fetch_sub(1, Ordering::Relaxed);
        writer.dropped.fetch_add(1, Ordering::Relaxed);
        return;
    }
    let time = chrono::Utc::now();
    let stamp = time.format("%Y-%m-%dT%H:%M:%SZ");
    let code = code.map(|code| format!("（代码 {code}）")).unwrap_or_default();
    let detail = match kind {
        Kind::WorkerExit(Some(failure)) => format!("：{failure}"),
        Kind::WorkerDetail(kind) => format!("：{kind}"),
        Kind::Panic(site) => format!("（native/terms/{}:{}:{}）", site.file, site.line, site.column),
        _ => String::new(),
    };
    let line = format!("{stamp} [{}] {reason}{code}{detail}\n", source.label());
    if writer.queue.try_send(Message::Line { line, stderr }).is_err() {
        if stderr {
            writer.stderr.fetch_sub(1, Ordering::Relaxed);
        }
        writer.dropped.fetch_add(1, Ordering::Relaxed);
    }
}

enum Message {
    Line { line: String, stderr: bool },
    Flush(SyncSender<()>),
}
struct Writer {
    queue: SyncSender<Message>,
    dropped: Arc<AtomicU64>,
    stderr: Arc<AtomicUsize>,
}
impl Writer {
    fn start() -> io::Result<Self> {
        let (queue, pending) = mpsc::sync_channel(QUEUE_SIZE);
        let dropped = Arc::new(AtomicU64::new(0));
        let stderr = Arc::new(AtomicUsize::new(0));
        let lost = dropped.clone();
        let queued_stderr = stderr.clone();
        thread::Builder::new().name("diagnostic-log".into()).spawn(move || {
            while let Ok(first) = pending.recv() {
                let mut batch = String::new();
                let mut entries = 0;
                for message in std::iter::once(first).chain(pending.try_iter().take(WRITE_BATCH - 1)) {
                    match message {
                        Message::Line { line, stderr } => {
                            if stderr {
                                queued_stderr.fetch_sub(1, Ordering::Relaxed);
                            }
                            batch.push_str(&line);
                            entries += 1;
                        }
                        Message::Flush(done) => {
                            write_pending(&lost, &batch, entries);
                            batch.clear();
                            entries = 0;
                            let _ = done.send(());
                        }
                    }
                }
                if entries > 0 {
                    write_pending(&lost, &batch, entries);
                }
            }
        })?;
        Ok(Self { queue, dropped, stderr })
    }
}

fn write_pending(lost: &AtomicU64, batch: &str, entries: u64) {
    let count = lost.swap(0, Ordering::Relaxed);
    if count == 0 && entries == 0 {
        return;
    }
    let mut entry = String::new();
    if count > 0 {
        let time = chrono::Utc::now();
        let stamp = time.format("%Y-%m-%dT%H:%M:%SZ");
        entry = format!("{stamp} [log] 有 {count} 条日志未能写入\n");
    }
    entry.push_str(batch);
    if directory().and_then(|dir| append(&dir, entry.as_bytes(), MAX_BYTES)).is_err() {
        lost.fetch_add(count + entries, Ordering::Relaxed);
    }
}

/// 宿主和外壳正常退出时，给已排队的日志一个短暂的写入窗口。
pub fn flush() {
    let Some(Some(writer)) = WRITER.get() else { return };
    let (done, flushed) = mpsc::sync_channel(1);
    let until = Instant::now() + Duration::from_millis(300);
    let mut marker = Message::Flush(done);
    loop {
        match writer.queue.try_send(marker) {
            Ok(()) => {
                let _ = flushed.recv_timeout(until.saturating_duration_since(Instant::now()));
                break;
            }
            Err(TrySendError::Full(pending)) if Instant::now() < until => {
                marker = pending;
                thread::sleep(Duration::from_millis(2));
            }
            Err(_) => break,
        }
    }
}

fn append(directory: &Path, bytes: &[u8], max_bytes: u64) -> io::Result<()> {
    let until = Instant::now() + LOCK_WAIT;
    let _lock = loop {
        match lock_directory(directory) {
            Ok(lock) => break lock,
            Err(error) if conflict(&error) && Instant::now() < until => thread::sleep(Duration::from_millis(2)),
            Err(error) => return Err(error),
        }
    };
    let active = directory.join("runtime.log");
    let pending = directory.join(".runtime.pending");
    if pending.try_exists()? {
        finish_rotation(directory, &pending)?;
    }
    let mut options = private_options();
    options.append(true).create(true);
    let mut file = options.open(&active)?;
    if file.metadata()?.len().saturating_add(bytes.len() as u64) > max_bytes {
        drop(file);
        // Windows 读者可能打开 active 且拒绝重命名。先移走 active，成功后才动历史。
        fs::rename(&active, &pending)?;
        finish_rotation(directory, &pending)?;
        file = options.open(active)?;
    }
    file.write_all(bytes)
}

fn finish_rotation(directory: &Path, pending: &Path) -> io::Result<()> {
    let previous = directory.join("runtime.1.log");
    // 替换第二代是原子重命名。如果第一代已经移动（上次中断），第二代原样保留。
    ignore_missing(fs::rename(&previous, directory.join("runtime.2.log")))?;
    fs::rename(pending, previous)
}

fn conflict(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::WouldBlock || (cfg!(windows) && error.raw_os_error() == Some(33))
}

fn ignore_missing(result: io::Result<()>) -> io::Result<()> {
    match result {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// 逐行排空 stderr。每行最多保留 4 KiB 供判断；超长行的余部仍从管道读走。
/// 回调只供启动计时和自检界面使用，收到的是内存中的原文，不能拿去落盘。
/// 返回 worker 适配器报告的固定失败原因，供异常退出记录合并保留。
pub fn drain(source: Source, stderr: impl Read, mut on_line: impl FnMut(&[u8])) -> Option<&'static str> {
    let mut reader = BufReader::new(stderr);
    let mut line = Vec::new();
    let mut previous = None;
    let mut failure = None;
    while let Ok(chunk) = reader.fill_buf() {
        let end = chunk.iter().position(|&byte| byte == b'\n');
        let consumed = end.map_or(chunk.len(), |offset| offset + 1);
        let text_length = end.unwrap_or(consumed).min(MAX_LINE - line.len());
        line.extend_from_slice(&chunk[..text_length]);
        if end.is_some() || (consumed == 0 && !line.is_empty()) {
            let text = String::from_utf8_lossy(&line);
            on_line(&line);
            match classify_stderr(&text) {
                Some(reason) => {
                    if text.trim_start().starts_with(crate::app_tasks::WORKER_ERROR_PREFIX)
                        || text.contains("panicked at")
                    {
                        failure = Some(reason);
                    }
                    let now = Instant::now();
                    if previous.is_none_or(|(last, at)| last != reason || now.duration_since(at) >= DEDUP_WINDOW) {
                        record(source, reason, None, Kind::Stderr);
                        previous = Some((reason, now));
                    }
                }
                None => previous = None,
            }
            line.clear();
        }
        reader.consume(consumed);
        if consumed == 0 {
            break;
        }
    }
    failure
}

fn classify_stderr(line: &str) -> Option<&'static str> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    // 匹配发出方使用的固定片段，只返回本表中的文案。路径、账号、第三方异常正文等动态部分一律舍弃。
    let rules = [
        ("识别加速：缓存键", "识别加速缓存键生成失败"),
        ("识别加速：缓存目录", "识别加速缓存目录不可用"),
        ("改用 CPU", "识别加速不可用，已回退到 CPU"),
        ("原生识别 运行库", "无法加载 ONNX Runtime"),
        ("模型不匹配", "识别模型不匹配"),
        ("输入/输出数量不匹配", "识别模型输入或输出不匹配"),
        ("模型/锚点", "无法读取识别模型或锚点"),
        ("research OpenBLAS", "无法使用指定的 OpenBLAS 运行库"),
        ("识别 identity:", "识别请求身份格式不正确"),
        ("索引 identity:", "索引请求身份格式不正确"),
        ("IQDB signature calculation failed", "IQDB 签名计算失败"),
        ("可选 IQDB 程序不可用", "无法启动 IQDB 签名程序"),
        ("原图路径缺失", "计算缺少原图"),
        ("panicked at", "进程内部崩溃"),
        ("Permission denied", "系统拒绝访问"),
        ("Access is denied", "系统拒绝访问"),
        ("No such file or directory", "系统找不到文件或程序"),
        ("No space left on device", "磁盘空间不足"),
        ("数据库版本", "数据库版本不兼容"),
        ("图库数据库损坏", "图库数据库损坏"),
        ("数据库错误", "数据库操作失败"),
    ];
    if let Some((_, reason)) = rules.iter().find(|(fragment, _)| line.contains(fragment)) {
        return Some(reason);
    }
    let worker = line.strip_prefix(crate::app_tasks::WORKER_ERROR_PREFIX);
    let line = worker.unwrap_or(line);
    let prefixes = [
        ("HTTP native:", "本地服务请求失败"),
        ("HTTP image:", "本地服务读取图片失败"),
        ("HTTP response:", "本地服务回送响应失败"),
        ("HTTP i18n:", "本地服务加载语言表失败"),
        ("同步失败", "同步任务失败"),
        ("名称获取失败", "名称获取失败"),
        ("models:", "模型下载连接或校验异常"),
        ("导入失败", "导入失败"),
        ("词条表升级已提交", "词条表升级完成，暂未回收旧空间"),
        ("不合法：", "计算请求或输入数据无效"),
        ("冲突：", "计算输入或任务状态冲突"),
        ("读取失败：", "计算读取失败"),
        ("文件操作失败", "计算文件操作失败"),
        ("原生识别 ", "原生识别模型加载或推理失败"),
    ];
    Some(prefixes.iter().find_map(|(prefix, reason)| line.starts_with(prefix).then_some(*reason)).unwrap_or(
        if worker.is_some() { "worker 无法执行计算" } else { "进程报告了未分类的错误或警告" },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "由 concurrent_processes_keep_all_normal_writes 通过隔离进程运行"]
    fn writer_fixture() {
        super::super::initialize();
        let first: i32 = std::env::var("ILLUSTASH_TEST_LOG_OFFSET").unwrap().parse().unwrap();
        for code in first..first + 32 {
            host_failure(Cause::HostExit, Some(code));
            thread::sleep(Duration::from_millis(1));
        }
        flush();
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn concurrent_processes_keep_all_normal_writes() {
        let root = std::env::temp_dir().join(format!("illustash-log-contention-{}", crate::store::new_key().unwrap()));
        fs::create_dir(&root).unwrap();
        let mut processes = Vec::new();
        for first in [100, 200, 300] {
            processes.push(
                Command::new(std::env::current_exe().unwrap())
                    .args(["--ignored", "--exact", "diagnostics::logs::tests::writer_fixture"])
                    .env("HOME", &root)
                    .env("USERPROFILE", &root)
                    .env("ILLUSTASH_APP_SUPPORT", &root)
                    .env("ILLUSTASH_TEST_LOG_OFFSET", first.to_string())
                    .stdout(std::process::Stdio::null())
                    .spawn()
                    .unwrap(),
            );
        }
        for process in &mut processes {
            assert!(process.wait().unwrap().success());
        }
        let contents = fs::read_to_string(root.join("diagnostics/logs/runtime.log")).unwrap();
        fs::remove_dir_all(root).unwrap();
        assert_eq!(contents.lines().count(), 96, "{contents}");
        for first in [100, 200, 300] {
            for code in first..first + 32 {
                assert!(
                    contents.contains(&format!("本地服务异常退出（代码 {code}）")),
                    "缺少合成退出码 {code}：\n{contents}"
                );
            }
        }
    }

    #[cfg(any(unix, windows))]
    #[test]
    #[ignore = "由 saturated_queue_recovers_after_a_bounded_flush 通过隔离进程运行"]
    fn saturation_fixture() {
        #[cfg(unix)]
        use std::os::unix::process::ExitStatusExt;
        #[cfg(windows)]
        use std::os::windows::process::ExitStatusExt;

        super::super::initialize();
        let directory = directory().unwrap();
        let lock = lock_directory(&directory).unwrap();
        let writer = WRITER.get_or_init(|| Writer::start().ok()).as_ref().unwrap();
        let warning = "识别加速：CCIP 改用 CPU\n其他告警\n";
        let mut spam = warning.repeat(QUEUE_SIZE);
        spam.push_str("可选 worker 不可用：不合法：原生识别 运行库 synthetic-runtime");
        let failure = drain(Source::Recognition, spam.as_bytes(), |_| {});
        assert!(writer.dropped.load(Ordering::Relaxed) > 0);
        let since = Instant::now();
        flush();
        // 目录锁一直不放，flush 只能靠自身 300 ms 的上限返回；这里只防挂死（#508）。
        assert!(since.elapsed() < crate::test_wait::limit());
        assert!(!directory.join("runtime.log").exists());
        Source::Recognition.worker_exit(ExitStatus::from_raw(if cfg!(unix) { 7 << 8 } else { 7 }), failure);
        drop(lock);

        flush();
        let text = fs::read_to_string(directory.join("runtime.log")).unwrap();
        assert!(text.contains("[recognition] worker 异常退出（代码 7）：无法加载 ONNX Runtime"), "{text}");
        assert!(text.contains("[log] 有 ") && text.contains(" 条日志未能写入"), "{text}");
        assert!(!text.contains("synthetic-runtime"));

        // 最后一条被丢弃后，再也没有业务消息时 flush 也会记录这一条。
        writer.dropped.store(1, Ordering::Relaxed);
        flush();
        let text = fs::read_to_string(directory.join("runtime.log")).unwrap();
        assert!(text.lines().last().unwrap().ends_with("[log] 有 1 条日志未能写入"));
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn saturated_queue_recovers_after_a_bounded_flush() {
        let root = std::env::temp_dir().join(format!("illustash-log-overflow-{}", crate::store::new_key().unwrap()));
        fs::create_dir(&root).unwrap();
        let result = Command::new(std::env::current_exe().unwrap())
            .args(["--ignored", "--exact", "diagnostics::logs::tests::saturation_fixture", "--nocapture"])
            .env("HOME", &root)
            .env("USERPROFILE", &root)
            .env("ILLUSTASH_APP_SUPPORT", &root)
            .output()
            .unwrap();
        fs::remove_dir_all(root).unwrap();
        assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    }

    #[test]
    fn rotation_keeps_three_bounded_files_and_writes_whole_records() {
        let directory = std::env::temp_dir().join(format!("illustash-log-{}", crate::store::new_key().unwrap()));
        let directory = directory.as_path();
        create_private_directory(directory).unwrap();
        for line in [b"aaa\n", b"bbb\n", b"ccc\n", b"ddd\n", b"eee\n", b"fff\n", b"ggg\n"] {
            append(directory, line, 8).unwrap();
        }
        assert_eq!(fs::read(directory.join("runtime.log")).unwrap(), b"ggg\n");
        assert_eq!(fs::read(directory.join("runtime.1.log")).unwrap(), b"eee\nfff\n");
        assert_eq!(fs::read(directory.join("runtime.2.log")).unwrap(), b"ccc\nddd\n");
        assert!(!directory.join("runtime.3.log").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(directory).unwrap().permissions().mode() & 0o777, 0o700);
            assert_eq!(fs::metadata(directory.join("runtime.log")).unwrap().permissions().mode() & 0o777, 0o600);
        }
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn rotation_resumes_after_first_archive_has_moved() {
        let directory = std::env::temp_dir().join(format!("illustash-log-resume-{}", crate::store::new_key().unwrap()));
        create_private_directory(&directory).unwrap();
        fs::write(directory.join(".runtime.pending"), b"recover\n").unwrap();
        fs::write(directory.join("runtime.2.log"), b"history\n").unwrap();
        append(&directory, b"latest\n", 8).unwrap();
        assert_eq!(fs::read(directory.join("runtime.log")).unwrap(), b"latest\n");
        assert_eq!(fs::read(directory.join("runtime.1.log")).unwrap(), b"recover\n");
        assert_eq!(fs::read(directory.join("runtime.2.log")).unwrap(), b"history\n");
        assert!(!directory.join(".runtime.pending").exists());
        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn rotation_preserves_both_archives_while_active_file_disallows_rename() {
        use std::os::windows::fs::OpenOptionsExt;

        let directory = std::env::temp_dir().join(format!("illustash-log-share-{}", crate::store::new_key().unwrap()));
        create_private_directory(&directory).unwrap();
        let filenames = ["runtime.log", "runtime.1.log", "runtime.2.log"];
        let original = [b"current\n", b"first  \n", b"second \n"];
        for (name, content) in filenames.into_iter().zip(original) {
            fs::write(directory.join(name), content).unwrap();
        }
        let reader = fs::OpenOptions::new().read(true).share_mode(3).open(directory.join("runtime.log")).unwrap();
        for _ in 0..3 {
            assert_eq!(append(&directory, b"latest\n", 8).unwrap_err().raw_os_error(), Some(32));
            for (name, content) in filenames.into_iter().zip(original) {
                assert_eq!(&fs::read(directory.join(name)).unwrap(), content);
            }
        }
        drop(reader);
        append(&directory, b"latest\n", 8).unwrap();
        for (name, content) in filenames.into_iter().zip([b"latest\n".as_slice(), original[0], original[1]]) {
            assert_eq!(fs::read(directory.join(name)).unwrap(), content);
        }
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    #[ignore = "由 desktop_and_host_share_logs_from_different_working_directories 隔离启动"]
    fn desktop_root_fixture() {
        super::super::initialize();
        if std::env::var_os(DESKTOP_ROOT_ENV).is_some() {
            host_failure(Cause::HostExit, Some(22));
            flush();
            return;
        }
        host_failure(Cause::HostStartup, Some(11));
        flush();
        let status = Command::new(std::env::current_exe().unwrap())
            .args(["--ignored", "--exact", "diagnostics::logs::tests::desktop_root_fixture"])
            .current_dir("resources")
            .env("ILLUSTASH_DESKTOP_STDIN", "1")
            .env(DESKTOP_ROOT_ENV, root().unwrap())
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn desktop_and_host_share_logs_from_different_working_directories() {
        let root = std::env::temp_dir().join(format!("illustash-logs-cwd-{}", crate::store::new_key().unwrap()));
        for support in ["", "relative support"] {
            let current = root.join(if support.is_empty() { "empty" } else { "relative" });
            fs::create_dir_all(current.join("resources")).unwrap();
            let result = Command::new(std::env::current_exe().unwrap())
                .args(["--ignored", "--exact", "diagnostics::logs::tests::desktop_root_fixture"])
                .current_dir(&current)
                .env("HOME", &current)
                .env("USERPROFILE", &current)
                .env("ILLUSTASH_APP_SUPPORT", support)
                .env_remove("ILLUSTASH_DESKTOP_STDIN")
                .env_remove(DESKTOP_ROOT_ENV)
                .output()
                .unwrap();
            assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stdout));
            let log_path = Path::new(support).join("diagnostics/logs/runtime.log");
            let text = fs::read_to_string(current.join(&log_path)).unwrap();
            assert!(text.contains("本地服务启动失败（代码 11）"));
            assert!(text.contains("本地服务异常退出（代码 22）"));
            assert!(!current.join("resources").join(log_path).exists());
        }
        fs::remove_dir_all(root).unwrap();
    }
}
