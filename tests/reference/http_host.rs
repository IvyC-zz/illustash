//! Historical HTTP front door retained only for synthetic differential tests.
//! Core reads open the current library anew and share one request-local transaction.
use crate::{Error, Result};
use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use tiny_http::{Header, Request, Response, Server, StatusCode};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    Forward,
    Core,
}

// Exact method + path. Unknown routes forward; file routes are native in the package host.
// R1–R4 share these route keys with the HTTP comparison corpus.
pub const READ_ROUTES: &[(&str, &str)] = &[
    ("R4", "/api/bootstrap"),
    ("R4", "/api/prefs"),
    ("R1", "/api/user-tags"),
    ("R1", "/api/general-tags"),
    ("R1", "/api/artists"),
    ("R1", "/api/artist"),
    ("R1", "/api/artist-audit"),
    ("R2", "/api/entities"),
    ("R3", "/api/assets"),
    ("R3", "/api/assets/count"),
    ("R3", "/api/ids"),
    ("R3", "/api/detail"),
    ("R3", "/api/home"),
    ("R3", "/api/fold"),
    ("R3", "/api/similar"),
    ("R3", "/api/clusters"),
    ("R3", "/api/review-pools"),
    ("R4", "/api/characters"),
    ("R4", "/api/character/profile"),
    ("R4", "/api/character/activity"),
    ("R4", "/api/character/stats"),
    ("R2", "/api/works"),
    ("R4", "/api/tagged"),
    ("R4", "/api/oc/groups"),
    ("R4", "/api/local-links"),
    ("R4", "/api/ask"),
    ("R4", "/api/identity/status"),
    ("R4", "/api/identity/relations"),
    ("R4", "/api/identity/count"),
    ("R4", "/api/identity/queue"),
    ("R4", "/api/identity/pairs"),
    ("R4", "/api/identity/assignments"),
];

#[derive(Clone, Default)]
pub struct Routes(HashMap<(String, String), Backend>);
impl Routes {
    pub fn new(overrides: &[String]) -> Result<Self> {
        Self::configured(overrides, false)
    }
    fn configured(overrides: &[String], native: bool) -> Result<Self> {
        let mut routes = Self(
            READ_ROUTES
                .iter()
                .map(|(_, path)| {
                    (
                        ("GET".into(), (*path).into()),
                        if crate::http_read::CORE_ROUTES.contains(path)
                            && !["/api/similar", "/api/clusters"].contains(path)
                        {
                            Backend::Core
                        } else {
                            Backend::Forward
                        },
                    )
                })
                .collect(),
        );
        for path in crate::app_files::ROUTES {
            routes.0.insert(("GET".into(), (*path).into()), Backend::Core);
        }
        for path in crate::app_local::ROUTES {
            routes.0.insert(("GET".into(), (*path).into()), Backend::Core);
        }
        if native {
            routes.0.insert(("GET".into(), "/api/provenance".into()), Backend::Core);
        }
        for spec in overrides {
            let (key, backend) = spec
                .split_once('=')
                .ok_or_else(|| Error::Invalid("route 格式：GET:/api/assets=forward|core".into()))?;
            let (method, path) = key.split_once(':').ok_or_else(|| Error::Invalid("route 缺少方法或路径".into()))?;
            if method.is_empty()
                || !method.bytes().all(|c| c.is_ascii_uppercase())
                || !path.starts_with('/')
                || path.contains('?')
            {
                return Err(Error::Invalid(format!("route 方法或路径不合法：{key}")));
            }
            let backend = match backend {
                "forward" => Backend::Forward,
                // Do not silently fall back or pretend a forwarded result is a core read.
                "core"
                    if method == "GET"
                        && (crate::http_read::CORE_ROUTES.contains(&path)
                            || crate::app_files::ROUTES.contains(&path)
                            || crate::app_local::ROUTES.contains(&path)
                            || (native && ["/api/local-links", "/api/provenance"].contains(&path))) =>
                {
                    Backend::Core
                }
                "core" => return Err(Error::Invalid(format!("尚未迁移核心读接口：{key}"))),
                _ => return Err(Error::Invalid("route 后端只能是 forward|core".into())),
            };
            routes.0.insert((method.into(), path.into()), backend);
        }
        Ok(routes)
    }
    pub fn backend(&self, method: &str, url: &str) -> Backend {
        let path = url.split('?').next().unwrap_or(url);
        self.0.get(&(method.into(), path.into())).copied().unwrap_or(Backend::Forward)
    }
}

