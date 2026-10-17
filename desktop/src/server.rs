//! 启动原生 HTTP 前门（与 CLI 共用），ready 后导航；退出等待宿主任务收尾。
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use tauri::{AppHandle, Manager, Url, WebviewWindow};

const PORT: u16 = 8765;
const READY: &str = "图库已启动：";
const PROGRESS: &str = "启动进度：";
const STOP_GRACE: Duration = Duration::from_secs(10);
/// The fold takes about three seconds; past this the start page is assumed unable to open the library itself.
const LAUNCH_FALLBACK: Duration = Duration::from_secs(8);
/// A large library's integrity check alone takes tens of seconds, so only silence counts as a hang:
/// every output line, including each progress report, restarts this window.
const STALL: Duration = Duration::from_secs(30);

/// Only the native host spawned by this window. Stop waits for its task cleanup.
#[derive(Default)]
pub struct Child {
    process: Mutex<Option<std::process::Child>>,
    stopping: AtomicBool,
    job: Mutex<Option<illustash_platform::platform_process::DesktopJob>>,
}

impl Child {
    pub fn stop(&self) {
        self.stopping.store(true, Ordering::SeqCst);
        let mut process = self.process.lock().unwrap();
        let Some(mut child) = process.take() else {
            return;
        };
        // Closing our write end also survives abnormal parent exit, on every platform.
        drop(child.stdin.take());
        let deadline = Instant::now() + STOP_GRACE;
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                _ if Instant::now() >= deadline => {
                    illustash_platform::platform_process::kill_owned_tree(child.id());
                    self.job.lock().unwrap().take();
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
                _ => thread::sleep(Duration::from_millis(50)),
            }
        }
        self.job.lock().unwrap().take();
    }
}

#[derive(Default)]
pub struct ErrorPage(Mutex<Option<String>>);

/// The latest startup progress script, replayed if the start page (re)loads after it was sent.
#[derive(Default)]
pub struct StartupPage(Mutex<Option<String>>);

/// One local service at a time. Every start takes a new generation; progress, ready and exit from an older
/// one (cancelled from the start page, or replaced by a retry or another library) are ignored.
#[derive(Default)]
pub struct Launch {
    generation: AtomicU64,
    config: Mutex<Option<Config>>,
    /// The library of the latest start (None: whatever the command line or the registry chose), for retry.
    library: Mutex<Option<PathBuf>>,
    /// Why startup stopped before the service could be configured; a retry cannot get past it.
    blocked: Mutex<Option<String>>,
}
#[derive(Clone)]
struct Config {
    port: u16,
    bridge_port: u16,
    token: String,
    root: PathBuf,
    args: Vec<String>,
}
fn current(app: &AppHandle, generation: u64) -> bool {
    app.state::<Launch>().generation.load(Ordering::SeqCst) == generation
}
pub fn resource_root(app: &AppHandle) -> Option<PathBuf> {
    app.state::<Launch>().config.lock().unwrap().as_ref().map(|c| c.root.clone())
}

/// Resource layout shared with #69: resource_dir/resources; explicit root wins.
fn root(app: &AppHandle, args: &[String]) -> Result<PathBuf, String> {
    let mut explicit = std::env::var_os("ILLUSTASH_ROOT").map(PathBuf::from);
    for (i, arg) in args.iter().enumerate() {
        if arg == "--root" {
            explicit = Some(PathBuf::from(args.get(i + 1).ok_or("--root 缺少路径")?));
        } else if let Some(value) = arg.strip_prefix("--root=") {
            explicit = Some(PathBuf::from(value));
        }
    }
    let root = explicit.unwrap_or_else(|| {
        let installed = app
            .path()
            .resource_dir()
            .unwrap_or_default()
            .join("resources");
        if installed.is_dir() {
            return installed;
        }
        if cfg!(debug_assertions) {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
        } else {
            installed
        }
    });
    validate_root(root)
}

fn validate_root(root: PathBuf) -> Result<PathBuf, String> {
    let root = root.canonicalize().map_err(|e| {
        format!(
            "找不到应用资源（{}）：{e}；请重新安装或指定 --root",
            root.display()
        )
    })?;
    for file in ["web/static/index.html", "tags/tags_v3.csv"] {
        if !root.join(file).is_file() {
            return Err(format!(
                "应用资源缺失：{}；请重新安装或指定 --root",
                root.join(file).display()
            ));
        }
    }
    Ok(root)
}

