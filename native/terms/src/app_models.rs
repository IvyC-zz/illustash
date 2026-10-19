//! Recognition models fetched on first use (#168, user 2026-10-16): installers ship without the WD and CCIP models;
//! the person downloads them once, or imports files they fetched themselves.
//!
//! Safety rests on the manifest built into the app (native/terms/models.json), not on where bytes come from: every
//! file's size and SHA-256 are listed, and a file that does not match is refused whichever source sent it. Sources,
//! per file and in order: the Hugging Face address through a mirror (default hf-mirror, changeable in the settings,
//! kept app-wide in preferences.json as `model_mirror`), then the ModelScope copy. No speed test.
//!
//! Location: the model root the workers use (`ILLUSTASH_MODELS`, else `<resources>/models`; app_resources). Work
//! happens beside it in `<root>.partial/`: each file is written as `<path>.part`, renamed to its own name once its
//! hash matches, and when all are there the folder is renamed to the root in one step (per file when the root
//! already exists). A cancelled, failed or interrupted download never touches the root; the next run resumes from the
//! `.part` with an HTTP Range request, or the person discards the folder.
//!
//! Network rules as everywhere else: nothing is downloaded until the person clicks; the request is a plain HTTPS GET
//! carrying no library information. HTTP proxies come from the environment, else the system settings.
use crate::{http_json::Json, Error, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex, OnceLock,
    },
    time::{Duration, Instant},
};

#[derive(Deserialize)]
pub struct Manifest {
    pub format: u32,
    /// Names the model folder in the shared data, so a later pack never mixes with this one.
    pub pack: String,
    /// The project page with manual download instructions; the app links only to it.
    pub manual: String,
    pub mirror: String,
    pub modelscope: String,
    pub files: Vec<Entry>,
}
#[derive(Deserialize, Clone)]
pub struct Entry {
    /// Under the model root, `/`-separated.
    pub path: String,
    pub size: u64,
    pub sha256: String,
    hf: Origin,
    modelscope: Origin,
}
#[derive(Deserialize, Clone)]
struct Origin {
    repo: String,
    revision: String,
    file: String,
}

/// The models this build uses.
pub fn manifest() -> &'static Manifest {
    static MANIFEST: OnceLock<Manifest> = OnceLock::new();
    MANIFEST.get_or_init(|| {
        let manifest: Manifest = serde_json::from_str(include_str!("../models.json")).expect("models.json is valid");
        assert!(manifest.format == 1 && !manifest.files.is_empty(), "models.json format");
        manifest
    })
}

/// The model root workers load from: `ILLUSTASH_MODELS` (relative to the resource root), else `<resources>/models`.
pub(crate) fn root(resources: &Path) -> PathBuf {
    configured_root(resources, std::env::var_os("ILLUSTASH_MODELS").as_deref().map(Path::new))
}
pub(crate) fn configured_root(resources: &Path, configured: Option<&Path>) -> PathBuf {
    match configured {
        Some(path) if path.is_absolute() => path.to_owned(),
        Some(path) => resources.join(path),
        None => resources.join("models"),
    }
}

fn has(dir: &Path, entry: &Entry) -> bool {
    fs::symlink_metadata(dir.join(&entry.path)).is_ok_and(|m| m.is_file() && m.len() == entry.size)
}
/// Every model file is in place. Only verified files are ever moved into the root (see `activate`), so presence is
/// enough here; recognition still fingerprints the files it loads (recognition_compute::models). Cheap enough for
/// every status read.
fn ready_at(root: &Path, manifest: &Manifest) -> bool {
    manifest.files.iter().all(|entry| fs::metadata(root.join(&entry.path)).is_ok_and(|m| m.is_file()))
}
/// Set by the desktop shell for an installer without models (#168): recognition, the character index and the
/// self-check wait until the models are in place, saying why. Without it nothing waits (full installers, tests
/// and development runs keep their own model folders).
pub(crate) fn on_demand() -> bool {
    std::env::var_os("ILLUSTASH_MODELS_ON_DEMAND").is_some_and(|v| v == "1")
}
/// Recognition must wait for the models to be downloaded or imported.
pub(crate) fn waiting(resources: &Path) -> bool {
    on_demand() && !ready_at(&root(resources), manifest())
}
/// Why recognition cannot start yet, for the sync, index and self-check messages.
pub(crate) const MISSING: &str = "识别模型还没下载";

