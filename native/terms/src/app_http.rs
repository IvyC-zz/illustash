//! Default native package host with explicit creation and upgrades.
use crate::{app_library::Library, http, http_json::Json, Error, Result};
use std::{
    io::Read,
    net::TcpListener,
    path::{Path, PathBuf},
    sync::{atomic::Ordering, Arc},
    thread,
    time::{Duration, Instant},
};
use tiny_http::{Request, Server};

#[path = "http_host_get.rs"]
mod http_host_get;
#[path = "http_host_post.rs"]
mod http_host_post;

const FORCED_OFF: [&str; 2] = ["similar", "sync"];
/// Connections served at once (#25). A browser keeps about six per origin, so several windows stay far below it.
const MAX_CONNECTIONS: usize = 64;
/// Request handlers running at once; pipelined requests on one connection each take one.
const MAX_HANDLERS: usize = 128;

/// The local HTTP server with per-connection bounds (#25): a client that stops sending (or reading) loses its
/// connection after the timeout, and connections beyond the limit are refused with 503, so a slow or stalled
/// client cannot hold threads without bound. `ILLUSTASH_HTTP_TIMEOUT_MS` shortens the timeout for tests.
fn serve(listener: TcpListener) -> Result<Server> {
    let timeout = std::env::var("ILLUSTASH_HTTP_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|ms| *ms > 0)
        .map_or(Duration::from_secs(30), Duration::from_millis);
    let limits = tiny_http::Limits {
        read_timeout: Some(timeout),
        write_timeout: Some(timeout),
        max_connections: Some(MAX_CONNECTIONS),
    };
    Server::from_listener_with_limits(listener, None, limits).map_err(|e| Error::Invalid(e.to_string()))
}

use crate::app_service::{truthy, Application, Options, Session};
struct Host {
    application: Application,
    origin: String,
}
impl std::ops::Deref for Host {
    type Target = Application;
    fn deref(&self) -> &Application {
        &self.application
    }
}
impl Session {
    fn context(&self, tags: &std::path::Path) -> http::LibraryContext {
        http::LibraryContext {
            db: self.library.db(),
            generation: self.token.clone(),
            library: Some(self.library.instance_path().to_owned()),
            tags: tags.to_owned(),
            source: Some(self.library.source()),
            token: Some(self.token.clone()),
            read_only: None,
        }
    }
}
fn header<'a>(request: &'a Request, name: &str) -> Option<&'a str> {
    request.headers().iter().find(|h| h.field.as_str().as_str().eq_ignore_ascii_case(name)).map(|h| h.value.as_str())
}
fn error(message: &str) -> Json {
    Json::object([("error", message.into())])
}
fn respond(request: Request, payload: Json, status: u16, route: &str) {
    match http::json_response(&request, &payload, status, route) {
        Ok(response) => {
            let _ = request.respond(response);
        }
        Err(e) => eprintln!("HTTP response: {e}"),
    }
}
fn failure(request: Request, err: Error, write: bool) {
    let (payload, status) = match err {
        Error::Invalid(message) => (error(&message), 400),
        Error::Network(message) => (error(&message), 502),
        Error::Conflict(message) => (error(&message), 409),
        err => {
            eprintln!("HTTP native: {err}");
            (
                error(if write {
                    "保存失败，未提交的操作已回滚；请查看启动终端"
                } else {
                    "读取失败，请重试；详情请查看启动终端"
                }),
                500,
            )
        }
    };
    respond(request, payload, status, "core");
}
fn unavailable(request: Request) {
    respond(request, error("接口或可选研究任务不可用"), 502, "unavailable");
}
fn forced_off(
    library: &Library,
    names_enabled: bool,
    sync_disabled: bool,
    provenance_enabled: bool,
    index_automatic: bool,
) -> Vec<String> {
    let mut result: Vec<String> = FORCED_OFF
        .iter()
        .filter(|key| {
            (**key != "similar" || !crate::app_research::configured())
                && (**key != "sync" || sync_disabled || !crate::app_sync::Syncer::available(library))
        })
        .map(|s| s.to_string())
        .collect();
    if !index_automatic {
        result.insert(0, "index_check".into());
    }
    if !provenance_enabled || std::env::var_os("ILLUSTASH_PROVENANCE_WORKER").is_none() {
        result.insert(if index_automatic { 0 } else { 1 }, "provenance".into());
    }
    if !names_enabled {
        result.insert(1, "names".into());
    }
    result
}
fn settings(
    library: &Library,
    names_enabled: bool,
    sync_disabled: bool,
    provenance_enabled: bool,
    index_automatic: bool,
) -> Result<Json> {
    crate::app_settings::info(
        library,
        &forced_off(library, names_enabled, sync_disabled, provenance_enabled, index_automatic),
    )
}