/// `启动进度：<per mille> <stage>` from the native host, as (fraction, stage).
fn progress(line: &str) -> Option<(f64, &str)> {
    let (permille, stage) = line.strip_prefix(PROGRESS)?.split_once(' ')?;
    let permille: u32 = permille.parse().ok().filter(|n| *n <= 1000)?;
    Some((permille as f64 / 1000.0, stage.trim()))
}

fn show_progress(main: &WebviewWindow, done: f64, stage: &str) {
    let js = format!(
        "window.showProgress && window.showProgress({done}, {})",
        serde_json::to_string(stage).unwrap()
    );
    *main.app_handle().state::<StartupPage>().0.lock().unwrap() = Some(js.clone());
    let _ = main.eval(&js);
}

/// The start page finishes its launch animation, then opens the library; `launched` makes that load
/// skip the fold it just watched. If the page cannot (its script failed), open the library directly.
fn launch(main: &WebviewWindow, mut url: Url, generation: u64) {
    url.query_pairs_mut().append_pair("launched", "1");
    let js = format!("window.launch && window.launch({})", serde_json::to_string(url.as_str()).unwrap());
    // Replayed if the start page (re)loads before it runs; a no-op on the library page, which has no launch().
    *main.app_handle().state::<StartupPage>().0.lock().unwrap() = Some(js.clone());
    let _ = main.eval(&js);
    let main = main.clone();
    thread::spawn(move || {
        thread::sleep(LAUNCH_FALLBACK);
        if current(main.app_handle(), generation)
            && on_start_page(&main)
            && main.app_handle().state::<ErrorPage>().0.lock().unwrap().is_none()
        {
            let _ = main.navigate(url);
        }
    });
}

fn start_page() -> Url {
    #[cfg(windows)]
    let home = "http://tauri.localhost/index.html";
    #[cfg(not(windows))]
    let home = "tauri://localhost/index.html";
    Url::parse(home).unwrap()
}

/// Show `js`'s page on the start page: run it there now, or open the start page, which replays it on load.
fn on_start(main: &WebviewWindow, js: &str) {
    if main.url().is_ok() && !on_start_page(main) {
        let _ = main.navigate(start_page());
    } else {
        let _ = main.eval(js);
    }
}

fn on_start_page(main: &WebviewWindow) -> bool {
    main.url().map(|u| u.scheme() == "tauri" || u.host_str() == Some("tauri.localhost")).unwrap_or(false)
}

fn ready_url(line: &str, port: u16) -> Option<Url> {
    let url = Url::parse(line.strip_prefix(READY)?.trim()).ok()?;
    (url.scheme() == "http"
        && url.host_str() == Some("127.0.0.1")
        && url.port_or_known_default() == Some(port)
        && url.username().is_empty()
        && url.password().is_none())
    .then_some(url)
}

/// 8765 as usual when free (the page keeps its origin, so its local settings survive); any free port otherwise.
fn free_port() -> u16 {
    TcpListener::bind(("127.0.0.1", PORT))
        .or_else(|_| TcpListener::bind(("127.0.0.1", 0)))
        .and_then(|l| l.local_addr())
        .map(|a| a.port())
        .unwrap_or(PORT)
}

/// Links in the main window: the local server stays inside, everything else opens in the default browser.
pub fn keep_inside(url: &Url) -> bool {
    match url.scheme() {
        "tauri" | "about" | "data" | "blob" => true,
        "http" | "https"
            if matches!(
                url.host_str(),
                Some("127.0.0.1" | "localhost" | "tauri.localhost")
            ) =>
        {
            true
        }
        _ => {
            open_outside(url);
            false
        }
    }
}

pub fn open_outside(url: &Url) {
    if matches!(url.scheme(), "http" | "https") {
        #[cfg(target_os = "macos")]
        let _ = Command::new("open").arg(url.as_str()).spawn();
        #[cfg(target_os = "windows")]
        let _ = Command::new("explorer").arg(url.as_str()).spawn();
    }
}