use crate::http::LibraryContext;

pub fn current_library(state: &Path) -> Result<LibraryContext> {
    let value: serde_json::Value = serde_json::from_slice(&std::fs::read(state)?)
        .map_err(|e| Error::Invalid(format!("HTTP 图库状态损坏：{e}")))?;
    let field = |key: &str| {
        value[key].as_str().filter(|v| !v.is_empty()).ok_or_else(|| Error::Invalid(format!("HTTP 图库状态缺少 {key}")))
    };
    Ok(LibraryContext {
        db: PathBuf::from(field("db")?),
        source: value["source"].as_str().map(PathBuf::from),
        token: value["token"].as_str().map(str::to_owned),
        read_only: value["read_only"].as_str().map(str::to_owned),
        generation: field("generation")?.into(),
        library: value["library"].as_str().map(PathBuf::from),
        tags: value["tags"].as_str().map(PathBuf::from).unwrap_or_default(),
    })
}

fn hop_header(name: &str, connection: &[String]) -> bool {
    [
        "connection",
        "proxy-connection",
        "keep-alive",
        "transfer-encoding",
        "te",
        "trailer",
        "upgrade",
        "proxy-authenticate",
        "proxy-authorization",
    ]
    .iter()
    .any(|h| name.eq_ignore_ascii_case(h))
        || connection.iter().any(|h| name.eq_ignore_ascii_case(h))
}

fn forward(request: &mut Request, upstream: SocketAddr) -> io::Result<Response<Box<dyn Read + Send>>> {
    let mut socket = TcpStream::connect_timeout(&upstream, Duration::from_secs(2))?;
    // Preserve the existing server's long-running POST behavior: no new response deadline.
    let connection: Vec<String> = request
        .headers()
        .iter()
        .filter(|h| h.field.equiv("Connection"))
        .flat_map(|h| h.value.as_str().split(',').map(|v| v.trim().to_owned()))
        .collect();
    // HTTP/1.0 keeps the Python response unchunked. Never decode gzip or reserialize JSON.
    write!(socket, "{} {} HTTP/1.0\r\n", request.method(), request.url())?;
    for h in request.headers() {
        let name: &str = h.field.as_str().as_ref();
        if !hop_header(name, &connection) && !h.field.equiv("Content-Length") {
            write!(socket, "{h}\r\n")?;
        }
    }
    // Preserve Python's body framing/rejection rules. It does not accept chunked POSTs or
    // bodies >100000 bytes; do not buffer them into a valid request or wait on rejected data.
    let length = request.body_length();
    if let Some(n) = length {
        write!(socket, "Content-Length: {n}\r\n")?;
    }
    write!(socket, "Connection: close\r\n\r\n")?;
    if request.method().as_str() == "POST" {
        if let Some(n @ 1..=100000) = length {
            // Python can refuse Host/Origin before reading the body. A broken write must not
            // overwrite that valid rejection response with a proxy 502.
            let _ = io::copy(&mut request.as_reader().take(n as u64), &mut socket);
        }
    }
    socket.flush()?;
    let mut reader = BufReader::new(socket);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let status = line
        .split_whitespace()
        .nth(1)
        .and_then(|v| v.parse::<u16>().ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Python HTTP status missing"))?;
    let mut headers = Vec::new();
    let mut size = None;
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "Python HTTP headers incomplete"));
        }
        if line == "\r\n" || line == "\n" {
            break;
        }
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Python HTTP header invalid"))?;
        let value = value.trim();
        if name.eq_ignore_ascii_case("Content-Length") {
            size = Some(
                value
                    .parse::<usize>()
                    .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "Python Content-Length invalid"))?,
            );
        } else if !hop_header(name, &[]) && !name.eq_ignore_ascii_case("X-Illustash-Route") {
            headers.push(
                Header::from_bytes(name, value)
                    .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "Python HTTP header invalid"))?,
            );
        }
    }
    headers.push(Header::from_bytes("X-Illustash-Route", "forward").unwrap());
    let body: Box<dyn Read + Send> =
        if let Some(n) = size { Box::new(reader.take(n as u64)) } else { Box::new(reader) };
    Ok(Response::new(StatusCode(status), headers, body, size, None).with_chunked_threshold(usize::MAX))
}