fn handle(mut request: Request, host: &Host, received: u64) {
    let write = request.method().as_str() == "POST";
    // Security checks precede body reads. A partial body must not hold the library lock.
    {
        let session = host.session.read().expect("app_http session lock poisoned");
        let denied = if header(&request, "Host") != Some(host.origin.trim_start_matches("http://")) {
            Some("仅允许本机地址访问")
        } else if header(&request, "Origin").is_some_and(|v| !v.is_empty() && v != host.origin) {
            Some("不允许跨站访问")
        } else if !host.accepts(&session, received)
            || (write
                && (header(&request, "Origin") != Some(host.origin.as_str())
                    || header(&request, "X-CSRF-Token") != Some(session.token.as_str())))
        {
            Some("会话已过期，请刷新页面")
        } else {
            None
        };
        if let Some(message) = denied {
            respond(request, error(message), 403, "core");
            return;
        }
    }
    if write {
        let length = request.body_length();
        if !matches!(length, Some(1..=100000))
            || !header(&request, "Content-Type")
                .is_some_and(|v| v.split(';').next().unwrap_or("").trim().eq_ignore_ascii_case("application/json"))
        {
            respond(request, error("请求格式不正确"), 400, "core");
            return;
        }
        let mut bytes = Vec::new();
        if request
            .as_reader()
            .take(length.expect("app_http internal invariant") as u64)
            .read_to_end(&mut bytes)
            .is_err()
        {
            respond(request, error("请求格式不正确"), 400, "core");
            return;
        }
        let text = match std::str::from_utf8(&bytes) {
            Ok(text) => text,
            Err(_) => {
                respond(request, error("请求格式不正确"), 400, "core");
                return;
            }
        };
        let body = match crate::http_json::parse_request(text) {
            Ok(value @ Json::Object(_)) => value,
            Ok(_) => {
                respond(request, error("请求格式不正确"), 400, "core");
                return;
            }
            Err(e) => {
                failure(request, e, true);
                return;
            }
        };
        http_host_post::handle(request, host, received, text, body);
        return;
    }
    if request.method().as_str() != "GET" {
        unavailable(request);
        return;
    }
    http_host_get::handle(request, host, received);
}

// Python library_action wraps filesystem failures as Invalid, while SQLite failures remain 500.
fn management_error(error: Error) -> Error {
    match error {
        Error::Io(e) => Error::Invalid(e.to_string()),
        other => other,
    }
}

