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

use crate::i18n::{tr, tr_with};

const PORT: u16 = 8765;
const READY: &str = "图库已启动：";
const PROGRESS: &str = "启动进度：";
const STOP_GRACE: Duration = Duration::from_secs(10);
/// 折叠动画大约三秒；超过这个时间就认为启动页没法自己打开图库。
const LAUNCH_FALLBACK: Duration = Duration::from_secs(8);
/// 大图库光完整性检查就要几十秒，所以只有一直没有输出才算卡住：每一行输出，包括每次进度报告，都重新开始计时。
const STALL: Duration = Duration::from_secs(30);

/// 只管这个窗口启动的原生宿主。停止时等它清理完任务。
#[derive(Default)]
pub struct Child {
    process: Mutex<Option<std::process::Child>>,
    stopping: AtomicBool,
    job: Mutex<Option<illustash_platform::platform_process::DesktopJob>>,
}

impl Child {
    pub fn stop(&self) {
        self.stopping.store(true, Ordering::SeqCst);
        let mut process = self.process.lock().expect("lock poisoned by an earlier panic");
        let Some(mut child) = process.take() else {
            return;
        };
        // 关闭我们这一端的写入在父进程异常退出时同样有效，所有平台都是。
        drop(child.stdin.take());
        let deadline = Instant::now() + STOP_GRACE;
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                _ if Instant::now() >= deadline => {
                    illustash_platform::platform_process::kill_owned_tree(child.id());
                    self.job.lock().expect("lock poisoned by an earlier panic").take();
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
                _ => thread::sleep(Duration::from_millis(50)),
            }
        }
        self.job.lock().expect("lock poisoned by an earlier panic").take();
    }
}

#[derive(Default)]
pub struct ErrorPage(Mutex<Option<String>>);

/// 最近一次的启动进度脚本；发出之后启动页（重新）加载时再执行一遍。
#[derive(Default)]
pub struct StartupPage(Mutex<Option<String>>);

/// 同一时间只有一个本地服务。每次启动取一个新的 generation；旧 generation（在启动页上取消，或被重试、
/// 另一个图库取代）的进度、就绪和退出都忽略。
#[derive(Default)]
pub struct Launch {
    generation: AtomicU64,
    config: Mutex<Option<Config>>,
    /// 最近一次启动的图库（None：命令行或登记表选的那个），供重试用。
    library: Mutex<Option<PathBuf>>,
    /// 服务还没配置好启动就停下的原因；重试也过不去。
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
    app.state::<Launch>().config.lock().expect("lock poisoned by an earlier panic").as_ref().map(|c| c.root.clone())
}

/// 与 #69 共用的资源布局：resource_dir/resources；明确给出的 root 优先。
fn root(app: &AppHandle, args: &[String]) -> Result<PathBuf, String> {
    let mut explicit = std::env::var_os("ILLUSTASH_ROOT").map(PathBuf::from);
    for (i, arg) in args.iter().enumerate() {
        if arg == "--root" {
            explicit = Some(PathBuf::from(args.get(i + 1).ok_or_else(|| tr("--root 缺少路径"))?));
        } else if let Some(value) = arg.strip_prefix("--root=") {
            explicit = Some(PathBuf::from(value));
        }
    }
    let root = explicit.unwrap_or_else(|| {
        let installed = app.path().resource_dir().unwrap_or_default().join("resources");
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
        tr_with(
            "找不到应用资源（{path}）：{error}；请重新安装或指定 --root",
            &[("path", &root.display()), ("error", &e)],
        )
    })?;
    for file in ["web/static/index.html", "tags/tags_v3.csv"] {
        if !root.join(file).is_file() {
            return Err(tr_with(
                "应用资源缺失：{path}；请重新安装或指定 --root",
                &[("path", &root.join(file).display())],
            ));
        }
    }
    Ok(root)
}

/// 原生宿主输出的 `启动进度：<per mille> <stage>`，解析为 (fraction, stage)。
fn progress(line: &str) -> Option<(f64, &str)> {
    let (permille, stage) = line.strip_prefix(PROGRESS)?.split_once(' ')?;
    let permille: u32 = permille.parse().ok().filter(|n| *n <= 1000)?;
    Some((permille as f64 / 1000.0, stage.trim()))
}