fn accepts_gzip(value: &str) -> bool {
    let mut accepted = HashMap::new();
    for part in value.split(',') {
        let lowered = part.trim().to_lowercase();
        let mut pieces = lowered.split(';');
        let name = pieces.next().unwrap_or("").trim();
        let q =
            pieces.find_map(|p| p.trim().strip_prefix("q=")).map(|v| v.parse::<f64>().unwrap_or(0.0)).unwrap_or(1.0);
        accepted.insert(name.to_owned(), q);
    }
    accepted.get("gzip").or_else(|| accepted.get("*")).copied().unwrap_or(0.0) > 0.0
}

fn core(
    request: &mut Request,
    upstream: SocketAddr,
    state: &Path,
    origin: &str,
) -> io::Result<Response<Box<dyn Read + Send>>> {
    let header = |name: &str| {
        request
            .headers()
            .iter()
            .find(|h| h.field.as_str().as_str().eq_ignore_ascii_case(name))
            .map(|h| h.value.as_str())
    };
    if header("Host") != Some(origin.trim_start_matches("http://")) || header("Origin").is_some_and(|v| v != origin) {
        return forward(request, upstream);
    }
    let context = match current_library(state) {
        Ok(c) => c,
        Err(_) => return forward(request, upstream),
    };
    let (payload, status) = match crate::http_read::read(&context, request.url()) {
        Ok(Some(v)) => v,
        Ok(None) => return forward(request, upstream),
        Err(e) => {
            eprintln!("HTTP core: {e}");
            (crate::http_json::Json::object([("error", "读取失败，请重试；详情请查看启动终端".into())]), 500)
        }
    };
    // An exec/library switch invalidates the context before the replacement becomes ready.
    if !current_library(state).is_ok_and(|c| c.generation == context.generation) {
        return forward(request, upstream);
    }
    json_response(request, &payload, status, "core")
}

/// Identical JSON serialization, compression and security headers for both application hosts.
pub(crate) fn json_response(
    request: &Request,
    payload: &crate::http_json::Json,
    status: u16,
    route: &str,
) -> io::Result<Response<Box<dyn Read + Send>>> {
    let accepted =
        request.headers().iter().find(|h| h.field.equiv("Accept-Encoding")).map(|h| h.value.as_str()).unwrap_or("");
    let mut bytes = payload.dump().into_bytes();
    let mut encoding = None;
    if bytes.len() >= 1024 && accepts_gzip(accepted) {
        let mtime =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs() as u32;
        let mut gz =
            flate2::GzBuilder::new().mtime(mtime).operating_system(255).write(Vec::new(), flate2::Compression::new(1));
        gz.write_all(&bytes)?;
        let compressed = gz.finish()?;
        if compressed.len() < bytes.len() {
            bytes = compressed;
            encoding = Some("gzip");
        }
    }
    let n = bytes.len();
    let mut headers:Vec<Header>=[("Content-Type","application/json; charset=utf-8"),("Cache-Control","no-store"),("Vary","Accept-Encoding"),("X-Content-Type-Options","nosniff"),("Referrer-Policy","no-referrer"),("Content-Security-Policy","default-src 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline'; script-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'"),("X-Illustash-Route",route)].into_iter().map(|(k,v)|Header::from_bytes(k,v).unwrap()).collect();
    if let Some(encoding) = encoding {
        headers.push(Header::from_bytes("Content-Encoding", encoding).unwrap());
    }
    Ok(Response::new(
        StatusCode(status),
        headers,
        Box::new(io::Cursor::new(bytes)) as Box<dyn Read + Send>,
        Some(n),
        None,
    )
    .with_chunked_threshold(usize::MAX))
}