fn partial_dir(root: &Path) -> PathBuf {
    let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "models".into());
    root.with_file_name(format!("{name}.partial"))
}
fn part(dir: &Path, entry: &Entry) -> PathBuf {
    dir.join(format!("{}.part", entry.path))
}

// ---------- where to download from ----------

/// A source base: any HTTPS address, or plain HTTP on this machine (tests and a local mirror). No credentials,
/// query or fragment; the trailing slash is dropped.
fn base(text: &str) -> std::result::Result<String, String> {
    let u = url::Url::parse(text.trim()).map_err(|_| "地址无效".to_string())?;
    let local = matches!(u.host_str(), Some("127.0.0.1" | "localhost" | "[::1]")) && u.port().is_some();
    if !(u.scheme() == "https" && u.host_str().is_some_and(|h| !h.is_empty()) || u.scheme() == "http" && local)
        || !u.username().is_empty()
        || u.password().is_some()
        || u.query().is_some()
        || u.fragment().is_some()
    {
        return Err("镜像地址须为 https:// 开头的地址".into());
    }
    Ok(u.as_str().trim_end_matches('/').to_owned())
}
fn host(address: &str) -> String {
    url::Url::parse(address)
        .ok()
        .and_then(|u| {
            u.host_str().map(|h| match u.port() {
                Some(port) => format!("{h}:{port}"),
                None => h.to_owned(),
            })
        })
        .unwrap_or_default()
}

/// Where the state lives: preferences.json in the app, a fixture in tests.
#[derive(Clone)]
struct Place {
    root: PathBuf,
    manifest: &'static Manifest,
    preferences: PathBuf,
    /// ModelScope's base: the manifest's, or a local test server (ILLUSTASH_MODELSCOPE_BASE, plain HTTP on this
    /// machine only).
    modelscope: String,
    /// Read when the request arrives, so a proxy switched on in the system settings counts from the next click.
    proxy: Option<String>,
    /// The models are this install's to download, and so to delete (`ILLUSTASH_MODELS_ON_DEMAND`).
    on_demand: bool,
    /// The session is recognising or updating the character index: the models are in use.
    in_use: bool,
}
impl Place {
    fn app(resources: &Path) -> Result<Self> {
        let modelscope = match std::env::var("ILLUSTASH_MODELSCOPE_BASE") {
            Ok(text) => match base(&text) {
                Ok(b) if b.starts_with("http://") => b,
                _ => return Err(Error::Invalid("ModelScope 的测试地址只允许本机 HTTP 端口".into())),
            },
            Err(_) => manifest().modelscope.clone(),
        };
        Ok(Self {
            root: root(resources),
            manifest: manifest(),
            preferences: crate::app_paths::app_support(&crate::app_paths::user_home()?).join("preferences.json"),
            modelscope,
            proxy: None,
            on_demand: on_demand(),
            in_use: false,
        })
    }
    fn mirror(&self) -> String {
        crate::app_updates::read(&self.preferences)
            .get("model_mirror")
            .and_then(Value::as_str)
            .and_then(|text| base(text).ok())
            .unwrap_or_else(|| self.manifest.mirror.clone())
    }
    /// The sources for one file, in order: the Hugging Face address through the mirror, then ModelScope.
    fn sources(&self, entry: &Entry) -> [(String, String); 2] {
        let mirror = self.mirror();
        let (hf, ms) = (&entry.hf, &entry.modelscope);
        [
            (host(&mirror), format!("{mirror}/{}/resolve/{}/{}", hf.repo, hf.revision, hf.file)),
            (
                host(&self.modelscope),
                format!("{}/models/{}/resolve/{}/{}", self.modelscope, ms.repo, ms.revision, ms.file),
            ),
        ]
    }
}

// ---------- proxy ----------

