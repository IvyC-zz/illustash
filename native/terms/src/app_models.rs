//! 首次使用时下载的识别模型（#168，用户 2026-10-16）：安装包不带 WD 和 CCIP 模型；用户下载一次，
//! 或者导入自己下载的文件。
//!
//! 安全依靠应用内置的清单（native/terms/models.json），而不是字节从哪里来：清单列出每个文件的大小和 SHA-256，
//! 对不上的文件不管来自哪个来源都拒绝。来源按文件依次尝试：经镜像访问的 Hugging Face 地址（默认 hf-mirror，
//! 可在设置里更改，作为 `model_mirror` 全应用保存在 preferences.json），然后是 ModelScope 的副本。
//! WD Core ML 派生文件来自项目固定版本的 GitHub Release。不做测速。
//!
//! 位置：worker 使用的模型根目录（`ILLUSTASH_MODELS`，否则 `<resources>/models`；见 app_resources）。
//! 工作在它旁边的 `<root>.partial/` 里进行：每个文件写成 `<path>.part`，哈希对上后改名为它自己的名字，
//! 原始模型到齐后激活，再下载可选的 WD 加速文件；加速文件失败或中断不影响已就位的原始模型。
//! 下次运行用 HTTP Range 请求从 `.part` 接着下，或者用户丢弃这个文件夹。
//!
//! 联网规则与其他地方一样：用户点击之前什么都不下载；请求是普通的 HTTPS GET，不带任何图库信息。
//! HTTP 代理取自环境变量，否则取系统设置。
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

#[derive(Deserialize, Clone)]
pub struct Manifest {
    pub format: u32,
    /// 在共享数据里给模型文件夹命名，以后的模型包从不会和这一个混在一起。
    pub pack: String,
    /// 带手动下载说明的项目页面；应用只链接到它。
    pub manual: String,
    pub mirror: String,
    pub modelscope: String,
    pub files: Vec<Entry>,
    /// macOS 的 WD 执行变体；原始文件仍决定模型身份和 CPU 回退。
    pub wd_coreml: Option<WdCoreMl>,
}
#[derive(Deserialize, Clone)]
pub struct Entry {
    /// 在模型根目录下，用 `/` 分隔。
    pub path: String,
    pub size: u64,
    pub sha256: String,
    #[serde(flatten)]
    source: Source,
    /// 运行时加入本平台的变体；缺失不会阻塞原始模型识别。
    #[serde(skip)]
    pub optional: bool,
}
#[derive(Deserialize, Clone)]
#[serde(untagged)]
enum Source {
    Upstream { hf: Origin, modelscope: Origin },
    Project { url: String },
}
#[derive(Deserialize, Clone)]
pub struct WdCoreMl {
    pub revision: u32,
    pub source_sha256: String,
    pub file: Entry,
}
#[derive(Deserialize, Clone)]
struct Origin {
    repo: String,
    revision: String,
    file: String,
}

/// 这个构建使用的模型。
pub fn manifest() -> &'static Manifest {
    static MANIFEST: OnceLock<Manifest> = OnceLock::new();
    MANIFEST.get_or_init(|| {
        let mut manifest: Manifest =
            serde_json::from_str(include_str!("../models.json")).expect("models.json is valid");
        assert!(manifest.format == 1 && !manifest.files.is_empty(), "models.json format");
        if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
            if let Some(variant) = &manifest.wd_coreml {
                let mut file = variant.file.clone();
                file.optional = true;
                manifest.files.push(file);
            }
        }
        manifest
    })
}

/// worker 加载模型的根目录：`ILLUSTASH_MODELS`（相对于资源根目录），否则 `<resources>/models`。
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
/// 下载与导入时校验执行变体的已有副本，允许修复同长度损坏；轮询状态不读取大文件。
fn usable(dir: &Path, entry: &Entry) -> bool {
    has(dir, entry)
        && (!entry.optional
            || crate::recognition_compute::models::hash(&dir.join(&entry.path), &|| Ok(()))
                .is_ok_and(|sha| sha == entry.sha256))
}
/// 每个模型文件都已就位。只有校验过的文件才会移进根目录（见 `activate`），所以这里看有没有就够了；
/// 识别仍会对它加载的文件计算指纹（recognition_compute::models）。开销很小，每次读状态都可以做。
fn ready_at(root: &Path, manifest: &Manifest) -> bool {
    manifest
        .files
        .iter()
        .filter(|e| !e.optional)
        .all(|entry| fs::metadata(root.join(&entry.path)).is_ok_and(|m| m.is_file()))
}
/// 桌面外壳为不带模型的安装包设置（#168）：识别、角色索引和自检会等到模型就位，并说明原因。
/// 不设置时什么都不等（完整安装包、测试和开发运行各有自己的模型文件夹）。
pub(crate) fn on_demand() -> bool {
    std::env::var_os("ILLUSTASH_MODELS_ON_DEMAND").is_some_and(|v| v == "1")
}
/// 识别必须等模型下载或导入完成。
pub(crate) fn waiting(resources: &Path) -> bool {
    on_demand() && !ready_at(&root(resources), manifest())
}
/// 识别为什么还不能开始，用于同步、索引和自检的消息。
pub(crate) const MISSING: &str = "识别模型还没下载";