/// One independent worker per request: slow thumbnail reads cannot serialize JSON requests.
pub fn serve(
    server: Arc<Server>,
    upstream: SocketAddr,
    routes: Routes,
    stop: Arc<AtomicBool>,
    state: Option<PathBuf>,
    origin: String,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        while !stop.load(Ordering::SeqCst) {
            match server.recv_timeout(Duration::from_millis(100)) {
                Ok(Some(mut request)) => {
                    let backend = if state.is_none() || crate::http_read::remote(request.url()) {
                        Backend::Forward
                    } else {
                        routes.backend(request.method().as_str(), request.url())
                    };
                    let state = state.clone();
                    let origin = origin.clone();
                    thread::spawn(move || {
                        let result = match backend {
                            Backend::Forward => forward(&mut request, upstream),
                            Backend::Core => {
                                core(&mut request, upstream, state.as_deref().expect("core context"), &origin)
                            }
                        };
                        match result {
                            Ok(response) => {
                                let _ = request.respond(response);
                            }
                            Err(e) => {
                                eprintln!("HTTP upstream: {e}");
                                let _ = request.respond(
                                    Response::from_string("{\"error\": \"Python 服务暂不可用\"}")
                                        .with_status_code(502)
                                        .with_header(
                                            Header::from_bytes("Content-Type", "application/json; charset=utf-8")
                                                .unwrap(),
                                        )
                                        .with_header(Header::from_bytes("Cache-Control", "no-store").unwrap())
                                        .with_header(Header::from_bytes("X-Illustash-Route", "unavailable").unwrap()),
                                );
                            }
                        }
                    });
                }
                Ok(None) => {}
                Err(e) => {
                    eprintln!("HTTP listener: {e}");
                    break;
                }
            }
        }
    })
}