/// The HTTP proxy to use: `HTTPS_PROXY` / `ALL_PROXY`, else the system's HTTPS proxy (macOS network settings,
/// Windows Internet Options). Automatic configuration (PAC) is not read: then the download goes direct.
fn proxy() -> Option<String> {
    for key in ["HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy"] {
        if let Some(value) = std::env::var(key).ok().filter(|v| !v.trim().is_empty()) {
            return Some(value.trim().to_owned());
        }
    }
    system_proxy()
}
#[cfg(target_os = "macos")]
fn system_proxy() -> Option<String> {
    let out = std::process::Command::new("/usr/sbin/scutil").arg("--proxy").output().ok()?;
    scutil_proxy(&String::from_utf8_lossy(&out.stdout))
}
#[cfg(windows)]
fn system_proxy() -> Option<String> {
    use std::os::windows::process::CommandExt;
    let out = std::process::Command::new(reg_exe(std::env::var_os("SystemRoot")))
        .args(["query", r"HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings"])
        .creation_flags(0x08000000) // CREATE_NO_WINDOW
        .output()
        .ok()?;
    windows_proxy(&String::from_utf8_lossy(&out.stdout))
}
/// `reg.exe` by its full path, never looked up on PATH: `%SystemRoot%\System32`, else `C:\Windows\System32`.
#[cfg(any(windows, test))]
fn reg_exe(system_root: Option<std::ffi::OsString>) -> PathBuf {
    let root = system_root.filter(|r| !r.is_empty()).map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    root.join("System32").join("reg.exe")
}
#[cfg(not(any(target_os = "macos", windows)))]
fn system_proxy() -> Option<String> {
    None
}
/// `scutil --proxy`: `HTTPSEnable : 1`, `HTTPSProxy : host`, `HTTPSPort : 8080`.
#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
fn scutil_proxy(text: &str) -> Option<String> {
    let field = |key: &str| {
        text.lines().find_map(|line| {
            let (k, v) = line.split_once(" : ")?;
            (k.trim() == key).then(|| v.trim().to_owned())
        })
    };
    if field("HTTPSEnable").as_deref() != Some("1") {
        return None;
    }
    let host = field("HTTPSProxy").filter(|h| !h.is_empty())?;
    Some(match field("HTTPSPort").filter(|p| !p.is_empty()) {
        Some(port) => format!("http://{host}:{port}"),
        None => format!("http://{host}"),
    })
}
/// `reg query …\Internet Settings`: `ProxyEnable REG_DWORD 0x1` and `ProxyServer REG_SZ host:port`, or per protocol
/// `http=a:1;https=b:2`.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
fn windows_proxy(text: &str) -> Option<String> {
    let field = |key: &str| {
        text.lines().find_map(|line| {
            let mut parts = line.split_whitespace();
            (parts.next()? == key).then(|| {
                parts.next();
                parts.collect::<Vec<_>>().join(" ")
            })
        })
    };
    if field("ProxyEnable").is_none_or(|v| v.trim() != "0x1") {
        return None;
    }
    let server = field("ProxyServer").filter(|s| !s.trim().is_empty())?;
    let chosen = if server.contains('=') {
        let pick = |scheme: &str| {
            server.split(';').find_map(|part| part.trim().strip_prefix(scheme).map(|s| s.trim().to_owned()))
        };
        pick("https=").or_else(|| pick("http="))?
    } else {
        server.trim().to_owned()
    };
    Some(if chosen.contains("://") { chosen } else { format!("http://{chosen}") })
}
fn agent(proxy: Option<&str>) -> ureq::Agent {
    let mut builder = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(20))
        .timeout_read(Duration::from_secs(60))
        .redirects(8);
    if let Some(address) = proxy {
        match ureq::Proxy::new(address) {
            Ok(proxy) => builder = builder.proxy(proxy),
            // SOCKS needs a feature this build does not have: go direct rather than not at all.
            Err(e) => eprintln!("models: proxy {address} not usable ({e}); connecting directly"),
        }
    }
    builder.build()
}

// ---------- the job ----------