fn partial_dir(root: &Path) -> PathBuf {
    let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "models".into());
    root.with_file_name(format!("{name}.partial"))
}
fn part(dir: &Path, entry: &Entry) -> PathBuf {
    dir.join(format!("{}.part", entry.path))
}

// ---------- 从哪里下载 ----------

/// 来源的基础地址：任何 HTTPS 地址，或本机上的普通 HTTP（测试和本地镜像）。不带凭据、查询串或片段；去掉结尾的斜杠。
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

/// 状态存在哪里：应用里是 preferences.json，测试里是夹具。
#[derive(Clone)]
struct Place {
    root: PathBuf,
    manifest: &'static Manifest,
    preferences: PathBuf,
    /// ModelScope 的基础地址：清单里的，或本地测试服务器（ILLUSTASH_MODELSCOPE_BASE，只允许本机上的普通 HTTP）。
    modelscope: String,
    /// 在请求到达时读取，所以在系统设置里打开的代理从下一次点击起生效。
    proxy: Option<String>,
    /// 模型由这次安装负责下载，因而也由它负责删除（`ILLUSTASH_MODELS_ON_DEMAND`）。
    on_demand: bool,
    /// 会话正在识别或更新角色索引：模型正在使用中。
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
    /// 原始模型经 HF 镜像和 ModelScope；派生文件直接使用清单固定的项目 Release。
    fn sources(&self, entry: &Entry) -> Vec<(String, String)> {
        match &entry.source {
            Source::Project { url } => vec![(host(url), url.clone())],
            Source::Upstream { hf, modelscope: ms } => {
                let mirror = self.mirror();
                vec![
                    (host(&mirror), format!("{mirror}/{}/resolve/{}/{}", hf.repo, hf.revision, hf.file)),
                    (
                        host(&self.modelscope),
                        format!("{}/models/{}/resolve/{}/{}", self.modelscope, ms.repo, ms.revision, ms.file),
                    ),
                ]
            }
        }
    }
}

// ---------- proxy ----------

/// 要使用的 HTTP 代理：`HTTPS_PROXY` / `ALL_PROXY`，否则系统的 HTTPS 代理（macOS 网络设置、Windows Internet 选项）。
/// 不读自动配置（PAC）：这时下载直连。
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
/// 按完整路径调用 `reg.exe`，从不在 PATH 上查找：`%SystemRoot%\System32`，否则 `C:\Windows\System32`。
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
/// `reg query …\Internet Settings`：`ProxyEnable REG_DWORD 0x1` 和 `ProxyServer REG_SZ host:port`，
/// 或按协议的 `http=a:1;https=b:2`。
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
            // SOCKS 需要这个构建没有的功能：直连，总比完全连不上好。
            Err(e) => eprintln!("models: proxy {address} not usable ({e}); connecting directly"),
        }
    }
    builder.build()
}

// ---------- the job ----------

/// 这个应用同一时间只运行一个下载或导入；切换图库不会停止它。
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
    /// 在它自己的线程上启动 `work`，除非已有任务在运行。
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

/// 到目前为止计入的字节，以及计算显示速度用的一个短窗口。
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
    /// 一个文件完成了（或跳过了）：它的字节计入基数。
    fn finish(&mut self, size: u64) {
        self.base += size;
        let done = self.base;
        self.models.set(|job| job["done"] = done.into());
    }
}

// ---------- download ----------