// library_action('new') deliberately accepts str(value or ''). Keep its existing repr semantics.
fn python_text(value: &Json, nested: bool) -> String {
    match value {
        Json::Scalar(serde_json::Value::String(value)) if nested => crate::http_read::python_repr(value),
        Json::Scalar(serde_json::Value::String(value)) => value.clone(),
        Json::Scalar(serde_json::Value::Null) => "None".into(),
        Json::Scalar(serde_json::Value::Bool(value)) => if *value { "True" } else { "False" }.into(),
        Json::Integer(value) if value.trim_start_matches('-').chars().all(|c| c == '0') => "0".into(),
        Json::Float(value) if value.is_nan() => "nan".into(),
        Json::Float(value) if *value == f64::INFINITY => "inf".into(),
        Json::Float(value) if *value == f64::NEG_INFINITY => "-inf".into(),
        Json::Array(values) => {
            format!("[{}]", values.iter().map(|v| python_text(v, true)).collect::<Vec<_>>().join(", "))
        }
        Json::Object(values) => format!(
            "{{{}}}",
            values
                .iter()
                .map(|(k, v)| format!("{}: {}", crate::http_read::python_repr(k), python_text(v, true)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        _ => value.dump(),
    }
}
fn creation_name(body: &Json) -> String {
    let name = body.get("name").filter(|v| truthy(v)).map(|v| python_text(v, false)).unwrap_or_default();
    let name = crate::http_read::trim(&name);
    if name.is_empty() {
        "新图库".into()
    } else {
        name.into()
    }
}
fn create_library(host: &Host, body: &Json) -> Result<Json> {
    let name = creation_name(body);
    let name = name.as_str();
    let dest = crate::app_libraries::unique_path(name)?;
    let result = match body.get("kind").map(Json::text) {
        _ if body.get("sources").is_some() => create_sources_library(host, body, &dest, name),
        Some("photos") => {
            if name.contains('\0') {
                return Err(Error::Invalid("mkdir: embedded null character in path".into()));
            }
            // Match web.sync.PHOTOS; this is configuration only, never access the Photos package.
            let photos = crate::app_libraries::photos_library()?;
            crate::app_create::create_photos(&dest, &photos, Some(name), creation_reference(host)?)
        }
        Some("folder") => create_folder_library(host, body, &dest, name),
        _ => return Err(Error::Invalid("请选择来源".into())),
    }
    .map_err(|error| creation_error(error, &host.reference))?;
    // Published packages survive registry failure. Creation never rotates the active session. A registry failure is
    // reported with the published path (#97): creating again would only make a second, numbered package.
    let path = result["path"].as_str().ok_or_else(|| Error::Schema("建库结果缺少路径".into()))?;
    let mut out = Json::object([("path", path.into()), ("name", name.into())]);
    if let Err(error) = crate::app_libraries::remember_created(std::path::Path::new(path), name) {
        out.set("listed", false.into());
        out.set("error", crate::app_libraries::unlisted_reason(&error).into());
    }
    Ok(out)
}
/// `sources: [{kind: "folder", path, preset} | {kind: "photos"}]`: every folder names its origin; none is guessed.
fn create_sources_library(host: &Host, body: &Json, dest: &Path, name: &str) -> Result<serde_json::Value> {
    let Some(Json::Array(items)) = body.get("sources") else {
        return Err(Error::Invalid("sources 必须是数组".into()));
    };
    if name.contains('\0') {
        return Err(Error::Invalid("mkdir: embedded null character in path".into()));
    }
    let mut sources = Vec::new();
    for item in items {
        match item.get("kind").map(Json::text) {
            Some("folder") => {
                let path = item.get("path").map(Json::text).unwrap_or("");
                if path.is_empty() || path.contains('\0') {
                    return Err(Error::Invalid("请选择文件夹".into()));
                }
                let path = expand_path(path)?;
                let path = if path.is_absolute() { path } else { host.root.join(path) };
                sources.push(crate::app_create::NewSource::Folder {
                    path: crate::app_paths::resolve(&path)?,
                    preset: item.get("preset").map(Json::text).unwrap_or("").to_owned(),
                });
            }
            Some("photos") => {
                if !illustash_platform::capabilities::photos() {
                    return Err(Error::Invalid("Unsupported：照片图库只在 macOS 上可用".into()));
                }
                sources.push(crate::app_create::NewSource::Photos { path: crate::app_libraries::photos_library()? });
            }
            _ => return Err(Error::Invalid("请选择来源".into())),
        }
    }
    crate::app_service::build_sources(&host.root, dest, name, sources)
}
fn create_folder_library(host: &Host, body: &Json, dest: &Path, name: &str) -> Result<serde_json::Value> {
    let folder = match body.get("folder").filter(|v| truthy(v)) {
        Some(Json::Scalar(serde_json::Value::String(value))) => value.as_str(),
        None => "",
        Some(value) => {
            let kind = match value {
                Json::Integer(_) => "int",
                Json::Float(_) => "float",
                Json::Array(_) => "list",
                Json::Object(_) => "dict",
                Json::Scalar(serde_json::Value::Bool(_)) => "bool",
                _ => "object",
            };
            return Err(Error::Invalid(format!(
                "argument should be a str or an os.PathLike object where __fspath__ returns a str, not '{kind}'"
            )));
        }
    };
    if folder.contains('\0') {
        return Err(Error::Invalid("lstat: embedded null character in path".into()));
    }
    let folder = expand_path(folder)?;
    let folder = if folder.is_absolute() { folder } else { host.root.join(folder) };
    // Non-strict resolution precedes directory/preset validation, matching new_folder_library.
    let folder = crate::app_paths::resolve(&folder)?;
    if !folder.is_dir() {
        return Err(Error::Invalid(format!("不是文件夹：{}", folder.display())));
    }
    let preset = match body.get("preset").filter(|v| truthy(v)) {
        Some(Json::Scalar(serde_json::Value::String(value))) => Some(value.as_str()),
        None => None,
        Some(_) => return Err(Error::Invalid("来源预设只能是 x、pixiv、other".into())),
    };
    if preset.is_some_and(|value| crate::origin::find(value).is_none()) {
        return Err(Error::Invalid("来源预设只能是 x、pixiv、other".into()));
    }
    if name.contains('\0') {
        return Err(Error::Invalid("mkdir: embedded null character in path".into()));
    }
    crate::app_create::create_folder(dest, &folder, Some(name), preset, creation_reference(host)?)
}
fn creation_reference(host: &Host) -> Result<Option<&Path>> {
    crate::app_create::optional_reference(&host.reference)
}
use crate::app_create::creation_error;

fn expand_path(value: &str) -> Result<PathBuf> {
    if value == "~" || value.starts_with("~/") {
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .ok_or_else(|| Error::Invalid("无法定位用户目录".into()))?;
        return Ok(PathBuf::from(home).join(value.strip_prefix("~/").unwrap_or("")));
    }
    Ok(PathBuf::from(value))
}

/// Desktop startup protocol: `启动进度：<per mille> <stage>` lines precede the ready line. The shell shows
/// them and treats each as a sign of life; the CLI prints nothing extra.
fn startup_progress(stage: &str, done: f64) {
    static LAST: std::sync::Mutex<Option<(u32, String)>> = std::sync::Mutex::new(None);
    if std::env::var_os("ILLUSTASH_DESKTOP_STDIN").is_none() {
        return;
    }
    let permille = (done.clamp(0.0, 1.0) * 1000.0) as u32;
    let mut last = LAST.lock().expect("startup progress lock poisoned");
    // One line per visible change: at most about a thousand for any library size.
    if last.as_ref().is_some_and(|(p, s)| *p == permille && s == stage) {
        return;
    }
    *last = Some((permille, stage.to_owned()));
    println!("启动进度：{permille} {stage}");
    let _ = std::io::Write::flush(&mut std::io::stdout());
}

/// The integrity checks read both databases in full; they are most of a large library's startup.
fn open_reporting(path: &Path) -> Result<Library> {
    startup_progress("正在打开图库", 0.0);
    let progress: crate::app_library::Progress =
        std::sync::Arc::new(|label, done| startup_progress(&format!("正在检查图库数据（{label}）"), done * 0.95));
    Library::open_reporting(path, progress)
}

pub(crate) fn run(args: &[String]) -> Result<u8> {
    // Pipe ownership is captured before initialization, so even an early parent crash
    // leads to the normal stop/join path. Never read CLI stdin unless explicitly requested.
    let desktop_closed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    if std::env::var_os("ILLUSTASH_DESKTOP_STDIN").as_deref() == Some(std::ffi::OsStr::new("1")) {
        // The shell assigns its Windows Job before releasing this startup gate.
        // If it dies before assignment, EOF exits before any task can start.
        let mut gate = [0u8; 1];
        if std::io::stdin().read_exact(&mut gate).is_err() {
            return Ok(0);
        }
        let closed = desktop_closed.clone();
        thread::spawn(move || {
            let mut buffer = [0u8; 64];
            let mut input = std::io::stdin().lock();
            loop {
                match input.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
            closed.store(true, Ordering::SeqCst);
        });
    }
    let mut path = None;
    let mut root = crate::app_resources::default_root();
    let mut port = 8765;
    let mut names_enabled = true;
    let mut names_network = false;
    let mut provenance_enabled = true;
    let mut provenance_network = false;
    let mut index_network = false;
    let mut index_automatic = true;
    let mut auto_sync = true;
    let mut sync_disabled = false;
    let mut per_run = 0usize;
    let mut backup = false;
    let mut upgrade = false;
    let mut expanded = Vec::new();
    for arg in args {
        if let Some((key, value)) = arg.split_once('=') {
            if ["--library", "--root", "--port", "--sync-limit"].contains(&key) {
                expanded.extend([key.to_owned(), value.to_owned()]);
                continue;
            }
        }
        expanded.push(arg.clone());
    }
    let mut iter = expanded.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--help" => {
                println!("illustash-terms http --library PACKAGE [--root ROOT] [--port PORT] [--backup | --upgrade] [--no-open] [--no-sync | --no-auto-sync] [--sync-limit N] [--no-names] [--names-network] [--no-provenance] [--allow-provenance-network] [--no-index-check | --index-check] [--index-network]");
                return Ok(0);
            }
            "--native" | "--no-open" => {}
            arg if [
                "--python",
                "--upstream",
                "--route",
                "--",
                "--refresh",
                "--source",
                "--db",
                "--cache",
                "--identity",
                "--no-similar",
            ]
            .iter()
            .any(|key| arg == *key || arg.starts_with(&format!("{key}="))) =>
            {
                return Err(Error::Invalid(format!(
                    "兼容宿主参数已退役：{arg}；HTTP 默认使用原生宿主，升级请使用 --upgrade"
                )));
            }
            "--no-names" => names_enabled = false,
            "--no-provenance" => provenance_enabled = false,
            "--allow-provenance-network" => provenance_network = true,
            "--names-network" => names_network = true,
            "--index-network" => index_network = true,
            "--no-index-check" => index_automatic = false,
            "--index-check" => index_automatic = true,
            "--auto-sync" => auto_sync = true,
            "--no-auto-sync" => auto_sync = false,
            "--no-sync" => {
                auto_sync = false;
                sync_disabled = true;
            }
            "--backup" => backup = true,
            "--upgrade" => upgrade = true,
            "--library" | "--root" | "--port" | "--route" | "--sync-limit" => {
                let value = iter.next().ok_or_else(|| Error::Invalid(format!("{arg} 缺少值")))?;
                match arg.as_str() {
                    "--library" => path = Some(expand_path(value)?),
                    "--root" => root = PathBuf::from(value),
                    "--sync-limit" => {
                        per_run = value.parse().map_err(|_| Error::Invalid("sync-limit 必须是非负整数".into()))?
                    }
                    _ => {
                        port = value
                            .parse::<u16>()
                            .ok()
                            .filter(|n| *n > 0)
                            .ok_or_else(|| Error::Invalid("port 必须在 1–65535 之间".into()))?
                    }
                }
            }
            _ => return Err(Error::Invalid(format!("原生 HTTP 不支持参数：{arg}"))),
        }
    }
    let root = if root.is_absolute() { root } else { std::env::current_dir()?.join(root) };
    let path = match path {
        Some(path) => path,
        None => crate::app_libraries::startup()?,
    };
    if upgrade && backup {
        return Err(Error::Invalid("--upgrade 不可与 --backup 同用".into()));
    }
    if backup {
        let library = Library::open(&path)?;
        println!("{}", crate::app_settings::backup(&library)?.display());
        return Ok(0);
    }
    // Reserve the port and session entropy before any explicit upgrade commit.
    let server = if upgrade {
        let listener = TcpListener::bind(("127.0.0.1", port))?;
        Some(serve(listener)?)
    } else {
        None
    };
    let token = Session::token()?;
    let (library, upgrade_backup) = if upgrade {
        match Library::open(&path) {
            Ok(library) if !crate::app_legacy_upgrade::required(&library)? => (library, None),
            _ => {
                let prepared = crate::app_upgrade::prepare(&path)?;
                let report =
                    crate::app_upgrade::upgrade(&prepared).map_err(|failure| Error::Schema(failure.to_string()))?;
                eprintln!(
                    "图库升级已提交：terms {} → {}；升级前备份：{}{}",
                    report.before,
                    report.after,
                    report.backup.display(),
                    if report.maintenance_pending { "；空间维护待重试" } else { "" }
                );
                (report.library, Some(report.backup))
            }
        }
    } else {
        (open_reporting(&path)?, None)
    };
    let server = match server {
        Some(server) => server,
        None => {
            let listener = TcpListener::bind(("127.0.0.1", port))?;
            serve(listener)?
        }
    };
    let application = Application::from_library(
        library,
        token,
        Options {
            root,
            names_enabled,
            names_network,
            provenance_enabled,
            provenance_network,
            index_network,
            index_automatic,
            auto_sync,
            sync_disabled,
            per_run,
        },
        |error| match &upgrade_backup {
            Some(backup) => format!(
                "升级已提交，没能记进最近图库：{}；升级前备份：{}",
                crate::app_libraries::unlisted_reason(&error),
                backup.display()
            ),
            None => crate::app_libraries::unlisted_reason(&error),
        },
    )?;
    let host = Arc::new(Host { application, origin: format!("http://127.0.0.1:{port}") });
    http::install_signals();
    let desktop_parent = crate::platform_process::DesktopParent::capture();
    let mut handlers: Vec<thread::JoinHandle<()>> = Vec::new();
    // Recover durable research work, or the established no-learner verdict notification.
    // Hold this session while recovering; no event from its database is replayed into a replacement library.
    host.start_reporting(|| startup_progress("正在恢复后台任务", 0.97))?;
    println!("图库已启动：{}", host.origin);
    std::io::Write::flush(&mut std::io::stdout())?;
    let mut next_recovery = Instant::now() + Duration::from_secs(1);
    while !http::stopping() && !desktop_closed.load(Ordering::SeqCst) {
        if desktop_parent.lost() {
            break;
        }
        if let Some(request) = server.recv_timeout(Duration::from_millis(100))? {
            // Capture before spawning/locking/body IO: queued requests belong to the receiving session.
            let received = host.generation.load(Ordering::SeqCst);
            if handlers.iter().filter(|h| !h.is_finished()).count() >= MAX_HANDLERS {
                // Bounded threads: further requests are refused at once instead of piling up.
                respond(request, error("服务繁忙，请稍后重试"), 503, "core");
            } else {
                let host = host.clone();
                handlers.push(thread::spawn(move || handle(request, &host, received)));
            }
        }
        if Instant::now() >= next_recovery {
            host.recover();
            next_recovery = Instant::now() + Duration::from_secs(1);
        }
        let mut i = 0;
        while i < handlers.len() {
            if handlers[i].is_finished() {
                let _ = handlers.swap_remove(i).join();
            } else {
                i += 1;
            }
        }
    }
    // EOF and parent loss must cancel selectors/handlers as well as the main loop.
    http::request_stop();
    host.shutdown();
    let deadline = Instant::now() + Duration::from_secs(8);
    while handlers.iter().any(|h| !h.is_finished()) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(25));
    }
    // Unfinished socket bodies do not keep the executable alive. SQLite owns interrupted-write recovery.
    Ok(0)
}

#[cfg(test)]
mod tests {
    #[test]
    fn creation_name_keeps_python_truthiness_repr_and_whitespace() {
        for (input, expected) in [
            ("null", "新图库"),
            ("false", "新图库"),
            ("0", "新图库"),
            ("-0", "新图库"),
            ("-0.0", "新图库"),
            ("[]", "新图库"),
            ("{}", "新图库"),
            (r#"" ""#, "新图库"),
            ("true", "True"),
            ("123", "123"),
            ("1e400", "inf"),
            ("NaN", "nan"),
            ("Infinity", "inf"),
            ("-Infinity", "-inf"),
            (r#""\u001c 名字 \u001f""#, "名字"),
            ("[-0]", "[0]"),
            (r#"["x", true, null]"#, "['x', True, None]"),
            (r#"{"a":"b","c":[1,false]}"#, "{'a': 'b', 'c': [1, False]}"),
        ] {
            let body = crate::http_json::parse_request(&format!("{{\"name\":{input}}}")).unwrap();
            assert_eq!(super::creation_name(&body), expected, "{input}");
        }
    }
}