/// The one download or import this app runs at a time; switching libraries does not stop it.
pub(crate) struct Models {
    job: Mutex<Option<Value>>,
    cancel: AtomicBool,
    running: AtomicBool,
}
static APP: Models = Models::new();
impl Models {
    const fn new() -> Self {
        Self { job: Mutex::new(None), cancel: AtomicBool::new(false), running: AtomicBool::new(false) }
    }
    fn set(&self, change: impl FnOnce(&mut Value)) {
        let mut job = self.job.lock().expect("models job mutex poisoned");
        if let Some(job) = job.as_mut() {
            change(job);
        }
    }
    fn snapshot(&self) -> Value {
        self.job.lock().expect("models job mutex poisoned").clone().unwrap_or(Value::Null)
    }
    fn stopped(&self) -> bool {
        self.cancel.load(Ordering::SeqCst) || crate::platform_process::stopping()
    }
    /// Starts `work` on its own thread unless a job is running.
    fn start(
        &'static self,
        kind: &str,
        total: u64,
        work: impl FnOnce(&'static Models) -> std::result::Result<Value, Stop> + Send + 'static,
    ) -> Result<()> {
        if self.running.swap(true, Ordering::SeqCst) {
            return Err(Error::Invalid("模型正在下载或导入，请等它结束".into()));
        }
        self.cancel.store(false, Ordering::SeqCst);
        *self.job.lock().expect("models job mutex poisoned") = Some(json!({
            "kind": kind, "status": "running", "done": 0, "total": total, "speed": 0, "source": null, "file": null,
            "message": null, "proxy": null, "results": [], "failed_file": null, "failures": [],
        }));
        std::thread::spawn(move || {
            let outcome = work(self);
            self.set(|job| match outcome {
                Ok(fields) => {
                    job["status"] = "done".into();
                    if let Value::Object(fields) = fields {
                        for (k, v) in fields {
                            job[k] = v;
                        }
                    }
                }
                Err(Stop::Cancelled) => job["status"] = "cancelled".into(),
                Err(Stop::Failed(message)) => {
                    job["status"] = "failed".into();
                    job["message"] = message.into();
                }
            });
            self.running.store(false, Ordering::SeqCst);
        });
        Ok(())
    }
}
enum Stop {
    Cancelled,
    Failed(String),
}
impl From<std::io::Error> for Stop {
    fn from(e: std::io::Error) -> Self {
        Stop::Failed(format!("写入模型文件失败：{error}", error = e))
    }
}

/// Bytes counted so far and a short window for the speed shown.
struct Meter<'a> {
    models: &'a Models,
    base: u64,
    samples: Vec<(Instant, u64)>,
    shown: Instant,
}
impl<'a> Meter<'a> {
    fn new(models: &'a Models, base: u64) -> Self {
        Self { models, base, samples: vec![(Instant::now(), base)], shown: Instant::now() }
    }
    fn tick(&mut self, current: u64) -> std::result::Result<(), Stop> {
        if self.models.stopped() {
            return Err(Stop::Cancelled);
        }
        let now = Instant::now();
        if now.duration_since(self.shown) < Duration::from_millis(250) {
            return Ok(());
        }
        self.shown = now;
        let done = self.base + current;
        self.samples.push((now, done));
        self.samples.retain(|(at, _)| now.duration_since(*at) <= Duration::from_secs(5));
        let (first, bytes) = self.samples[0];
        let seconds = now.duration_since(first).as_secs_f64();
        let speed = if seconds > 0.5 { (done.saturating_sub(bytes) as f64 / seconds) as u64 } else { 0 };
        self.models.set(|job| {
            job["done"] = done.into();
            job["speed"] = speed.into();
        });
        Ok(())
    }
    /// A file is finished (or skipped): its bytes join the base.
    fn finish(&mut self, size: u64) {
        self.base += size;
        let done = self.base;
        self.models.set(|job| job["done"] = done.into());
    }
}

// ---------- download ----------

enum Failure {
    /// The connection dropped part way: the bytes so far are kept; the same source may continue.
    Retry(String),
    /// This source cannot give this file: try the next.
    Next(String),
    Stop(Stop),
}
impl From<Stop> for Failure {
    fn from(stop: Stop) -> Self {
        Failure::Stop(stop)
    }
}
impl From<std::io::Error> for Failure {
    fn from(e: std::io::Error) -> Self {
        Failure::Stop(e.into())
    }
}

/// Reads `part` into the hash, so a resumed download is checked from its first byte.
fn hash_existing(file: &mut fs::File, length: u64, meter: &mut Meter) -> std::result::Result<Sha256, Failure> {
    let mut digest = Sha256::new();
    file.seek(SeekFrom::Start(0))?;
    let mut reader = (&mut *file).take(length);
    let mut buffer = vec![0; 1 << 18];
    loop {
        let n = reader.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        digest.update(&buffer[..n]);
        meter.tick(0)?;
    }
    Ok(digest)
}