pub(crate) fn show_error(main: &WebviewWindow, message: &str, log: &VecDeque<String>) {
    let tail: Vec<&str> = log.iter().map(String::as_str).collect();
    let js = format!(
        "window.showError && window.showError({}, {})",
        serde_json::to_string(message).unwrap(),
        serde_json::to_string(&tail.join("\n")).unwrap()
    );
    *main.app_handle().state::<ErrorPage>().0.lock().unwrap() = Some(js.clone());
    on_start(main, &js);
}

/// Startup stopped before the service was configured: remember why, so a retry repeats it instead of waiting.
pub(crate) fn blocked(main: &WebviewWindow, message: &str) {
    *main.app_handle().state::<Launch>().blocked.lock().unwrap() = Some(message.into());
    show_error(main, message, &VecDeque::new());
}

pub fn page_loaded(main: &WebviewWindow) {
    let app = main.app_handle();
    let error = app.state::<ErrorPage>().0.lock().unwrap().clone();
    if let Some(js) = error.or_else(|| app.state::<StartupPage>().0.lock().unwrap().clone()) {
        let _ = main.eval(&js);
    }
}

/// Start the local service the command line asks for; without `--library`, the most recent library that can
/// still be opened; with none, the welcome page.
pub fn boot(app: AppHandle, main: WebviewWindow, bridge_port: u16, token: String) {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut requested = None;
    for (i, arg) in args.iter().enumerate() {
        if arg == "--port" {
            requested = args.get(i + 1).and_then(|v| v.parse::<u16>().ok());
        } else if let Some(value) = arg.strip_prefix("--port=") {
            requested = value.parse::<u16>().ok();
        }
    }
    let port = requested.unwrap_or_else(free_port);
    let root = match root(&app, &args) {
        Ok(root) => root,
        Err(error) => {
            blocked(&main, &error);
            return;
        }
    };
    let explicit = args.iter().any(|a| a == "--library" || a.starts_with("--library="));
    *app.state::<Launch>().config.lock().unwrap() = Some(Config { port, bridge_port, token, root, args });
    if explicit {
        run(&app, &main, None);
    } else if let Some(library) = illustash_terms::desktop_shell::startup_library() {
        run(&app, &main, Some(library));
    } else {
        welcome(&app, &main, false);
    }
}

/// Stop the current local service (if any) and start one on `library` (None: the same as last time),
/// on a fresh start page.
pub fn restart(app: &AppHandle, library: Option<PathBuf>) {
    let Some(main) = app.get_webview_window("main") else { return };
    app.state::<Launch>().generation.fetch_add(1, Ordering::SeqCst);
    app.state::<Child>().stop();
    app.state::<ErrorPage>().0.lock().unwrap().take();
    app.state::<StartupPage>().0.lock().unwrap().take();
    let library = library.or_else(|| app.state::<Launch>().library.lock().unwrap().clone());
    let _ = main.navigate(start_page());
    run(app, &main, library);
}

/// Stop the current local service (if any) and show the welcome page; `create` opens its new-library form.
pub fn welcome(app: &AppHandle, main: &WebviewWindow, create: bool) {
    app.state::<Launch>().generation.fetch_add(1, Ordering::SeqCst);
    let mut state = illustash_terms::desktop_shell::describe();
    state["recent"] = illustash_terms::desktop_shell::recent().into();
    state["create"] = create.into();
    let js = format!("window.showWelcome && window.showWelcome({state})");
    app.state::<ErrorPage>().0.lock().unwrap().take();
    *app.state::<StartupPage>().0.lock().unwrap() = Some(js.clone());
    on_start(main, &js);
    app.state::<Child>().stop();
}

