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
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
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
    /// 基础角色索引（#607）：所有平台都作为可选文件下载，缺它不挡识别。
    #[serde(default)]
    pub character_index: Option<CharacterIndex>,
}
#[derive(Deserialize, Clone)]
pub struct CharacterIndex {
    #[serde(flatten)]
    pub release: crate::character_index::Release,
    pub file: Entry,
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
        if let Some(index) = &mut manifest.character_index {
            if let Some(path) = std::env::var_os("ILLUSTASH_TEST_CHARACTER_INDEX") {
                test_character_index(index, Path::new(&path));
            }
            let mut file = index.file.clone();
            file.optional = true;
            manifest.files.push(file);
        }
        manifest
    })
}

/// 走查和测试用本机合成包换掉清单里的基础角色索引（#607，同 `ILLUSTASH_TEST_CCIP_LONGTAIL`）：文件名和发布版一致，
/// 角色数和大小不超过发布版，只能从 127.0.0.1 的 HTTP 端口下载。只在测试进程里设置，清单无效时直接停下。
fn test_character_index(index: &mut CharacterIndex, path: &Path) {
    #[derive(Deserialize)]
    struct Fixture {
        characters: usize,
        ccip_sha256: String,
        size: u64,
        sha256: String,
        url: String,
    }
    let text = fs::read_to_string(path).expect("读得到基础角色索引测试清单");
    let fixture: Fixture = serde_json::from_str(&text).expect("基础角色索引测试清单有效");
    let local = url::Url::parse(&fixture.url).is_ok_and(|u| {
        u.scheme() == "http" && u.host_str() == Some("127.0.0.1") && u.username().is_empty() && u.port().is_some()
    });
    let hex = |s: &str| s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    assert!(
        local
            && (1..=index.release.characters).contains(&fixture.characters)
            && (1..=index.file.size).contains(&fixture.size)
            && hex(&fixture.sha256)
            && hex(&fixture.ccip_sha256),
        "基础角色索引测试清单无效，下载地址须为 127.0.0.1 的 HTTP 端口"
    );
    index.release.characters = fixture.characters;
    index.release.ccip_sha256 = fixture.ccip_sha256;
    index.file.size = fixture.size;
    index.file.sha256 = fixture.sha256;
    index.file.source = Source::Project { url: fixture.url };
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
/// 校验项目发布的派生文件，允许修复同长度损坏；原始模型保持原有的大小检查。
fn usable(dir: &Path, entry: &Entry) -> bool {
    has(dir, entry)
        && (!matches!(entry.source, Source::Project { .. })
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
/// 识别模型（含基础角色索引）正在下载或导入。
pub(crate) fn downloading() -> bool {
    APP.running.load(Ordering::SeqCst)
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
    /// 参考包的整个 ZIP 都是清单中的文件。
    raw_archive: bool,
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
            raw_archive: false,
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
/// 读数据包目录用的连接（#602）：照下载模型的规矩走代理（本机上的测试服务不走）、跟随跳转（GitHub 的下载地址会跳到
/// 存储服务），整体限时。
pub(crate) fn download_agent(timeout: Duration, address: &str) -> ureq::Agent {
    let local =
        url::Url::parse(address).is_ok_and(|u| matches!(u.host_str(), Some("127.0.0.1" | "localhost" | "[::1]")));
    let mut builder = ureq::AgentBuilder::new().timeout(timeout).redirects(8);
    if let Some(proxy) = proxy().filter(|_| !local).as_deref().and_then(|address| ureq::Proxy::new(address).ok()) {
        builder = builder.proxy(proxy);
    }
    builder.build()
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
                    let d = crate::diagnostics::record(
                        crate::diagnostics::Operation::Models,
                        crate::diagnostics::Stage::Background,
                        crate::diagnostics::Cause::Background,
                        None,
                        crate::diagnostics::Publication::MayBePartial,
                        crate::diagnostics::Retry::CheckState,
                    );
                    job["diagnostic"] = d.value();
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
            // 建连、握手时被断开这类暂时性的错误在同一个来源上再试（#642）；完整的错误写进日志，页面上照旧是一句话。
            Err(ureq::Error::Transport(t)) if transient(&t) => {
                eprintln!("models: {t}");
                return Err(Failure::Retry(transport(&t)));
            }
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
/// 拿到回应之前出的、换个时间再连多半就好的传输错误（#642）：连接或 TLS 握手被断开、被拒（`ConnectionFailed`，ureq 把
/// 握手时对端关掉连接也归在这里），读写出错或超时（`Io`）。走代理、透明代理的网络里偶尔就会这样，同一时刻别的程序照常
/// 下得到。找不到服务器、代理拒绝、跳转过多不算，再试也一样。
pub(crate) fn transient(t: &ureq::Transport) -> bool {
    matches!(t.kind(), ureq::ErrorKind::ConnectionFailed | ureq::ErrorKind::Io)
}

/// 第 `attempt` 次（从 1 起）重试前等多久：1、2、4 秒；单元测试里缩成几十毫秒。
pub(crate) fn backoff(attempt: u32) -> Duration {
    let unit = if cfg!(test) { Duration::from_millis(20) } else { Duration::from_secs(1) };
    unit * 2u32.pow(attempt.saturating_sub(1).min(4))
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
    // 进度只算这次要下载的文件（#607）：只补基础角色索引时，分母是它自己的大小，不是整套模型。已经就位的文件在这里
    // 校验一次，下面的循环沿用结果。
    let needed: Vec<bool> =
        place.manifest.files.iter().map(|e| !(usable(&place.root, e) || usable(&partial, e))).collect();
    let total: u64 = place.manifest.files.iter().zip(&needed).filter(|(_, n)| **n).map(|(e, _)| e.size).sum();
    models.set(|job| job["total"] = total.into());
    let (direct, proxied) = (agent(None), agent(place.proxy.as_deref()));
    let mut meter = Meter::new(models, 0);
    let mut originals_active = false;
    for (entry, needed) in place.manifest.files.iter().zip(needed) {
        if entry.optional && !originals_active {
            if !ready_at(&place.root, place.manifest) {
                activate(&place.root, &partial, place.manifest)?;
            }
            fs::create_dir_all(&partial)?;
            check_space(&partial, missing_bytes(true))?;
            originals_active = true;
        }
        if !needed {
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
            // 中途断开的连接从停下的地方接着下，没连上的重新连，在同一个来源上重试几次，每次多等一会儿（#642）。
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
                        let until = Instant::now() + backoff(drops);
                        while let Some(left) = until.checked_duration_since(Instant::now()).filter(|d| !d.is_zero()) {
                            if models.stopped() {
                                return Err(Stop::Cancelled);
                            }
                            std::thread::sleep(left.min(Duration::from_millis(50)));
                        }
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
    import_into(models, std::slice::from_ref(place), paths, &[])
}

/// 同一种数据包的几个版本一起认（#602）：每个所选文件或 ZIP 成员按大小、SHA-256 归到它所属的版本，各自核对、改名就位。
/// `notes` 是事先处理过的成员（离线包里的目录和签名，以及目录没列出的附带文件 `extra`）的结果，原样列进结果。
/// 参考包（`raw_archive`）本身是 ZIP：整个文件对得上某个版本时整份接收，否则当作离线包读里面的成员。
fn import_into(
    models: &Models,
    places: &[Place],
    paths: &[PathBuf],
    notes: &[(String, &'static str)],
) -> std::result::Result<Value, Stop> {
    let partials = places.iter().map(|place| partial_dir(&place.root)).collect::<Vec<_>>();
    for partial in &partials {
        fs::create_dir_all(partial)?;
    }
    let raw_archive = places.iter().any(|place| place.raw_archive);
    let wanted = |size: u64| {
        places
            .iter()
            .enumerate()
            .find_map(|(i, place)| place.manifest.files.iter().find(|e| e.size == size).map(|e| (i, e)))
    };
    let mut results = Vec::new();
    let mut meter = Meter::new(models, 0);
    let mut take =
        |name: String, size: u64, reader: &mut dyn Read, meter: &mut Meter| -> std::result::Result<(), Stop> {
            if let Some((_, result)) = notes.iter().find(|(noted, _)| *noted == name) {
                results.push(json!({"name": name, "result": result, "path": null}));
                return Ok(());
            }
            let result = match wanted(size) {
                None => "unknown",
                Some((i, entry))
                    if !places[i].raw_archive && (usable(&places[i].root, entry) || usable(&partials[i], entry)) =>
                {
                    "already"
                }
                Some((i, entry)) => {
                    let place = &places[i];
                    let installed = place.raw_archive
                        && crate::recognition_compute::models::hash(&place.root.join(&entry.path), &|| Ok(()))
                            .is_ok_and(|sha| sha == entry.sha256);
                    models.set(|job| job["file"] = entry.path.clone().into());
                    let ok = copy_checked(reader, &partials[i], entry, meter)?;
                    meter.finish(entry.size);
                    if ok && installed {
                        "already"
                    } else if ok {
                        "accepted"
                    } else {
                        "mismatch"
                    }
                }
            };
            results.push(json!({"name": name, "result": result, "path": wanted(size).map(|(_, e)| e.path.clone())}));
            Ok(())
        };
    for path in paths {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let unreadable = |e: std::io::Error| Stop::Failed(format!("读不到所选文件 {name}：{error}", error = e));
        let metadata = fs::metadata(path).map_err(unreadable)?;
        let whole = raw_archive && metadata.is_file() && wanted(metadata.len()).is_some();
        let zip = !whole && path.extension().is_some_and(|x| x.eq_ignore_ascii_case("zip"));
        if zip {
            let file = fs::File::open(path).map_err(unreadable)?;
            let mut archive =
                zip::ZipArchive::new(file).map_err(|_| Stop::Failed(format!("{name} 不是有效的 ZIP 文件")))?;
            let unreadable_entry = |error| Stop::Failed(format!("{name} 读取失败：{error}"));
            for i in 0..archive.len() {
                let mut item = archive.by_index(i).map_err(unreadable_entry)?;
                if !item.is_file() {
                    continue;
                }
                let inner = format!("{name}/{}", item.name().map_err(unreadable_entry)?);
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
    for (place, partial) in places.iter().zip(&partials) {
        let manifest = place.manifest;
        let complete = manifest.files.iter().filter(|e| !e.optional).all(|e| has(&place.root, e) || has(partial, e));
        let touched = manifest.files.iter().any(|e| has(partial, e));
        if complete && (touched || places.len() == 1) {
            activate(&place.root, partial, manifest)?;
        } else if !touched && places.len() > 1 && fs::read_dir(partial).is_ok_and(|mut d| d.next().is_none()) {
            // 这次没有用到的版本不留空的 .partial 文件夹。
            let _ = fs::remove_dir(partial);
        }
    }
    Ok(json!({"results": results}))
}

// ---------- 状态和请求 ----------

fn info_at(models: &Models, place: &Place) -> Json {
    package_info(place, || models.snapshot())
}

fn package_info(place: &Place, job: impl FnOnce() -> Value) -> Json {
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
    let mut sources = Vec::new();
    for entry in &manifest.files {
        for (name, _) in place.sources(entry) {
            if !sources.contains(&name) {
                sources.push(name);
            }
        }
    }
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
        "sources": sources,
        "manual": manifest.manual,
        "files": manifest.files.iter().map(|e| json!({"path": e.path, "size": e.size, "sha256": e.sha256, "optional": e.optional})).collect::<Vec<_>>(),
        "job": job(),
    }))
}

/// 正在进行或刚结束的下载、导入（`GET /api/models` 的 `job`），只读内存；桌面外壳用它（#451）。
pub(crate) fn job() -> Value {
    APP.snapshot()
}

/// GET /api/models.
pub(crate) fn info(resources: &Path) -> Result<Json> {
    let place = Place::app(resources)?;
    Ok(with_acceleration(resources, &place, info_at(&APP, &place)))
}

static CCIP_LONGTAIL: Models = Models::new();

const LONGTAIL_MANUAL: &str =
    "https://github.com/shio-vinyl/illustash/blob/main/docs/INSTALL.md#可选的-ccip-长尾角色参考包";

/// 某个版本的参考包放在共享数据的 `extensions/<版本>/` 下（内置的是 `extensions/ccip-longtail-v1/`）。
fn longtail_place(resources: &Path, pack: &crate::app_packs::Pack) -> Result<Place> {
    let shared = crate::app_resources::shared_path(resources)?;
    Ok(Place {
        root: crate::app_ccip_longtail::package_path(&shared, pack).parent().expect("参考包有存放目录").into(),
        manifest: pack_manifest(pack, LONGTAIL_MANUAL)?,
        preferences: PathBuf::new(),
        modelscope: String::new(),
        proxy: None,
        on_demand: true,
        in_use: false,
        raw_archive: true,
    })
}

/// `GET /api/ccip-longtail` 的 `package`：说的是卡片上的那个版本。
pub(crate) fn longtail_package(resources: &Path, pack: &crate::app_packs::Pack) -> Result<Json> {
    // 下载状态可能在文件改名后进入终态，响应以取得任务快照时序为准。
    let job = CCIP_LONGTAIL.snapshot();
    Ok(package_info(&longtail_place(resources, pack)?, || job))
}

/// `POST /api/ccip-longtail/package`，动作同 `extension_post`。
pub(crate) fn longtail_package_post(
    resources: &Path,
    body: &Json,
    in_use: bool,
    current: &crate::app_packs::Pack,
) -> Result<()> {
    if body.get("action").map(Json::text) == Some("delete") && in_use {
        return Err(Error::Conflict("正在检查角色，请等任务结束后再删除参考包".into()));
    }
    pack_post(resources, body, current, "未知操作")
}

/// 「使用 GPU 加速识别」（#270）在模型卡片上：这个构建有没有加速器、开关（POST /api/preferences），
/// 以及上一个 worker 每个模型用的是什么（recognition_compute/backend.rs）。
fn with_acceleration(resources: &Path, place: &Place, mut value: Json) -> Json {
    let shared = crate::app_resources::shared_path(resources).ok();
    value.set("acceleration", Json::from(acceleration_at(shared.as_deref(), place)));
    value
}
/// 加速状态，去掉 WD 加速文件装好之前留下的那条 WD 回退记录（#375）：它说的是装好之前的文件，下一次识别会写新的。
/// 只读 acceleration.json，不改它（那是 worker 写的文件）。
fn acceleration_at(shared: Option<&Path>, place: &Place) -> Value {
    let mut value = crate::recognition_compute::backend::info(shared);
    if let Some(last) = value.get_mut("last").and_then(Value::as_object_mut) {
        drop_superseded_wd(last, variant_installed(&place.root, place.manifest));
    }
    value
}
/// WD 加速文件装好的时间（可选文件里只看它，基础角色索引不算）：下载和导入都先在 `.partial` 里写出新文件，校验后改名进根目录（`activate`），
/// 所以它的修改时间就是装好的时间，与所选文件原来的时间无关；同长度损坏的修复同样写出新文件。
fn variant_installed(root: &Path, manifest: &Manifest) -> Option<SystemTime> {
    let index = manifest.character_index.as_ref().map(|c| c.file.path.as_str());
    manifest
        .files
        .iter()
        .filter(|e| e.optional && Some(e.path.as_str()) != index && has(root, e))
        .filter_map(|e| fs::metadata(root.join(&e.path)).and_then(|m| m.modified()).ok())
        .max()
}
/// WD 的回退记录（有原因的那种）写于 `installed` 之前时去掉。记录的 `at` 只精确到秒：与装好时间同一秒的算作装好之后写的，
/// 照常报告。CCIP 的记录不看。
fn drop_superseded_wd(last: &mut serde_json::Map<String, Value>, installed: Option<SystemTime>) {
    let Some(installed) = installed.and_then(|t| t.duration_since(UNIX_EPOCH).ok()) else { return };
    let Some(wd) = last.get("wd") else { return };
    let fell_back = wd.get("reason").is_some_and(|r| !r.is_null());
    if fell_back && wd.get("at").and_then(Value::as_u64).unwrap_or(0) < installed.as_secs() {
        last.remove("wd");
    }
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
    let first = !ready_at(&place.root, place.manifest);
    let value = post_at(&APP, place.clone(), body).map(|value| with_acceleration(resources, &place, value))?;
    // 首次准备时角色资料包和识别模型一起下载（2026-11-17 用户定，#602）：用户点了下载识别模型，就把最新的角色资料包里
    // 还没就位的文件一起下。它失败不影响模型下载，卡片上照样能重试。
    if body.get("action").map(Json::text) == Some("download") {
        if first {
            download_pack_after_catalog(resources);
        } else if let Err(error) = download_newest_pack(resources) {
            eprintln!("角色资料包没能和识别模型一起开始下载：{error}");
        }
    }
    Ok(value)
}

/// 首次准备时正在读包目录（`download_pack_after_catalog`），资料包的下载还没开始。
static READING_CATALOG: AtomicBool = AtomicBool::new(false);

/// 首次准备（识别模型还没就位时点了下载）先读一次包目录，再下载目录里最新的、这个版本能用的资料包（2026-11-18 用户定，
/// #630）：读法、签名校验、缓存和记录都和检查更新时一样（`app_updates::refresh_packs`）。读不到（断网、签名不对、没有能用
/// 的条目）时照常下载本机已知的最新版本，没有缓存的目录就是内置的 0.1。在后台线程里做，不耽误模型下载开始；读目录期间
/// `GET /api/packs` 的 `download` 报 `catalog`，页面照常轮询。
fn download_pack_after_catalog(resources: &Path) {
    if READING_CATALOG.swap(true, Ordering::SeqCst) {
        return;
    }
    let resources = resources.to_path_buf();
    let spawned = std::thread::Builder::new().name("pack-catalog".into()).spawn(move || {
        crate::app_updates::refresh_packs(&resources);
        if let Err(error) = download_newest_pack(&resources) {
            eprintln!("角色资料包没能和识别模型一起开始下载：{error}");
        }
        // 下载开始以后才放下：页面读到的状态从「读目录」直接接上「下载中」。
        READING_CATALOG.store(false, Ordering::SeqCst);
    });
    if let Err(error) = spawned {
        READING_CATALOG.store(false, Ordering::SeqCst);
        eprintln!("没能开始读取角色资料包目录：{error}");
    }
}

fn download_newest_pack(resources: &Path) -> Result<()> {
    let Some(pack) = crate::app_packs::known(resources)?.pop() else { return Ok(()) };
    for kind in [PackKind::Wd, PackKind::Ccip] {
        if !models_of(kind).running.load(Ordering::SeqCst)
            && !start_download(resources, pack.component(kind), &pack.version)?
        {
            forget_finished(kind);
        }
    }
    Ok(())
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
            if ready_at(&place.root, place.manifest)
                && !place.manifest.files.iter().any(|e| e.optional || matches!(e.source, Source::Project { .. }))
            {
                return Err(Error::Invalid("识别模型已经就位".into()));
            }
            // 先按大小估一个总量，下载线程校验过已有文件后改成准确的（见 `download`）。
            let partial = partial_dir(&place.root);
            let total = place
                .manifest
                .files
                .iter()
                .filter(|e| !(has(&place.root, e) || has(&partial, e)))
                .map(|e| e.size)
                .sum();
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

#[derive(Deserialize)]
pub(crate) struct WdExtension {
    pub revision: u32,
    pub format: String,
    pub heads: usize,
    pub license: String,
    pub base_sha256: String,
    /// WD 标签表的 SHA-256。models.json 不写（就是随包的 tags_v3.csv）；走查清单可以写合成标签表的（#602 判断目录里
    /// 的包和这个构建的模型对不对得上时用）。
    #[serde(default)]
    pub tags_sha256: Option<String>,
    pub file: Entry,
}

pub(crate) fn wd_extension() -> Result<&'static WdExtension> {
    #[derive(Deserialize)]
    struct Document {
        wd_extension: WdExtension,
    }
    static SPEC: OnceLock<WdExtension> = OnceLock::new();
    static FIXTURE: OnceLock<std::result::Result<WdExtension, String>> = OnceLock::new();
    let spec = SPEC.get_or_init(|| {
        serde_json::from_str::<Document>(include_str!("../models.json"))
            .expect("models.json 含有 WD 扩展包清单")
            .wd_extension
    });
    // 走查只替换冻结文件的身份和下载来源，目录和发布版一致。
    let Some(path) = std::env::var_os("ILLUSTASH_TEST_WD_EXTENSION") else { return Ok(spec) };
    FIXTURE
        .get_or_init(|| {
            let text = fs::read_to_string(path).map_err(|e| format!("读不到 WD 扩展包测试清单：{e}"))?;
            let fixture: WdExtension =
                serde_json::from_str(&text).map_err(|e| format!("WD 扩展包测试清单无效：{e}"))?;
            let local = matches!(&fixture.file.source, Source::Project { url } if
                url::Url::parse(url).is_ok_and(|u| u.scheme() == "http" && u.host_str() == Some("127.0.0.1")
                    && u.username().is_empty() && u.password().is_none() && u.port().is_some()));
            if !local
                || fixture.file.path != spec.file.path
                || fixture.revision != spec.revision
                || fixture.format != spec.format
                || !(1..=spec.heads).contains(&fixture.heads)
                || !(1..=spec.file.size).contains(&fixture.file.size)
            {
                return Err("WD 扩展包测试清单无效，下载地址须为 127.0.0.1 的 HTTP 端口".into());
            }
            Ok(fixture)
        })
        .as_ref()
        .map_err(|message| Error::invalid(message.clone()))
}

const WD_EXTENSION_MANUAL: &str = "https://github.com/shio-vinyl/illustash/blob/main/docs/INSTALL.md#角色资料包";
static WD_EXTENSION: Models = Models::new();
/// 两种数据包各自正在下载或导入的是哪个版本（#602）；下载状态（`download`）是整个种类共用的一个。
static PACK_JOBS: Mutex<[Option<String>; 2]> = Mutex::new([None, None]);

fn job_slot(kind: crate::app_packs::Kind) -> usize {
    match kind {
        crate::app_packs::Kind::Wd => 0,
        crate::app_packs::Kind::Ccip => 1,
    }
}

/// 这一种数据包最近一次下载或导入针对的版本（`GET /api/packs` 据此把下载进度算到「新版本」那一行）。
pub(crate) fn pack_job_version(kind: crate::app_packs::Kind) -> Option<String> {
    PACK_JOBS.lock().expect("数据包任务锁中毒")[job_slot(kind)].clone()
}

fn note_pack_job(kind: crate::app_packs::Kind, version: Option<String>) {
    PACK_JOBS.lock().expect("数据包任务锁中毒")[job_slot(kind)] = version;
}

/// 清单条目的文件名、大小、SHA-256 和下载地址（项目 Release 的派生文件才有地址）。
pub(crate) fn entry_parts(entry: &Entry) -> (String, u64, String, String) {
    let url = match &entry.source {
        Source::Project { url } => url.clone(),
        Source::Upstream { .. } => String::new(),
    };
    (entry.path.clone(), entry.size, entry.sha256.clone(), url)
}

/// 共享数据里放数据包的文件夹：WD 角色扩展包、各版本的数据包和包目录的本机缓存都在这里（#602）。
pub(crate) fn extensions_root(resources: &Path) -> Result<PathBuf> {
    let shared = if std::env::var_os("ILLUSTASH_SHARED").is_some() {
        crate::app_resources::shared_path(resources)?
    } else {
        let home = crate::app_paths::user_home()?;
        let support = if cfg!(windows) && std::env::var_os("ILLUSTASH_APP_SUPPORT").is_none() {
            std::env::var_os("LOCALAPPDATA")
                .filter(|path| !path.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join("AppData").join("Local"))
                .join("Illustash")
        } else {
            crate::app_paths::app_support(&home)
        };
        support.join("Shared")
    };
    Ok(shared.join("extensions"))
}

/// 下载清单要活到进程结束（`Place` 借用它）。每个版本只建一次。
fn pack_manifest(pack: &crate::app_packs::Pack, manual: &str) -> Result<&'static Manifest> {
    static MANIFESTS: Mutex<Vec<&'static Manifest>> = Mutex::new(Vec::new());
    // 目录里的地址是 GitHub Release 的；测试时换到本机测试服务（内置包的走查清单本来就指向本机）。
    let url = if pack.builtin {
        pack.file.url.clone()
    } else {
        crate::app_updates::asset_url(&pack.file.url).map_err(Error::Invalid)?
    };
    let mut known = MANIFESTS.lock().expect("数据包清单锁中毒");
    if let Some(found) = known.iter().find(|m| {
        m.pack == pack.version
            && m.files.first().is_some_and(|e| {
                e.sha256 == pack.file.sha256 && matches!(&e.source, Source::Project { url: u } if *u == url)
            })
    }) {
        return Ok(found);
    }
    let manifest: &'static Manifest = Box::leak(Box::new(Manifest {
        format: 1,
        pack: pack.version.clone(),
        manual: manual.into(),
        mirror: String::new(),
        modelscope: String::new(),
        files: vec![Entry {
            path: pack.file.name.clone(),
            size: pack.file.size,
            sha256: pack.file.sha256.clone(),
            source: Source::Project { url },
            optional: false,
        }],
        wd_coreml: None,
        character_index: None,
    }));
    known.push(manifest);
    Ok(manifest)
}

fn extension_place(resources: &Path, pack: &crate::app_packs::Pack) -> Result<Place> {
    Ok(Place {
        root: extensions_root(resources)?.join(&pack.version),
        manifest: pack_manifest(pack, WD_EXTENSION_MANUAL)?,
        preferences: PathBuf::new(),
        modelscope: String::new(),
        proxy: None,
        on_demand: true,
        in_use: false,
        raw_archive: false,
    })
}

/// 这个版本的 WD 角色扩展包已经就位（核对过 SHA-256）时，它的路径。
pub(crate) fn extension_file(resources: &Path, pack: &crate::app_packs::Pack) -> Result<Option<PathBuf>> {
    let place = extension_place(resources, pack)?;
    let entry = &place.manifest.files[0];
    Ok(usable(&place.root, entry).then(|| place.root.join(&entry.path)))
}

/// 这个版本的包文件是不是已经在本机（只看大小，读状态时用；用的时候再核对 SHA-256）。
pub(crate) fn extension_present(resources: &Path, pack: &crate::app_packs::Pack) -> Result<bool> {
    let place = extension_place(resources, pack)?;
    Ok(has(&place.root, &place.manifest.files[0]))
}

/// `GET /api/wd-extension` 的 `package` 和 `download`：说的是卡片上的那个版本（`pack`）。
pub(crate) fn extension_info(resources: &Path, pack: &crate::app_packs::Pack) -> Result<Value> {
    // 先读任务快照；看到下载已结束时才开始确认激活后的文件。
    let download = WD_EXTENSION.snapshot();
    let place = extension_place(resources, pack)?;
    let entry = &place.manifest.files[0];
    let ready = usable(&place.root, entry);
    let partial = partial_dir(&place.root);
    let have = if ready || usable(&partial, entry) {
        entry.size
    } else {
        fs::metadata(part(&partial, entry)).map_or(0, |m| m.len().min(entry.size))
    };
    Ok(json!({
        "package": {
            "version":place.manifest.pack, "heads":pack.heads, "license":pack.license,
            "file":{"path":entry.path,"size":entry.size,"sha256":entry.sha256},
            "manual":place.manifest.manual, "location":place.root,
            "status":if ready {"ready"} else if have > 0 {"partial"} else {"missing"},
            "have":have,"partial":!ready && have > 0,
            "sources":place.sources(entry).into_iter().map(|(host, _)| host).collect::<Vec<_>>()
        },
        "download":download
    }))
}

use crate::app_packs::{CharacterPack, Kind as PackKind, Pack};

fn place_of(resources: &Path, pack: &Pack) -> Result<Place> {
    match pack.kind {
        PackKind::Wd => extension_place(resources, pack),
        PackKind::Ccip => longtail_place(resources, pack),
    }
}

fn models_of(kind: PackKind) -> &'static Models {
    match kind {
        PackKind::Wd => &WD_EXTENSION,
        PackKind::Ccip => &CCIP_LONGTAIL,
    }
}

/// 一个组成部分下载了多少：`(就位, 已有字节, 有没下完的部分)`。
fn component_progress(resources: &Path, pack: &Pack) -> Result<(bool, u64, bool)> {
    let place = place_of(resources, pack)?;
    let entry = &place.manifest.files[0];
    let partial = partial_dir(&place.root);
    let ready = has(&place.root, entry);
    let started = !ready && (has(&partial, entry) || part(&partial, entry).is_file());
    let have = if ready || has(&partial, entry) {
        entry.size
    } else {
        fs::metadata(part(&partial, entry)).map_or(0, |m| m.len().min(entry.size))
    };
    Ok((ready, have, started))
}

/// 角色资料包的两个文件下载了多少（`GET /api/packs`）：`status` 为 `ready`（两个都就位）、`partial`（有下了一半的
/// 文件）或 `missing`。`have` 也算上已经在本机的那个文件（新版本只换了一个组成部分时，另一个不用再下）。
pub(crate) fn pack_progress(resources: &Path, pack: &CharacterPack) -> Result<Value> {
    let (wd_ready, wd_have, wd_started) = component_progress(resources, &pack.wd)?;
    let (ccip_ready, ccip_have, ccip_started) = component_progress(resources, &pack.ccip)?;
    let status = if wd_ready && ccip_ready {
        "ready"
    } else if wd_started || ccip_started {
        "partial"
    } else {
        "missing"
    };
    Ok(json!({"status":status,"have":wd_have + ccip_have}))
}

/// 两个组成部分的下载或导入合成一个状态（`GET /api/packs` 的 `download`）：有一个在进行就算进行中，字节数相加；
/// 都结束了，有一个失败就算失败；导入结果按文件名合并，一边认出、一边不认识的算认出。都没有过时为 null。
pub(crate) fn pack_download() -> Value {
    let jobs = [WD_EXTENSION.snapshot(), CCIP_LONGTAIL.snapshot()];
    let live = jobs.iter().filter(|j| !j.is_null()).collect::<Vec<_>>();
    if READING_CATALOG.load(Ordering::SeqCst) && !live.iter().any(|j| j["status"] == "running") {
        return json!({"kind":"catalog","status":"running","done":0,"total":0,"speed":0,"source":null,"proxy":null,
            "message":null,"failures":[],"results":[],"diagnostic":null,"version":null});
    }
    let Some(first) = live.first() else { return Value::Null };
    let status = |s: &str| live.iter().any(|j| j["status"] == s);
    let sum = |key: &str| live.iter().map(|j| j[key].as_u64().unwrap_or(0)).sum::<u64>();
    let mut results: Vec<Value> = Vec::new();
    for job in &live {
        for r in job["results"].as_array().into_iter().flatten() {
            match results.iter_mut().find(|x| x["name"] == r["name"]) {
                Some(x) if x["result"] == "unknown" => *x = r.clone(),
                Some(_) => {}
                None => results.push(r.clone()),
            }
        }
    }
    let failed = live.iter().find(|j| j["status"] == "failed");
    let state = if status("running") {
        "running"
    } else if failed.is_some() {
        "failed"
    } else if status("cancelled") {
        "cancelled"
    } else {
        "done"
    };
    let failures = live.iter().flat_map(|j| j["failures"].as_array().cloned().unwrap_or_default()).collect::<Vec<_>>();
    json!({
        "kind":first["kind"],"status":state,"done":sum("done"),"total":sum("total"),"speed":sum("speed"),
        "source":live.iter().find(|j| j["status"] == "running").map_or(first["source"].clone(), |j| j["source"].clone()),
        "proxy":first["proxy"],"message":failed.map_or(Value::Null, |j| j["message"].clone()),
        "failures":failures,"results":results,"diagnostic":failed.map_or(Value::Null, |j| j["diagnostic"].clone()),
        "version":pack_job_version(PackKind::Wd).or_else(|| pack_job_version(PackKind::Ccip)),
    })
}

/// 一种组成部分在所有可用版本里的文件位置（同一个文件只算一次），导入时一起认。
fn places_of(resources: &Path, kind: PackKind) -> Result<Vec<Place>> {
    let mut places: Vec<Place> = Vec::new();
    for pack in crate::app_packs::known(resources)? {
        let place = place_of(resources, pack.component(kind))?;
        if !places.iter().any(|p| p.root == place.root) {
            places.push(place);
        }
    }
    Ok(places)
}

fn import_paths(body: &Json) -> Result<Vec<PathBuf>> {
    let Json::Object(fields) = body else { return Err(Error::invalid("未知操作")) };
    if fields.iter().any(|(k, _)| k != "action" && k != "paths") {
        return Err(Error::invalid("未知参数"));
    }
    match body.get("paths") {
        Some(Json::Array(list)) if !list.is_empty() && list.len() <= 64 => list
            .iter()
            .map(|p| match p {
                Json::Scalar(Value::String(p)) if !p.is_empty() => Ok(PathBuf::from(p)),
                _ => Err(Error::Invalid("文件路径不正确".into())),
            })
            .collect(),
        _ => Err(Error::Invalid("请选择要导入的文件".into())),
    }
}

/// 同一种组成部分只有一个工作线程。在发布任务终态前，清掉零字节断点文件和空目录，留下可续传的数据。
fn cleanup_pack_partials(places: &[Place]) {
    for place in places {
        let partial = partial_dir(&place.root);
        for entry in &place.manifest.files {
            let path = part(&partial, entry);
            if fs::symlink_metadata(&path).is_ok_and(|m| m.is_file() && m.len() == 0) {
                let _ = fs::remove_file(path);
            }
        }
        let _ = fs::remove_dir(partial);
    }
}

/// 从文件导入：离线包里的目录先验签采用（`app_packs::adopt_archives`），再按采用后的目录，让 `kinds` 里每种组成部分
/// 各自的导入任务认所选的文件。认不出的文件在各自的结果里是 `unknown`，页面合并时以认出的那边为准。
fn start_import(resources: &Path, kinds: &[PackKind], paths: Vec<PathBuf>) -> Result<()> {
    let notes = crate::app_packs::adopt_archives(resources, &paths);
    let total = paths.iter().filter_map(|p| fs::metadata(p).ok()).map(|m| m.len()).sum();
    for &kind in kinds {
        let places = places_of(resources, kind)?;
        let (paths, notes) = (paths.clone(), notes.clone());
        models_of(kind).start("import", total, move |models| {
            let outcome = import_into(models, &places, &paths, &notes);
            cleanup_pack_partials(&places);
            outcome
        })?;
        note_pack_job(kind, None);
    }
    Ok(())
}

/// 下载一个组成部分（已经就位时什么都不做），记下它属于哪个版本。
fn start_download(resources: &Path, component: &Pack, version: &str) -> Result<bool> {
    let mut place = place_of(resources, component)?;
    let entry = &place.manifest.files[0];
    if usable(&place.root, entry) {
        return Ok(false);
    }
    place.proxy = proxy();
    let total = if has(&partial_dir(&place.root), entry) { 0 } else { entry.size };
    models_of(component.kind).start("download", total, move |models| {
        let outcome = download(models, &place);
        cleanup_pack_partials(std::slice::from_ref(&place));
        outcome
    })?;
    note_pack_job(component.kind, Some(version.to_owned()));
    Ok(true)
}

/// 这一批下载没有启动这个组成部分（它已经在本机）：清掉它上一次下载或导入留下的结果。不然 `pack_download` 会把它合进这一批，
/// 进度的分母多出上次的字节（#643：本机已有 0.3 时下载 0.4，按钮写约 7.8 MB，进度却是 … / 10.0 MB），种类也可能取成
/// 上次的导入。
fn forget_finished(kind: PackKind) {
    let models = models_of(kind);
    if !models.running.load(Ordering::SeqCst) {
        *models.job.lock().expect("models job mutex poisoned") = None;
        note_pack_job(kind, None);
    }
}

fn busy_message(kind: PackKind) -> (&'static str, &'static str) {
    match kind {
        PackKind::Wd => ("扩展包正在下载或导入，请等它结束", "扩展包已经就位"),
        PackKind::Ccip => ("参考包正在下载或导入，请等它结束", "参考包已经就位"),
    }
}

/// `POST /api/packs` 的文件操作（#602，一个角色资料包）：
/// - `download` 下载卡片上的版本，带 `version` 时下载那个版本（「有新版本」）；两个文件里已经就位的不再下载；
/// - `import` 一起认所选文件里的两种组成部分（`paths`，页面不带时宿主先打开选择框）；
/// - `cancel` 停下两边正在进行的下载或导入；`discard` 删掉卡片上的版本没下完的部分；`delete` 删掉它的两个文件。
pub(crate) fn character_pack_post(resources: &Path, body: &Json, current: &CharacterPack) -> Result<()> {
    let action = body.get("action").map(Json::text).unwrap_or("");
    let kinds = [PackKind::Wd, PackKind::Ccip];
    if action != "cancel" && kinds.iter().any(|&k| models_of(k).running.load(Ordering::SeqCst)) {
        return Err(Error::Conflict("角色资料包正在下载或导入，请等它结束".into()));
    }
    let Json::Object(fields) = body else { return Err(Error::invalid("未知操作")) };
    let only = |allowed: &[&str]| fields.iter().all(|(k, _)| allowed.contains(&k.as_str()));
    match action {
        "download" if only(&["action", "version"]) => {
            let pack = match body.get("version") {
                None => current.clone(),
                Some(Json::Scalar(Value::String(v))) if !v.is_empty() => crate::app_packs::by_version(resources, v)?,
                Some(_) => return Err(Error::invalid("未知参数")),
            };
            let mut started = false;
            for kind in kinds {
                if start_download(resources, pack.component(kind), &pack.version)? {
                    started = true;
                } else {
                    forget_finished(kind);
                }
            }
            if !started {
                return Err(Error::Conflict("角色资料包已经就位".into()));
            }
        }
        "import" => start_import(resources, &kinds, import_paths(body)?)?,
        "cancel" if only(&["action"]) => {
            for kind in kinds {
                models_of(kind).cancel.store(true, Ordering::SeqCst);
            }
        }
        "discard" | "delete" if only(&["action"]) => {
            for kind in kinds {
                post_at(models_of(kind), place_of(resources, current.component(kind))?, body)?;
            }
        }
        "download" | "cancel" | "discard" | "delete" => return Err(Error::invalid("未知参数")),
        _ => return Err(Error::invalid("未知的角色资料包操作")),
    }
    Ok(())
}

/// `POST /api/wd-extension/package`、`/api/ccip-longtail/package`：只针对一个组成部分（#550、#559 的旧路由，保留）。
/// `download` 带 `version` 时下载那个角色资料包版本里的这个组成部分。
pub(crate) fn extension_post(resources: &Path, body: &Json, current: &Pack) -> Result<()> {
    pack_post(resources, body, current, "未知的 WD 角色扩展包操作")
}

fn pack_post(resources: &Path, body: &Json, current: &Pack, unknown: &str) -> Result<()> {
    let kind = current.kind;
    let models = models_of(kind);
    let action = body.get("action").map(Json::text).unwrap_or("");
    if !matches!(action, "download" | "import" | "cancel" | "discard" | "delete") {
        return Err(Error::invalid(unknown));
    }
    let (busy, ready) = busy_message(kind);
    if action != "cancel" && models.running.load(Ordering::SeqCst) {
        return Err(Error::Conflict(busy.into()));
    }
    let Json::Object(fields) = body else { return Err(Error::invalid("未知操作")) };
    let version = match fields.iter().find(|(k, _)| k == "version").map(|(_, v)| v) {
        None => None,
        Some(Json::Scalar(Value::String(v))) if action == "download" && !v.is_empty() => Some(v.clone()),
        Some(_) => return Err(Error::invalid("未知参数")),
    };
    let rest = Json::Object(fields.iter().filter(|(k, _)| k != "version").cloned().collect());
    match action {
        "download" => {
            let (component, label) = match &version {
                Some(version) => {
                    (crate::app_packs::by_version(resources, version)?.component(kind).clone(), version.clone())
                }
                None => (current.clone(), current.version.clone()),
            };
            if !start_download(resources, &component, &label)? {
                return Err(Error::Conflict(ready.into()));
            }
        }
        "import" => start_import(resources, &[kind], import_paths(&rest)?)?,
        "cancel" => models.cancel.store(true, Ordering::SeqCst),
        _ => post_at(models, place_of(resources, current)?, &rest).map(|_| ())?,
    }
    Ok(())
}

#[cfg(test)]
mod tests;