/// One attempt to complete `entry` from `address` into `<partial>/<path>.part`; on success the file has its own
/// name in the partial folder.
fn fetch(
    agent: &ureq::Agent,
    address: &str,
    partial: &Path,
    entry: &Entry,
    meter: &mut Meter,
) -> std::result::Result<(), Failure> {
    let path = part(partial, entry);
    fs::create_dir_all(path.parent().expect("model part has a parent"))?;
    let mut file = fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&path)?;
    let mut length = file.metadata()?.len();
    if length > entry.size {
        file.set_len(0)?;
        length = 0;
    }
    let mut digest = hash_existing(&mut file, length, meter)?;
    if length < entry.size {
        let mut request = agent.get(address).set("User-Agent", "Illustash-models").set("Accept-Encoding", "identity");
        if length > 0 {
            request = request.set("Range", &format!("bytes={length}-"));
        }
        let reply = match request.call() {
            Ok(reply) => reply,
            Err(ureq::Error::Status(416, _)) => {
                file.set_len(0)?;
                return Err(Failure::Retry("续传位置无效，从头下载".into()));
            }
            Err(ureq::Error::Status(code, _)) => return Err(Failure::Next(format!("HTTP {code}"))),
            Err(ureq::Error::Transport(t)) => return Err(Failure::Next(transport(&t))),
        };
        match reply.status() {
            206 => {
                // `bytes start-end/total`: the source must continue exactly where the part stops.
                let range = reply.header("Content-Range").unwrap_or("");
                let start = range.strip_prefix("bytes ").and_then(|r| r.split('-').next()).and_then(|s| s.parse().ok());
                let total = range.rsplit('/').next().and_then(|s| s.parse::<u64>().ok());
                if total.is_some_and(|t| t != entry.size) {
                    return Err(Failure::Next("文件大小与清单不符".into()));
                }
                if start != Some(length) {
                    file.set_len(0)?;
                    return Err(Failure::Retry("续传位置不符，从头下载".into()));
                }
            }
            200 => {
                if reply.header("Content-Length").and_then(|n| n.parse::<u64>().ok()).is_some_and(|n| n != entry.size) {
                    return Err(Failure::Next("文件大小与清单不符".into()));
                }
                if length > 0 {
                    // The source does not resume: start over.
                    file.set_len(0)?;
                    length = 0;
                    digest = Sha256::new();
                }
            }
            code => return Err(Failure::Next(format!("HTTP {code}"))),
        }
        file.seek(SeekFrom::Start(length))?;
        let mut reader = reply.into_reader();
        let mut buffer = vec![0; 1 << 18];
        loop {
            let n = match reader.read(&mut buffer) {
                Ok(n) => n,
                Err(e) => {
                    file.flush()?;
                    eprintln!("models: {address}: {e}");
                    return Err(Failure::Retry("连接中断".into()));
                }
            };
            if n == 0 {
                break;
            }
            if length + n as u64 > entry.size {
                file.set_len(0)?;
                return Err(Failure::Next("下载的数据比清单里的文件大".into()));
            }
            file.write_all(&buffer[..n])?;
            digest.update(&buffer[..n]);
            length += n as u64;
            meter.tick(length)?;
        }
        file.flush()?;
        if length < entry.size {
            return Err(Failure::Retry("连接提前结束".into()));
        }
    }
    file.sync_all()?;
    drop(file);
    if format!("{:x}", digest.finalize()) != entry.sha256 {
        fs::remove_file(&path)?;
        return Err(Failure::Next("SHA-256 与清单不符，已丢弃".into()));
    }
    fs::rename(&path, partial.join(&entry.path))?;
    Ok(())
}
fn transport(t: &ureq::Transport) -> String {
    match t.kind() {
        ureq::ErrorKind::Dns => "找不到服务器（DNS）".into(),
        ureq::ErrorKind::ConnectionFailed => "连接失败".into(),
        ureq::ErrorKind::TooManyRedirects => "重定向过多".into(),
        ureq::ErrorKind::ProxyConnect | ureq::ErrorKind::ProxyUnauthorized => "代理连接失败".into(),
        ureq::ErrorKind::Io => "网络连接中断或超时".into(),
        kind => format!("网络错误（{kind}）"),
    }
}

/// Bytes already on disk toward the pack: finished files and the `.part`s.
fn present(root: &Path, partial: &Path, manifest: &Manifest) -> u64 {
    manifest
        .files
        .iter()
        .map(|e| {
            if has(root, e) || has(partial, e) {
                e.size
            } else {
                fs::metadata(part(partial, e)).map(|m| m.len().min(e.size)).unwrap_or(0)
            }
        })
        .sum()
}