fn show_progress(main: &WebviewWindow, done: f64, stage: &str) {
    let js = format!(
        "window.showProgress && window.showProgress({done}, {})",
        serde_json::to_string(stage).expect("strings always serialize")
    );
    *main.app_handle().state::<StartupPage>().0.lock().expect("lock poisoned by an earlier panic") = Some(js.clone());
    let _ = main.eval(&js);
}

/// 启动页播完启动动画再打开图库；`launched` 让这次加载跳过刚看过的折叠动画。页面做不到时（脚本出错），
/// 直接打开图库。`url` 是宿主就绪时给出的带一次性码的地址：第一次加载拿它换会话 cookie，
/// 所以这个地址只能用一次，新的服务会打印新的地址。
fn launch(main: &WebviewWindow, mut url: Url, generation: u64) {
    url.query_pairs_mut().append_pair("launched", "1");
    let js = format!(
        "window.launch && window.launch({})",
        serde_json::to_string(url.as_str()).expect("strings always serialize")
    );
    // 启动页在它执行之前（重新）加载时再执行一遍；在图库页上什么都不做，图库页没有 launch()。
    *main.app_handle().state::<StartupPage>().0.lock().expect("lock poisoned by an earlier panic") = Some(js.clone());
    let _ = main.eval(&js);
    let main = main.clone();
    thread::spawn(move || {
        thread::sleep(LAUNCH_FALLBACK);
        if current(main.app_handle(), generation)
            && on_start_page(&main)
            && main.app_handle().state::<ErrorPage>().0.lock().expect("lock poisoned by an earlier panic").is_none()
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
    Url::parse(home).expect("constant URL")
}

/// 在启动页上显示 `js` 对应的页面：当前就在启动页则立即执行，否则打开启动页，由它在加载时再执行。
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

/// 8765 空闲时照常用它（页面的 origin 不变，本地设置得以保留）；否则用任意空闲端口。
fn free_port() -> u16 {
    TcpListener::bind(("127.0.0.1", PORT))
        .or_else(|_| TcpListener::bind(("127.0.0.1", 0)))
        .and_then(|l| l.local_addr())
        .map(|a| a.port())
        .unwrap_or(PORT)
}

/// 主窗口里的链接：本地服务器留在窗口里，其他一律用默认浏览器打开。
pub fn keep_inside(url: &Url) -> bool {
    match url.scheme() {
        "tauri" | "about" | "data" | "blob" => true,
        "http" | "https" if matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "tauri.localhost")) => true,
        _ => {
            open_outside(url);
            false
        }
    }
}

/// 系统设置 › 隐私与安全性 › 照片：同步被拒绝访问照片图库时，页面把用户引到这里（#202）。
pub const PHOTOS_PRIVACY: &str = "x-apple.systempreferences:com.apple.preference.security?Privacy_Photos";

pub fn open_outside(url: &Url) {
    if matches!(url.scheme(), "http" | "https") || (cfg!(target_os = "macos") && url.as_str() == PHOTOS_PRIVACY) {
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
        serde_json::to_string(message).expect("strings always serialize"),
        serde_json::to_string(&tail.join("\n")).expect("strings always serialize")
    );
    *main.app_handle().state::<ErrorPage>().0.lock().expect("lock poisoned by an earlier panic") = Some(js.clone());
    on_start(main, &js);
}

/// 启动页询问是否升级名为 `name` 的图库（#193）；和失败页一样会再执行。
fn show_upgrade(main: &WebviewWindow, name: &str) {
    let js = format!(
        "window.showUpgrade && window.showUpgrade({}, {})",
        serde_json::to_string(name).expect("strings always serialize"),
        serde_json::to_string(illustash_terms::desktop_shell::upgrade_note()).expect("strings always serialize")
    );
    *main.app_handle().state::<ErrorPage>().0.lock().expect("lock poisoned by an earlier panic") = Some(js.clone());
    on_start(main, &js);
}

/// 检查发现图库数据损坏（#212）：说明怎么办，带上常规的重试和选择，损坏在 source.sqlite 时则给出「抢救重建」
/// （#228）；和失败页一样会再执行。
fn show_damage(main: &WebviewWindow, note: &str, rescue: bool) {
    let js = format!(
        "window.showDamage && window.showDamage({}, {rescue})",
        serde_json::to_string(note).expect("strings always serialize")
    );
    *main.app_handle().state::<ErrorPage>().0.lock().expect("lock poisoned by an earlier panic") = Some(js.clone());
    on_start(main, &js);
}