enum Failure {
    /// 连接中途断开：已下载的字节保留；可以从同一个来源继续。
    Retry(String),
    /// 这个来源给不了这个文件：试下一个。
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

/// 把 `part` 读进哈希，所以接着下载的文件从第一个字节起就经过核对。
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

/// 从 `address` 下载 `entry` 到 `<partial>/<path>.part` 的一次尝试；成功后文件在 partial 文件夹里有了它自己的名字。
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
                // `bytes start-end/total`：来源必须正好从 part 结束的地方接着发。
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
                    // 来源不支持续传：从头开始。
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

/// 磁盘上已有的、属于这个模型包的字节：完成的文件和各个 `.part`。
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

fn check_space(partial: &Path, needed: u64) -> std::result::Result<(), Stop> {
    if let Ok(free) = crate::platform_filesystem::available_space(partial) {
        if free < needed + (64 << 20) {
            return Err(Stop::Failed(format!(
                "磁盘空间不足：还需要约 {needed} MB，可用约 {available} MB",
                needed = (needed + (64 << 20)) >> 20,
                available = free >> 20
            )));
        }
    }
    Ok(())
}

fn download(models: &Models, place: &Place) -> std::result::Result<Value, Stop> {
    let partial = partial_dir(&place.root);
    fs::create_dir_all(&partial)?;
    let missing_bytes = |optional| {
        place
            .manifest
            .files
            .iter()
            .filter(|e| e.optional == optional)
            .map(|e| {
                if has(&place.root, e) || has(&partial, e) {
                    0
                } else {
                    e.size.saturating_sub(fs::metadata(part(&partial, e)).map(|m| m.len()).unwrap_or(0))
                }
            })
            .sum()
    };
    check_space(&partial, missing_bytes(false))?;
    let (direct, proxied) = (agent(None), agent(place.proxy.as_deref()));
    let mut meter = Meter::new(models, 0);
    let mut originals_active = false;
    for entry in &place.manifest.files {
        if entry.optional && !originals_active {
            activate(&place.root, &partial, place.manifest)?;
            fs::create_dir_all(&partial)?;
            check_space(&partial, missing_bytes(true))?;
            originals_active = true;
        }
        if usable(&place.root, entry) || usable(&partial, entry) {
            meter.finish(entry.size);
            continue;
        }
        let mut tried = Vec::new();
        let mut done = false;
        for (label, address) in place.sources(entry) {
            // 本机上的来源（本地镜像、测试）从不走代理。
            let local = url::Url::parse(&address)
                .is_ok_and(|u| matches!(u.host_str(), Some("127.0.0.1" | "localhost" | "[::1]")));
            let via = if local { None } else { place.proxy.as_deref().map(host) };
            models.set(|job| {
                job["source"] = label.clone().into();
                job["file"] = entry.path.clone().into();
                job["proxy"] = via.into();
            });
            // 中途断开的连接从停下的地方接着下，在同一个来源上重试几次。
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
            // 每个来源的原因分开，让页面能逐个显示（和翻译）。
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

/// 所有文件都在 `partial` 里（或已经在根目录里）：一步把它们变成根目录的文件。
fn activate(root: &Path, partial: &Path, manifest: &Manifest) -> std::io::Result<()> {
    let record = json!({"pack": manifest.pack, "files": manifest.files.iter()
        .filter(|e| has(root, e) || has(partial, e))
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
            // 根目录存在但文件不全（便携版或开发用的文件夹）：逐个移入文件；最后一个到位后根目录才算就绪。
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

/// 边计算哈希边把 `reader` 复制到条目的 `.part`；哈希对上时以它自己的名字保留。
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

/// 用户选的文件：每个文件或 ZIP 里的每一项，先按大小、再按 SHA-256 匹配。名字和文件夹无关紧要（上游的两个 ONNX
/// 文件都叫 model*.onnx）。
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
                Some(entry) if usable(&place.root, entry) || usable(&partial, entry) => "already",
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
    let complete = manifest.files.iter().filter(|e| !e.optional).all(|e| has(&place.root, e) || has(&partial, e));
    if complete {
        activate(&place.root, &partial, manifest)?;
    }
    Ok(json!({"results": results}))
}

// ---------- 状态和请求 ----------

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
        "have": present(&place.root, &partial, manifest),
        "optional_missing": manifest.files.iter().any(|e| e.optional && !has(&place.root, e)),
        "partial": partial.exists(),
        "missing": missing,
        "mirror": mirror,
        "mirror_default": manifest.mirror,
        "sources": manifest.files.iter().flat_map(|e| place.sources(e).into_iter().map(|(name, _)| name)).collect::<std::collections::BTreeSet<_>>(),
        "manual": manifest.manual,
        "files": manifest.files.iter().map(|e| json!({"path": e.path, "size": e.size, "sha256": e.sha256, "optional": e.optional})).collect::<Vec<_>>(),
        "job": models.snapshot(),
    }))
}

/// GET /api/models.
pub(crate) fn info(resources: &Path) -> Result<Json> {
    Ok(with_acceleration(resources, info_at(&APP, &Place::app(resources)?)))
}

/// 「使用 GPU 加速识别」（#270）在模型卡片上：这个构建有没有加速器、开关（POST /api/preferences），
/// 以及上一个 worker 每个模型用的是什么（recognition_compute/backend.rs）。
fn with_acceleration(resources: &Path, mut value: Json) -> Json {
    let shared = crate::app_resources::shared_path(resources).ok();
    value.set("acceleration", Json::from(crate::recognition_compute::backend::info(shared.as_deref())));
    value
}

/// POST /api/models：
/// - `{"action": "download"}` 开始（或继续）下载（用户的点击）；
/// - `{"action": "import", "paths": [...]}` 核对并安装选中的文件（页面先调用原生选择器）；
/// - `{"action": "cancel"}` 停止正在运行的任务，保留已下载的部分；
/// - `{"action": "discard"}` 删除未完成的部分；
/// - `{"action": "mirror", "mirror": "https://…" | null}` 设置镜像（null：默认值）；
/// - `{"action": "delete"}` 删除下载的模型（用户 2026-10-19，#168）：只限会下载模型的安装，
///   任务在运行或 `in_use`（会话正在识别或更新角色索引）时都不行。
pub(crate) fn post(resources: &Path, body: &Json, in_use: bool) -> Result<Json> {
    let mut place = Place::app(resources)?;
    place.proxy = proxy();
    place.in_use = in_use;
    post_at(&APP, place, body).map(|value| with_acceleration(resources, value))
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
            if ready_at(&place.root, place.manifest) && !place.manifest.files.iter().any(|e| e.optional) {
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
            // 先改名，这样中途停下的删除从不会留下看起来像模型的东西。
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