fn download(models: &Models, place: &Place) -> std::result::Result<Value, Stop> {
    let partial = partial_dir(&place.root);
    fs::create_dir_all(&partial)?;
    let needed =
        place.manifest.files.iter().map(|e| e.size).sum::<u64>() - present(&place.root, &partial, place.manifest);
    if let Ok(free) = crate::platform_filesystem::available_space(&partial) {
        if free < needed + (64 << 20) {
            return Err(Stop::Failed(format!(
                "磁盘空间不足：还需要约 {needed} MB，可用约 {available} MB",
                needed = (needed + (64 << 20)) >> 20,
                available = free >> 20
            )));
        }
    }
    let (direct, proxied) = (agent(None), agent(place.proxy.as_deref()));
    let mut meter = Meter::new(models, 0);
    for entry in &place.manifest.files {
        if has(&place.root, entry) || has(&partial, entry) {
            meter.finish(entry.size);
            continue;
        }
        let mut tried = Vec::new();
        let mut done = false;
        for (label, address) in place.sources(entry) {
            // A source on this machine (a local mirror, tests) never goes through the proxy.
            let local = url::Url::parse(&address)
                .is_ok_and(|u| matches!(u.host_str(), Some("127.0.0.1" | "localhost" | "[::1]")));
            let via = if local { None } else { place.proxy.as_deref().map(host) };
            models.set(|job| {
                job["source"] = label.clone().into();
                job["file"] = entry.path.clone().into();
                job["proxy"] = via.into();
            });
            // A connection that drops part way continues from where it stopped, a few times, on the same source.
            let mut drops = 0;
            loop {
                match fetch(if local { &direct } else { &proxied }, &address, &partial, entry, &mut meter) {
                    Ok(()) => {
                        done = true;
                        break;
                    }
                    Err(Failure::Stop(stop)) => return Err(stop),
                    Err(Failure::Retry(why)) if drops < 3 => {
                        drops += 1;
                        eprintln!("models: {label} {}: {why}; retrying", entry.path);
                    }
                    Err(Failure::Retry(why) | Failure::Next(why)) => {
                        tried.push((label.clone(), why));
                        break;
                    }
                }
            }
            if done {
                break;
            }
        }
        if !done {
            // Each source's reason apart, so the page can show (and translate) them one by one.
            models.set(|job| {
                job["failed_file"] = entry.path.clone().into();
                job["failures"] = tried.iter().map(|(source, why)| json!({"source": source, "why": why})).collect();
            });
            let reasons: Vec<String> = tried.iter().map(|(source, why)| format!("{source} 出错：{why}")).collect();
            return Err(Stop::Failed(format!(
                "{path} 下载失败（{reason}）",
                path = entry.path,
                reason = reasons.join("；")
            )));
        }
        meter.finish(entry.size);
    }
    activate(&place.root, &partial, place.manifest)?;
    Ok(json!({"message": null}))
}

/// All files are in `partial` (or already in the root): make them the root's in one step.
fn activate(root: &Path, partial: &Path, manifest: &Manifest) -> std::io::Result<()> {
    let record = json!({"pack": manifest.pack, "files": manifest.files.iter()
        .map(|e| json!({"path": e.path, "size": e.size, "sha256": e.sha256})).collect::<Vec<_>>()});
    fs::write(partial.join("pack.json"), serde_json::to_string_pretty(&record).expect("pack record") + "\n")?;
    match fs::symlink_metadata(root) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if let Some(parent) = root.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::rename(partial, root)
        }
        Err(e) => Err(e),
        Ok(_) => {
            // The root exists without a complete set (a portable or development folder): move the files in one by
            // one; the root counts as ready only once the last one is there.
            for entry in &manifest.files {
                if has(partial, entry) {
                    let target = root.join(&entry.path);
                    fs::create_dir_all(target.parent().expect("model file has a parent"))?;
                    fs::rename(partial.join(&entry.path), target)?;
                }
            }
            fs::rename(partial.join("pack.json"), root.join("pack.json"))?;
            fs::remove_dir_all(partial)
        }
    }
}

// ---------- import ----------

/// Copies `reader` to the entry's `.part` while hashing; keeps it under its own name when the hash matches.
fn copy_checked(
    reader: &mut dyn Read,
    partial: &Path,
    entry: &Entry,
    meter: &mut Meter,
) -> std::result::Result<bool, Stop> {
    let path = part(partial, entry);
    fs::create_dir_all(path.parent().expect("model part has a parent"))?;
    let mut file = fs::File::create(&path)?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0; 1 << 18];
    let mut length = 0u64;
    loop {
        let n = reader.read(&mut buffer).map_err(|e| Stop::Failed(format!("读取所选文件失败：{error}", error = e)))?;
        if n == 0 {
            break;
        }
        length += n as u64;
        if length > entry.size {
            break;
        }
        file.write_all(&buffer[..n])?;
        digest.update(&buffer[..n]);
        meter.tick(length)?;
    }
    file.sync_all()?;
    drop(file);
    if length != entry.size || format!("{:x}", digest.finalize()) != entry.sha256 {
        fs::remove_file(&path)?;
        return Ok(false);
    }
    fs::rename(&path, partial.join(&entry.path))?;
    Ok(true)
}