/// 服务配置好之前启动就停下了：记住原因，重试时直接再报一次，而不是等待。
pub(crate) fn blocked(main: &WebviewWindow, message: &str) {
    *main.app_handle().state::<Launch>().blocked.lock().expect("lock poisoned by an earlier panic") =
        Some(message.into());
    show_error(main, message, &VecDeque::new());
}

pub fn page_loaded(main: &WebviewWindow) {
    let app = main.app_handle();
    let error = app.state::<ErrorPage>().0.lock().expect("lock poisoned by an earlier panic").clone();
    if let Some(js) =
        error.or_else(|| app.state::<StartupPage>().0.lock().expect("lock poisoned by an earlier panic").clone())
    {
        let _ = main.eval(&js);
    }
}

/// 启动命令行要求的本地服务；没有 `--library` 时打开最近一个还能打开的图库；一个都没有就显示欢迎页。
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
    *app.state::<Launch>().config.lock().expect("lock poisoned by an earlier panic") =
        Some(Config { port, bridge_port, token, root, args });
    if explicit {
        run(&app, &main, None, Start::Open);
    } else if let Some(library) = illustash_terms::desktop_shell::startup_library() {
        run(&app, &main, Some(library), Start::Open);
    } else {
        welcome(&app, &main, false);
    }
}

/// 停掉当前的本地服务（如果有），在新的启动页上对 `library`（None：与上次相同）启动一个。
pub fn restart(app: &AppHandle, library: Option<PathBuf>) {
    start_again(app, library, Start::Open);
}

/// 用户在启动页上同意升级打开失败的图库（#193）：用 `--upgrade` 再次对它启动服务，
/// 这会先备份，升级没完成时图库保持不变。
pub fn upgrade(app: &AppHandle) {
    start_again(app, None, Start::Upgrade);
}

/// 检查发现 source.sqlite 损坏后，用户在启动页上选了「抢救重建」（#228）：用 `--rescue-source` 再次对这个图库
/// 启动服务，它先重建再打开；启动页显示抢救的进度。
pub fn rescue(app: &AppHandle) {
    start_again(app, None, Start::Rescue);
}

/// 对图库启动服务的方式：照常启动，或在用户同意升级、抢救之后启动。
#[derive(Clone, Copy, PartialEq)]
enum Start {
    Open,
    Upgrade,
    Rescue,
}

fn start_again(app: &AppHandle, library: Option<PathBuf>, start: Start) {
    let Some(main) = app.get_webview_window("main") else { return };
    app.state::<Launch>().generation.fetch_add(1, Ordering::SeqCst);
    app.state::<Child>().stop();
    app.state::<ErrorPage>().0.lock().expect("lock poisoned by an earlier panic").take();
    app.state::<StartupPage>().0.lock().expect("lock poisoned by an earlier panic").take();
    let library =
        library.or_else(|| app.state::<Launch>().library.lock().expect("lock poisoned by an earlier panic").clone());
    let _ = main.navigate(start_page());
    run(app, &main, library, start);
}

/// 停掉当前的本地服务（如果有），显示欢迎页；`create` 打开它的新建图库表单。
pub fn welcome(app: &AppHandle, main: &WebviewWindow, create: bool) {
    app.state::<Launch>().generation.fetch_add(1, Ordering::SeqCst);
    let mut state = illustash_terms::desktop_shell::describe();
    state["recent"] = illustash_terms::desktop_shell::recent().into();
    state["create"] = create.into();
    let js = format!("window.showWelcome && window.showWelcome({state})");
    app.state::<ErrorPage>().0.lock().expect("lock poisoned by an earlier panic").take();
    *app.state::<StartupPage>().0.lock().expect("lock poisoned by an earlier panic") = Some(js.clone());
    on_start(main, &js);
    app.state::<Child>().stop();
}