static STOP: AtomicBool = AtomicBool::new(false);
#[cfg(unix)]
extern "C" fn signal_stop(_: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
}
#[cfg(windows)]
unsafe extern "system" fn console_stop(event: u32) -> i32 {
    if event <= 2 || event == 5 || event == 6 {
        STOP.store(true, Ordering::SeqCst);
        1
    } else {
        0
    }
}
pub(crate) fn install_signals() {
    STOP.store(false, Ordering::SeqCst);
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGINT, signal_stop as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, signal_stop as *const () as libc::sighandler_t);
    }
    #[cfg(windows)]
    unsafe {
        #[link(name = "kernel32")]
        extern "system" {
            fn SetConsoleCtrlHandler(handler: Option<unsafe extern "system" fn(u32) -> i32>, add: i32) -> i32;
        }
        SetConsoleCtrlHandler(Some(console_stop), 1);
    }
}
fn stop_python(child: &mut Child) {
    #[cfg(unix)]
    unsafe {
        libc::kill(child.id() as i32, libc::SIGINT);
    }
    #[cfg(not(unix))]
    let _ = child.kill();
    let deadline = Instant::now() + Duration::from_secs(8);
    while Instant::now() < deadline {
        if child.try_wait().ok().flatten().is_some() {
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }
    #[cfg(unix)]
    unsafe {
        libc::killpg(child.id() as i32, libc::SIGKILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}

struct StateFile(PathBuf);
impl StateFile {
    fn new() -> Result<Self> {
        let mut nonce = [0u8; 16];
        getrandom::fill(&mut nonce).map_err(|e| Error::Invalid(e.to_string()))?;
        let name: String = nonce.iter().map(|v| format!("{v:02x}")).collect();
        let path = std::env::temp_dir().join(format!("illustash-http-{name}"));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options.open(&path)?;
        Ok(Self(path))
    }
}
impl Drop for StateFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// `http [--root ROOT] [--python PYTHON] [--port PORT] [--route METHOD:/path=forward|core] [--] <Python options>`.
/// --upstream PORT runs only the front door (useful for a controlled same-library comparison).
pub fn run(args: &[String]) -> Result<u8> {
    if args.iter().take_while(|arg| arg.as_str() != "--").any(|arg| arg == "--native") {
        return Err(Error::Invalid("测试参考宿主不支持 --native".into()));
    }
    #[cfg(unix)]
    let desktop_parent = std::env::var_os("ILLUSTASH_DESKTOP_BRIDGE").map(|_| unsafe { libc::getppid() });
    let mut root = std::env::var_os("ILLUSTASH_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."));
    let mut python = None;
    let mut port = 8765;
    let mut upstream = None;
    let mut overrides = Vec::new();
    let mut options = Vec::new();
    let mut expanded = Vec::new();
    let mut after_separator = false;
    for arg in args {
        if !after_separator {
            if arg == "--" {
                after_separator = true;
            }
            if let Some((key, value)) = arg.split_once('=') {
                if ["--root", "--python", "--port", "--route", "--upstream"].contains(&key) {
                    expanded.extend([key.to_owned(), value.to_owned()]);
                    continue;
                }
            }
        }
        expanded.push(arg.clone());
    }
    let mut args = expanded.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--" => {
                options.extend(args.cloned());
                break;
            }
            "--help" => {
                println!("illustash-terms http [--root ROOT] [--python PYTHON] [--port PORT] [--route METHOD:/path=forward|core] [--upstream PORT] [--] <web.server options>");
                return Ok(0);
            }
            "--root" | "--python" | "--port" | "--route" | "--upstream" => {
                let value = args.next().ok_or_else(|| Error::Invalid(format!("{arg} 缺少值")))?;
                match arg.as_str() {
                    "--root" => root = PathBuf::from(value),
                    "--python" => python = Some(PathBuf::from(value)),
                    "--route" => overrides.push(value.clone()),
                    "--port" => {
                        port = value
                            .parse::<u16>()
                            .ok()
                            .filter(|n| *n > 0)
                            .ok_or_else(|| Error::Invalid("port 必须在 1–65535 之间".into()))?
                    }
                    _ => {
                        upstream = Some(
                            value
                                .parse::<u16>()
                                .ok()
                                .filter(|n| *n > 0)
                                .ok_or_else(|| Error::Invalid("upstream 端口不合法".into()))?,
                        )
                    }
                }
            }
            _ => options.push(arg.clone()),
        }
    }
    let routes = Routes::new(&overrides)?;
    if upstream == Some(port) {
        return Err(Error::Invalid("upstream 不能指向前门自身".into()));
    }
    if options.iter().any(|s| s == "--port" || s.starts_with("--port=")) {
        return Err(Error::Invalid("Python 内部端口由前门分配，--port 放在 -- 之前".into()));
    }
    if upstream.is_none() {
        root = root.canonicalize()?;
    }
    let python = python
        .unwrap_or_else(|| root.join(if cfg!(windows) { ".venv/Scripts/python.exe" } else { ".venv/bin/python" }));
    let mut command = Command::new(&python);
    command
        .current_dir(&root)
        .args(["-m", "tests.reference.web.server"])
        .args(&options)
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .stdin(Stdio::null());
    if std::env::var_os("ILLUSTASH_TERMS_BIN").is_none() {
        command.env("ILLUSTASH_TERMS_BIN", std::env::current_exe()?);
    }
    // Existing --backup must still work while the public service is running.
    if upstream.is_none() && options.iter().any(|a| a == "--backup") {
        return Ok(command.status()?.code().unwrap_or(1).clamp(0, 255) as u8);
    }
    let listener = TcpListener::bind(("127.0.0.1", port))?;
    let server = Arc::new(Server::from_listener(listener, None).map_err(|e| Error::Invalid(e.to_string()))?);
    install_signals();
    let stop = Arc::new(AtomicBool::new(false));
    if let Some(upstream) = upstream {
        if !options.is_empty() {
            return Err(Error::Invalid("--upstream 模式不能同时启动 Python".into()));
        }
        let handle = serve(
            server,
            ([127, 0, 0, 1], upstream).into(),
            routes,
            stop.clone(),
            std::env::var_os("ILLUSTASH_HTTP_STATE").map(PathBuf::from),
            format!("http://127.0.0.1:{port}"),
        );
        println!("HTTP 前门已启动：http://127.0.0.1:{port}");
        while !STOP.load(Ordering::SeqCst) {
            thread::sleep(Duration::from_millis(50));
        }
        stop.store(true, Ordering::SeqCst);
        let _ = handle.join();
        return Ok(0);
    }
    // Reserve the public listener for the whole lifetime; do not reuse or kill another service.
    let internal = TcpListener::bind(("127.0.0.1", 0))?;
    let upstream = internal.local_addr()?;
    drop(internal);
    let state = StateFile::new()?;
    command
        .args(["--port", &upstream.port().to_string()])
        .env("ILLUSTASH_HTTP_ORIGIN", format!("http://127.0.0.1:{port}"))
        .env("ILLUSTASH_HTTP_STATE", &state.0);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command.spawn()?;
    let handle =
        serve(server, upstream, routes, stop.clone(), Some(state.0.clone()), format!("http://127.0.0.1:{port}"));
    let code = loop {
        #[cfg(unix)]
        if desktop_parent.is_some_and(|pid| unsafe { libc::getppid() } != pid) {
            stop_python(&mut child);
            break 0;
        }
        if STOP.load(Ordering::SeqCst) {
            stop_python(&mut child);
            break 0;
        }
        match child.try_wait() {
            Ok(Some(status)) => break status.code().unwrap_or(1).clamp(0, 255) as u8,
            Ok(None) => thread::sleep(Duration::from_millis(50)),
            Err(e) => {
                stop_python(&mut child);
                eprintln!("Python wait: {e}");
                break 1;
            }
        }
    };
    stop.store(true, Ordering::SeqCst);
    let _ = handle.join();
    Ok(code)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_method_and_path_routes_and_unimplemented_core() {
        let mut routes = Routes::new(&["GET:/api/assets=forward".into()]).unwrap();
        routes.0.insert(("GET".into(), "/api/assets".into()), Backend::Core);
        assert_eq!(routes.backend("GET", "/api/assets?q=x"), Backend::Core);
        assert_eq!(routes.backend("POST", "/api/assets"), Backend::Forward);
        assert_eq!(routes.backend("GET", "/api/assets/count"), Backend::Core);
        assert!(Routes::new(&["GET:/api/assets=core".into()]).is_ok());
        assert!(Routes::new(&["GET:/api/ask=core".into()]).is_ok());
        assert!(Routes::new(&["GET:/api/assets?q=x=forward".into()]).is_err());
    }
    #[test]
    fn library_context_is_not_cached_and_invalidates_on_restart() {
        let state = StateFile::new().unwrap();
        assert!(current_library(&state.0).is_err());
        for (db, generation) in [("/tmp/a.sqlite", "one"), ("/tmp/b.sqlite", "two")] {
            std::fs::write(&state.0, serde_json::json!({"db": db, "generation": generation}).to_string()).unwrap();
            let current = current_library(&state.0).unwrap();
            assert_eq!(current.db, PathBuf::from(db));
            assert_eq!(current.generation, generation);
        }
        std::fs::remove_file(&state.0).unwrap();
        assert!(current_library(&state.0).is_err());
    }
}