/// The files the person chose: each file, or each entry of a ZIP, is matched by size, then by SHA-256. Names and
/// folders do not matter (both upstream ONNX files are called model*.onnx).
fn import(models: &Models, place: &Place, paths: &[PathBuf]) -> std::result::Result<Value, Stop> {
    let partial = partial_dir(&place.root);
    fs::create_dir_all(&partial)?;
    let manifest = place.manifest;
    let wanted = |size: u64| manifest.files.iter().find(|e| e.size == size);
    let mut results = Vec::new();
    let mut meter = Meter::new(models, 0);
    let mut take =
        |name: String, size: u64, reader: &mut dyn Read, meter: &mut Meter| -> std::result::Result<(), Stop> {
            let result = match wanted(size) {
                None => "unknown",
                Some(entry) if has(&place.root, entry) || has(&partial, entry) => "already",
                Some(entry) => {
                    models.set(|job| job["file"] = entry.path.clone().into());
                    let ok = copy_checked(reader, &partial, entry, meter)?;
                    meter.finish(entry.size);
                    if ok {
                        "accepted"
                    } else {
                        "mismatch"
                    }
                }
            };
            results.push(json!({"name": name, "result": result, "path": wanted(size).map(|e| e.path.clone())}));
            Ok(())
        };
    for path in paths {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let unreadable = |e: std::io::Error| Stop::Failed(format!("读不到所选文件 {name}：{error}", error = e));
        let metadata = fs::metadata(path).map_err(unreadable)?;
        let zip = path.extension().is_some_and(|x| x.eq_ignore_ascii_case("zip"));
        if zip {
            let file = fs::File::open(path).map_err(unreadable)?;
            let mut archive = zip::ZipArchive::new(file)
                .map_err(|e| Stop::Failed(format!("{name} 不是有效的 ZIP：{error}", error = e)))?;
            for i in 0..archive.len() {
                let mut item =
                    archive.by_index(i).map_err(|e| Stop::Failed(format!("{name} 读取失败：{error}", error = e)))?;
                if !item.is_file() {
                    continue;
                }
                let inner = format!("{name}/{}", item.name());
                let size = item.size();
                take(inner, size, &mut item, &mut meter)?;
            }
        } else if metadata.is_file() {
            let mut file = fs::File::open(path).map_err(unreadable)?;
            take(name, metadata.len(), &mut file, &mut meter)?;
        } else {
            let mut empty = std::io::empty();
            take(name, u64::MAX, &mut empty, &mut meter)?;
        }
    }
    let complete = manifest.files.iter().all(|e| has(&place.root, e) || has(&partial, e));
    if complete && !ready_at(&place.root, manifest) {
        activate(&place.root, &partial, manifest)?;
    }
    Ok(json!({"results": results}))
}

// ---------- status and requests ----------

fn info_at(models: &Models, place: &Place) -> Json {
    let manifest = place.manifest;
    let partial = partial_dir(&place.root);
    let ready = ready_at(&place.root, manifest);
    let missing: Vec<Value> = manifest
        .files
        .iter()
        .filter(|e| !has(&place.root, e) && !has(&partial, e))
        .map(|e| e.path.clone().into())
        .collect();
    let mirror = place.mirror();
    Json::from(json!({
        "pack": manifest.pack,
        "status": if ready { "ready" } else { "missing" },
        "on_demand": on_demand(),
        "worker": std::env::var_os("ILLUSTASH_RECOGNITION_WORKER").is_some_and(|p| !p.is_empty()),
        "location": place.root.display().to_string(),
        "total": manifest.files.iter().map(|e| e.size).sum::<u64>(),
        "have": if ready { manifest.files.iter().map(|e| e.size).sum::<u64>() } else { present(&place.root, &partial, manifest) },
        "partial": partial.exists(),
        "missing": missing,
        "mirror": mirror,
        "mirror_default": manifest.mirror,
        "sources": [host(&mirror), host(&place.modelscope)],
        "manual": manifest.manual,
        "files": manifest.files.iter().map(|e| json!({"path": e.path, "size": e.size, "sha256": e.sha256})).collect::<Vec<_>>(),
        "job": models.snapshot(),
    }))
}

/// GET /api/models.
pub(crate) fn info(resources: &Path) -> Result<Json> {
    Ok(info_at(&APP, &Place::app(resources)?))
}