fn run(app: &AppHandle, main: &WebviewWindow, library: Option<PathBuf>) {
    let generation = app.state::<Launch>().generation.fetch_add(1, Ordering::SeqCst) + 1;
    let Some(Config { port, bridge_port, token, root, mut args }) = app.state::<Launch>().config.lock().unwrap().clone()
    else {
        let why = app.state::<Launch>().blocked.lock().unwrap().clone().unwrap_or_else(|| "应用没有启动完成".into());
        show_error(main, &format!("{why}。处理后请退出并重新打开 Illustash"), &VecDeque::new());
        return;
    };
    if let Some(library) = &library {
        // A library picked on the start page replaces the one on the command line.
        let mut kept = Vec::new();
        let mut iter = args.into_iter();
        while let Some(arg) = iter.next() {
            if arg == "--library" {
                iter.next();
            } else if !arg.starts_with("--library=") {
                kept.push(arg);
            }
        }
        args = kept;
        args.extend(["--library".to_owned(), library.to_string_lossy().into_owned()]);
    }
    *app.state::<Launch>().library.lock().unwrap() = library;
    app.state::<Child>().stopping.store(false, Ordering::SeqCst);
    let (app, main) = (app.clone(), main.clone());
    let script = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => {
            show_error(
                &main,
                &format!("找不到核心 HTTP 程序：{e}"),
                &VecDeque::new(),
            );
            return;
        }
    };
    let mut command = Command::new(&script);
    command
        .arg("http")
        .arg("--native")
        .arg("--root")
        .arg(&root)
        .args(["--port", &port.to_string(), "--no-open"])
        .args(args) // 参数由同一 CLI 校验；不重复实现图库或网络授权规则
        .arg("--root")
        .arg(&root)
        .current_dir(&root)
        .env(
            "ILLUSTASH_DESKTOP_BRIDGE",
            format!("http://127.0.0.1:{bridge_port}"),
        )
        .env("ILLUSTASH_DESKTOP_TOKEN", token)
        .env("ILLUSTASH_DESKTOP_STDIN", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    illustash_platform::platform_process::prepare_desktop(&mut command);
    let log = Arc::new(Mutex::new(VecDeque::<String>::new()));
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => {
            show_error(
                &main,
                &format!("无法启动本地服务（{}）：{e}", script.display()),
                &VecDeque::new(),
            );
            return;
        }
    };
    {
        match illustash_platform::platform_process::DesktopJob::assign(&child) {
            Ok(job) => *app.state::<Child>().job.lock().unwrap() = Some(job),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                show_error(
                    &main,
                    &format!("无法管理本地服务进程：{error}；请重新启动应用"),
                    &VecDeque::new(),
                );
                return;
            }
        }
    }
    if let Err(error) = child.stdin.as_mut().unwrap().write_all(&[1]) {
        let _ = child.kill();
        let _ = child.wait();
        show_error(
            &main,
            &format!("无法连接本地服务：{error}；请重新启动应用"),
            &VecDeque::new(),
        );
        return;
    }
    let ready = Arc::new(AtomicBool::new(false));
    let active = Arc::new(Mutex::new(Instant::now()));

    fn pump(
        stream: impl Read + Send + 'static,
        log: Arc<Mutex<VecDeque<String>>>,
        active: Arc<Mutex<Instant>>,
        on_line: impl Fn(&str) -> bool + Send + 'static,
    ) {
        thread::spawn(move || {
            for line in BufReader::new(stream).lines().map_while(Result::ok) {
                *active.lock().unwrap() = Instant::now();
                // Progress lines are consumed by the start page; the error tail keeps real output.
                if !on_line(&line) {
                    continue;
                }
                eprintln!("{line}");
                let mut log = log.lock().unwrap();
                log.push_back(line);
                if log.len() > 30 {
                    log.pop_front();
                }
            }
        });
    }
    let (win, flag) = (main.clone(), ready.clone());
    pump(child.stdout.take().unwrap(), log.clone(), active.clone(), move |line| {
        if !current(win.app_handle(), generation) {
            return false;
        }
        if let Some((done, stage)) = progress(line) {
            if !flag.load(Ordering::SeqCst) {
                show_progress(&win, done, stage);
            }
            return false;
        }
        if let Some(url) = ready_url(line, port) {
            if !flag.swap(true, Ordering::SeqCst) {
                launch(&win, url, generation);
            }
        }
        true
    });
    pump(child.stderr.take().unwrap(), log.clone(), active.clone(), |_| true);
    *app.state::<Child>().process.lock().unwrap() = Some(child);
    thread::spawn(move || {
        let status = loop {
            if !current(&app, generation) || app.state::<Child>().stopping.load(Ordering::SeqCst) {
                return;
            }
            let outcome = {
                let state = app.state::<Child>();
                let mut process = state.process.lock().unwrap();
                let Some(child) = process.as_mut() else {
                    return;
                };
                match child.try_wait() {
                    Ok(None) => None,
                    outcome => {
                        process.take();
                        Some(outcome)
                    }
                }
            };
            if let Some(status) = outcome {
                break status;
            }
            if !ready.load(Ordering::SeqCst) && active.lock().unwrap().elapsed() > STALL {
                app.state::<Child>().stop();
                show_error(
                    &main,
                    "本地服务 30 秒没有任何进展，已停止；可以重试，或选择其他图库",
                    &log.lock().unwrap(),
                );
                return;
            }
            thread::sleep(Duration::from_millis(50));
        };
        app.state::<Child>().job.lock().unwrap().take();
        if !current(&app, generation) || app.state::<Child>().stopping.load(Ordering::SeqCst) {
            return;
        }
        thread::sleep(Duration::from_millis(100));
        let why = match status {
            Ok(Some(s)) => {
                format!("本地服务已退出（{s}）；请查看下方日志，可以重试或选择其他图库")
            }
            Err(e) => format!("本地服务异常：{e}；请重新启动应用"),
            _ => unreachable!(),
        };
        show_error(&main, &why, &log.lock().unwrap());
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ready_requires_exact_native_origin() {
        assert_eq!(
            ready_url("图库已启动：http://127.0.0.1:9123", 9123)
                .unwrap()
                .as_str(),
            "http://127.0.0.1:9123/"
        );
        for bad in [
            "noise 图库已启动：http://127.0.0.1:9123",
            "图库已启动：https://127.0.0.1:9123",
            "图库已启动：http://example.com:9123",
            "图库已启动：http://127.0.0.1:9124",
            "图库已启动：http://user@127.0.0.1:9123",
        ] {
            assert!(ready_url(bad, 9123).is_none());
        }
    }
    #[test]
    fn progress_requires_exact_prefix_and_per_mille() {
        assert_eq!(
            progress("启动进度：421 正在检查图库数据（source.sqlite）"),
            Some((0.421, "正在检查图库数据（source.sqlite）"))
        );
        assert_eq!(progress("启动进度：1000 完成"), Some((1.0, "完成")));
        for bad in [
            "noise 启动进度：10 x",
            "启动进度：1001 x",
            "启动进度：-1 x",
            "启动进度：10",
            "图库已启动：http://127.0.0.1:9123",
        ] {
            assert!(progress(bad).is_none(), "{bad}");
        }
    }
    #[test]
    fn missing_resources_give_reinstall_action() {
        assert!(
            validate_root(PathBuf::from("/nonexistent/illustash-synthetic-resources"))
                .unwrap_err()
                .contains("重新安装")
        );
    }
    #[cfg(unix)]
    #[test]
    fn stop_waits_for_owned_child_and_is_idempotent() {
        let process = Command::new("/bin/sh")
            .args(["-c", "cat >/dev/null; exit 0"])
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        let pid = process.id();
        let child = Child::default();
        *child.process.lock().unwrap() = Some(process);
        child.stop();
        child.stop();
        assert!(child.process.lock().unwrap().is_none());
        assert_eq!(unsafe { libc::kill(pid as i32, 0) }, -1);
    }
    #[cfg(unix)]
    #[test]
    fn forced_cleanup_includes_descendants_and_leaves_bystander_alive() {
        use std::os::unix::process::CommandExt;
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "/bin/sleep 60 & echo $!; wait"])
            .stdout(Stdio::piped())
            .process_group(0);
        let mut host = command.spawn().unwrap();
        let mut line = String::new();
        BufReader::new(host.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let descendant: i32 = line.trim().parse().unwrap();
        let mut bystander = Command::new("/bin/sleep").arg("60").spawn().unwrap();
        illustash_platform::platform_process::kill_owned_tree(host.id());
        host.wait().unwrap();
        // The killed descendant cannot still execute; a transient zombie may await reaping.
        let state = Command::new("/bin/ps")
            .args(["-o", "stat=", "-p", &descendant.to_string()])
            .output()
            .unwrap();
        assert!(
            String::from_utf8_lossy(&state.stdout).trim().is_empty()
                || String::from_utf8_lossy(&state.stdout)
                    .trim()
                    .starts_with('Z')
        );
        assert!(bystander.try_wait().unwrap().is_none());
        bystander.kill().unwrap();
        bystander.wait().unwrap();
    }
}