fn run(app: &AppHandle, main: &WebviewWindow, library: Option<PathBuf>, start: Start) {
    let generation = app.state::<Launch>().generation.fetch_add(1, Ordering::SeqCst) + 1;
    let Some(Config { port, bridge_port, token, root, mut args }) =
        app.state::<Launch>().config.lock().expect("lock poisoned by an earlier panic").clone()
    else {
        let why = app
            .state::<Launch>()
            .blocked
            .lock()
            .expect("lock poisoned by an earlier panic")
            .clone()
            .unwrap_or_else(|| tr("应用没有启动完成"));
        show_error(main, &tr_with("{why}。处理后请退出并重新打开 Illustash", &[("why", &why)]), &VecDeque::new());
        return;
    };
    if let Some(library) = &library {
        // 在启动页上选的图库取代命令行里的那个。
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
    // 这个服务要打开的图库，打开失败时用来询问是否升级（#193）。
    let target = library.clone().or_else(|| {
        let at = args.iter().position(|a| a == "--library");
        at.and_then(|i| args.get(i + 1))
            .map(PathBuf::from)
            .or_else(|| args.iter().find_map(|a| a.strip_prefix("--library=")).map(PathBuf::from))
    });
    match start {
        Start::Open => {}
        Start::Upgrade => args.push("--upgrade".to_owned()),
        Start::Rescue => args.push("--rescue-source".to_owned()),
    }
    *app.state::<Launch>().library.lock().expect("lock poisoned by an earlier panic") = library;
    app.state::<Child>().stopping.store(false, Ordering::SeqCst);
    let (app, main) = (app.clone(), main.clone());
    let script = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => {
            show_error(&main, &tr_with("找不到核心 HTTP 程序：{error}", &[("error", &e)]), &VecDeque::new());
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
        .env("ILLUSTASH_DESKTOP_BRIDGE", format!("http://127.0.0.1:{bridge_port}"))
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
                &tr_with("无法启动本地服务（{path}）：{error}", &[("path", &script.display()), ("error", &e)]),
                &VecDeque::new(),
            );
            return;
        }
    };
    {
        match illustash_platform::platform_process::DesktopJob::assign(&child) {
            Ok(job) => *app.state::<Child>().job.lock().expect("lock poisoned by an earlier panic") = Some(job),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                show_error(
                    &main,
                    &tr_with("无法管理本地服务进程：{error}；请重新启动应用", &[("error", &error)]),
                    &VecDeque::new(),
                );
                return;
            }
        }
    }
    if let Err(error) = child.stdin.as_mut().expect("stdin is piped").write_all(&[1]) {
        let _ = child.kill();
        let _ = child.wait();
        show_error(
            &main,
            &tr_with("无法连接本地服务：{error}；请重新启动应用", &[("error", &error)]),
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
                *active.lock().expect("lock poisoned by an earlier panic") = Instant::now();
                // 进度行交给启动页；错误尾部只保留真正的输出。
                if !on_line(&line) {
                    continue;
                }
                eprintln!("{line}");
                let mut log = log.lock().expect("lock poisoned by an earlier panic");
                log.push_back(line);
                if log.len() > 30 {
                    log.pop_front();
                }
            }
        });
    }
    let (win, flag) = (main.clone(), ready.clone());
    pump(child.stdout.take().expect("stdout is piped"), log.clone(), active.clone(), move |line| {
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
    pump(child.stderr.take().expect("stderr is piped"), log.clone(), active.clone(), |_| true);
    *app.state::<Child>().process.lock().expect("lock poisoned by an earlier panic") = Some(child);
    thread::spawn(move || {
        let status = loop {
            if !current(&app, generation) || app.state::<Child>().stopping.load(Ordering::SeqCst) {
                return;
            }
            let outcome = {
                let state = app.state::<Child>();
                let mut process = state.process.lock().expect("lock poisoned by an earlier panic");
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
            if !ready.load(Ordering::SeqCst)
                && active.lock().expect("lock poisoned by an earlier panic").elapsed() > STALL
            {
                app.state::<Child>().stop();
                show_error(
                    &main,
                    &tr("本地服务 30 秒没有任何进展，已停止；可以重试，或选择其他图库"),
                    &log.lock().expect("lock poisoned by an earlier panic"),
                );
                return;
            }
            thread::sleep(Duration::from_millis(50));
        };
        app.state::<Child>().job.lock().expect("lock poisoned by an earlier panic").take();
        if !current(&app, generation) || app.state::<Child>().stopping.load(Ordering::SeqCst) {
            return;
        }
        thread::sleep(Duration::from_millis(100));
        // 旧格式的图库：先询问再升级，而不是显示失败（#193）。升级失败后显示失败本身；图库包没有改动，可以再试。
        // 检查发现的损坏优先：在数据库被替换之前图库包一律拒绝打开。没做完的抢救改为显示它自己的失败（#228）；
        // 图库包保持原样，损坏记录仍在，所以重试时会再次提供抢救。
        if start != Start::Rescue && !ready.load(Ordering::SeqCst) {
            if let Some(note) = target.as_deref().and_then(illustash_terms::desktop_shell::damage_note) {
                let rescue = target.as_deref().is_some_and(illustash_terms::desktop_shell::damage_rescuable);
                show_damage(&main, note, rescue);
                return;
            }
        }
        if start == Start::Open && !ready.load(Ordering::SeqCst) {
            if let Some(name) = target.as_deref().and_then(illustash_terms::desktop_shell::upgrade_needed) {
                show_upgrade(&main, &name);
                return;
            }
        }
        let why = match status {
            Ok(Some(s)) => {
                tr_with("本地服务已退出（{status}）；请查看下方日志，可以重试或选择其他图库", &[("status", &s)])
            }
            Err(e) => tr_with("本地服务异常：{error}；请重新启动应用", &[("error", &e)]),
            _ => unreachable!(),
        };
        show_error(&main, &why, &log.lock().expect("lock poisoned by an earlier panic"));
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ready_requires_exact_native_origin() {
        assert_eq!(ready_url("图库已启动：http://127.0.0.1:9123", 9123).unwrap().as_str(), "http://127.0.0.1:9123/");
        // 宿主打印带一次性码的地址，这个码用来换会话 cookie；launch() 再加上 `launched`，宿主在交换过程中保留它。
        let mut url = ready_url("图库已启动：http://127.0.0.1:9123/?k=0123abcd", 9123).unwrap();
        assert_eq!(url.query(), Some("k=0123abcd"));
        url.query_pairs_mut().append_pair("launched", "1");
        assert_eq!(url.as_str(), "http://127.0.0.1:9123/?k=0123abcd&launched=1");
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
    fn photos_privacy_link_is_matched_as_written() {
        // 页面的链接（web/static/core/jobs.js）到这里时已经解析过；只有正好是这个地址才会离开窗口（#202）。
        assert_eq!(Url::parse(PHOTOS_PRIVACY).unwrap().as_str(), PHOTOS_PRIVACY);
    }
    #[test]
    fn missing_resources_give_reinstall_action() {
        assert!(validate_root(PathBuf::from("/nonexistent/illustash-synthetic-resources"))
            .unwrap_err()
            .contains("重新安装"));
    }
    #[cfg(unix)]
    #[test]
    fn stop_waits_for_owned_child_and_is_idempotent() {
        let process =
            Command::new("/bin/sh").args(["-c", "cat >/dev/null; exit 0"]).stdin(Stdio::piped()).spawn().unwrap();
        let pid = process.id();
        let child = Child::default();
        *child.process.lock().expect("lock poisoned by an earlier panic") = Some(process);
        child.stop();
        child.stop();
        assert!(child.process.lock().expect("lock poisoned by an earlier panic").is_none());
        // SAFETY: kill 的信号 0 只检查进程是否存在，不发送信号。
        assert_eq!(unsafe { libc::kill(pid as i32, 0) }, -1);
    }
    #[cfg(unix)]
    #[test]
    fn forced_cleanup_includes_descendants_and_leaves_bystander_alive() {
        use std::os::unix::process::CommandExt;
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "/bin/sleep 60 & echo $!; wait"]).stdout(Stdio::piped()).process_group(0);
        let mut host = command.spawn().unwrap();
        let mut line = String::new();
        BufReader::new(host.stdout.take().unwrap()).read_line(&mut line).unwrap();
        let descendant: i32 = line.trim().parse().unwrap();
        let mut bystander = Command::new("/bin/sleep").arg("60").spawn().unwrap();
        illustash_platform::platform_process::kill_owned_tree(host.id());
        host.wait().unwrap();
        // 被杀掉的后代进程不可能还在执行；可能有一个等待回收的短暂僵尸进程。
        let state = Command::new("/bin/ps").args(["-o", "stat=", "-p", &descendant.to_string()]).output().unwrap();
        assert!(
            String::from_utf8_lossy(&state.stdout).trim().is_empty()
                || String::from_utf8_lossy(&state.stdout).trim().starts_with('Z')
        );
        assert!(bystander.try_wait().unwrap().is_none());
        bystander.kill().unwrap();
        bystander.wait().unwrap();
    }
}