/// POST /api/models:
/// - `{"action": "download"}` starts (or resumes) the download (the person's click);
/// - `{"action": "import", "paths": [...]}` checks and installs chosen files (the page asks the native chooser first);
/// - `{"action": "cancel"}` stops the running job, keeping what is downloaded;
/// - `{"action": "discard"}` deletes the unfinished part;
/// - `{"action": "mirror", "mirror": "https://…" | null}` sets the mirror (null: the default);
/// - `{"action": "delete"}` deletes the downloaded models (user 2026-10-19, #168): only an install that downloads
///   them, never while a job runs or `in_use` (the session recognises or updates the character index).
pub(crate) fn post(resources: &Path, body: &Json, in_use: bool) -> Result<Json> {
    let mut place = Place::app(resources)?;
    place.proxy = proxy();
    place.in_use = in_use;
    post_at(&APP, place, body)
}

fn post_at(models: &'static Models, place: Place, body: &Json) -> Result<Json> {
    let body: Value = serde_json::from_str(&body.dump()).map_err(|e| Error::Invalid(e.to_string()))?;
    let Some(fields) = body.as_object() else { return Err(Error::Invalid("未知操作".into())) };
    let action = fields.get("action").and_then(Value::as_str).unwrap_or("");
    let allowed: &[&str] = match action {
        "download" | "cancel" | "discard" | "delete" => &["action"],
        "import" => &["action", "paths"],
        "mirror" => &["action", "mirror"],
        _ => return Err(Error::Invalid("未知操作".into())),
    };
    if fields.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err(Error::Invalid("未知参数".into()));
    }
    match action {
        "download" => {
            if ready_at(&place.root, place.manifest) {
                return Err(Error::Invalid("识别模型已经就位".into()));
            }
            let total = place.manifest.files.iter().map(|e| e.size).sum();
            let job = place.clone();
            models.start("download", total, move |models| download(models, &job))?;
        }
        "import" => {
            let paths: Vec<PathBuf> = fields
                .get("paths")
                .and_then(Value::as_array)
                .filter(|p| !p.is_empty() && p.len() <= 64)
                .ok_or_else(|| Error::Invalid("请选择要导入的文件".into()))?
                .iter()
                .map(|p| p.as_str().filter(|p| !p.is_empty()).map(PathBuf::from))
                .collect::<Option<_>>()
                .ok_or_else(|| Error::Invalid("文件路径不正确".into()))?;
            let total = paths.iter().filter_map(|p| fs::metadata(p).ok()).map(|m| m.len()).sum();
            let job = place.clone();
            models.start("import", total, move |models| import(models, &job, &paths))?;
        }
        "cancel" => models.cancel.store(true, Ordering::SeqCst),
        "discard" => {
            if models.running.load(Ordering::SeqCst) {
                return Err(Error::Invalid("模型正在下载或导入，请先取消".into()));
            }
            let partial = partial_dir(&place.root);
            if partial.exists() {
                fs::remove_dir_all(&partial)?;
            }
            *models.job.lock().expect("models job mutex poisoned") = None;
        }
        "delete" => {
            if !place.on_demand {
                return Err(Error::Invalid("这个安装自带识别模型，不能删除".into()));
            }
            if models.running.load(Ordering::SeqCst) {
                return Err(Error::Invalid("模型正在下载或导入，请先取消".into()));
            }
            if place.in_use {
                return Err(Error::Invalid("正在识别图片，等它结束再删除识别模型".into()));
            }
            // Renamed first, so a removal stopped half way never leaves something that looks like the models.
            let doomed = place.root.with_file_name(format!(
                "{}.deleting",
                place.root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
            ));
            if doomed.exists() {
                fs::remove_dir_all(&doomed)?;
            }
            if place.root.exists() {
                fs::rename(&place.root, &doomed)?;
                fs::remove_dir_all(&doomed)?;
            }
            let partial = partial_dir(&place.root);
            if partial.exists() {
                fs::remove_dir_all(&partial)?;
            }
            *models.job.lock().expect("models job mutex poisoned") = None;
        }
        _ => {
            let mirror = match fields.get("mirror") {
                None | Some(Value::Null) => None,
                Some(Value::String(text)) if text.trim().is_empty() => None,
                Some(Value::String(text)) => Some(base(text).map_err(Error::Invalid)?),
                Some(_) => return Err(Error::Invalid("镜像地址不正确".into())),
            };
            crate::app_updates::update(&place.preferences, |prefs| match mirror {
                Some(mirror) => {
                    prefs.insert("model_mirror".into(), mirror.into());
                }
                None => {
                    prefs.remove("model_mirror");
                }
            })?;
        }
    }
    Ok(info_at(models, &place))
}

#[cfg(test)]
mod tests;
