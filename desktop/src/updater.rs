//! 应用内更新（#279，B 档）：在「有新版本」的提示里下载并重启。哪个版本由检查更新决定（native/terms/src/app_updates.rs，
//! 通道规则也在那里），原生宿主经本机桥（bridge.rs 的 `/platform/update/*`）转达用户的点击；这里用 Tauri 更新插件
//! 下载这个版本 Release 里的 latest.json 和本平台的包，用写在 tauri.conf.json 里的公钥校验签名（签名同时绑定版本号，
//! `requireSignedVersion`），再安装、重启。
//!
//! - 联网：只在用户点了「下载并重启」之后。请求发往 github.com 上这个版本的 Release（下载时跟随 GitHub 到它自己的
//!   文件服务器的跳转），只带插件固定的 User-Agent，地址里没有本机版本号、账号或图库信息；用系统代理设置。
//! - 下好的包只在内存里，退出 App 就丢掉，下次重新下载。
//! - 安装前先停掉本地服务，等它把任务收尾（`server::Child::stop`）：Windows 的安装器会强行结束仍在运行的同名程序，
//!   而本地服务就是同一个程序。安装失败时重新启动本地服务，页面退回「打开下载页」。
//! - 不能自己更新的安装（没有公钥的构建、不是打好的包、macOS 上从「下载」直接运行而被系统隔离到随机位置的）只提供
//!   「打开下载页」。
use std::error::Error as _;
use std::future::Future;
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

use serde_json::{json, Value};
use tauri::{AppHandle, Manager, Url};
use tauri_plugin_updater::{Update, UpdaterExt};

use crate::i18n::{tr, tr_with};
use crate::server;

/// 连上 GitHub 的时限；连上之后只要一直有数据就不限总时长，几十秒收不到数据才算断了。
const CONNECT: Duration = Duration::from_secs(15);
const STALL: Duration = Duration::from_secs(60);
const RETRY_DELAY: Duration = Duration::from_secs(1);

#[derive(Default)]
pub struct Updates(Mutex<State>);

#[derive(Default)]
struct State {
    stage: Stage,
    /// 每次开始或取消下载都换一个：被取消的那次下载回来时不再改状态。
    generation: u64,
    task: Option<tauri::async_runtime::JoinHandle<()>>,
}

#[derive(Default)]
enum Stage {
    #[default]
    Idle,
    Downloading {
        version: String,
        received: u64,
        total: Option<u64>,
    },
    /// 下好并通过了签名校验，等用户确认重启。
    Ready {
        version: String,
        update: Box<Update>,
        bytes: Vec<u8>,
    },
    Failed {
        version: String,
        error: String,
    },
    Installing {
        version: String,
    },
}

impl Updates {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.0.lock().expect("更新状态锁被之前的 panic 毒化")
    }
}

/// 这个安装能不能自己更新；不能时给出原因（页面据此说明为什么只有「打开下载页」）。
fn unavailable(app: &AppHandle) -> Option<&'static str> {
    let pubkey = app
        .config()
        .plugins
        .0
        .get("updater")
        .and_then(|updater| updater.get("pubkey"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if pubkey.trim().is_empty() {
        return Some("unsigned");
    }
    let Ok(exe) = std::env::current_exe() else { return Some("unpackaged") };
    packaged(&exe)
}

/// macOS：程序要在 `….app/Contents/MacOS/` 里；从「下载」等位置直接打开、被系统隔离运行（App Translocation）时，
/// 包在只读的随机位置，装不进去。Windows：要是 NSIS 安装包装的。
#[cfg(target_os = "macos")]
fn packaged(exe: &std::path::Path) -> Option<&'static str> {
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    let bundle = contents.parent()?;
    let packaged = macos.file_name()? == "MacOS"
        && contents.file_name()? == "Contents"
        && bundle.extension().is_some_and(|e| e == "app");
    if !packaged {
        return Some("unpackaged");
    }
    exe.components().any(|c| c.as_os_str() == "AppTranslocation").then_some("translocated")
}
#[cfg(windows)]
fn packaged(_: &std::path::Path) -> Option<&'static str> {
    use tauri::utils::{config::BundleType, platform::bundle_type};
    (bundle_type() != Some(BundleType::Nsis)).then_some("unpackaged")
}
#[cfg(not(any(windows, target_os = "macos")))]
fn packaged(_: &std::path::Path) -> Option<&'static str> {
    Some("unpackaged")
}

/// GET /platform/update/status：给页面的状态（经 `/api/updates` 的 `install`）。
pub fn status(app: &AppHandle) -> Value {
    let reason = unavailable(app);
    let state = app.state::<Updates>();
    let state = state.lock();
    let mut value = json!({"available": reason.is_none(), "reason": reason, "stage": "idle"});
    match &state.stage {
        Stage::Idle => {}
        Stage::Downloading { version, received, total } => {
            value["stage"] = "downloading".into();
            value["version"] = version.as_str().into();
            value["received"] = (*received).into();
            value["total"] = (*total).into();
        }
        Stage::Ready { version, .. } => {
            value["stage"] = "ready".into();
            value["version"] = version.as_str().into();
        }
        Stage::Failed { version, error } => {
            value["stage"] = "failed".into();
            value["version"] = version.as_str().into();
            value["error"] = error.as_str().into();
        }
        Stage::Installing { version } => {
            value["stage"] = "installing".into();
            value["version"] = version.as_str().into();
        }
    }
    value
}

/// POST /platform/update/download?version=X.Y.Z：开始下载；同一个版本已经在下或已经下好时什么都不做。
pub fn download(app: &AppHandle, version: &str) -> Result<Value, String> {
    if unavailable(app).is_some() {
        return Err(tr("这个安装不能自己更新，请打开下载页手动更新"));
    }
    let package = illustash_terms::app_updates::package(version).map_err(|error| {
        eprintln!("[updater] 不下载 {version}：{error}");
        tr("这个版本不能安装")
    })?;
    let manifest = Url::parse(&package.manifest).map_err(|e| e.to_string())?;
    let state = app.state::<Updates>();
    let mut state = state.lock();
    match &state.stage {
        Stage::Installing { .. } => return Err(tr("正在安装更新")),
        Stage::Downloading { version: v, .. } | Stage::Ready { version: v, .. } if v == version => {
            drop(state);
            return Ok(status(app));
        }
        _ => {}
    }
    if let Some(task) = state.task.take() {
        task.abort();
    }
    state.generation += 1;
    let generation = state.generation;
    state.stage = Stage::Downloading { version: version.to_owned(), received: 0, total: None };
    let (handle, version) = (app.clone(), version.to_owned());
    state.task = Some(tauri::async_runtime::spawn(async move {
        let result = fetch(&handle, manifest, &package.prefix, &version, generation).await;
        let state = handle.state::<Updates>();
        let mut state = state.lock();
        if state.generation != generation {
            return;
        }
        state.task = None;
        state.stage = match result {
            Ok((update, bytes)) => Stage::Ready { version, update: Box::new(update), bytes },
            Err(error) => {
                eprintln!("[updater] {version} 没有下载成功：{error}");
                Stage::Failed { version, error }
            }
        };
    }));
    drop(state);
    Ok(status(app))
}

async fn fetch(
    app: &AppHandle,
    manifest: Url,
    prefix: &str,
    version: &str,
    generation: u64,
) -> Result<(Update, Vec<u8>), String> {
    let expected = version.to_owned();
    let updater = app
        .updater_builder()
        .endpoints(vec![manifest])
        .map_err(describe)?
        // latest.json 说的版本必须就是要下载的这个；签名里绑定的版本号由插件另外核对（requireSignedVersion）。
        .version_comparator(move |_, release| release.version.to_string() == expected)
        .configure_client(|client| client.connect_timeout(CONNECT).read_timeout(STALL))
        .build()
        .map_err(describe)?;
    let update = with_network_retry("检查应用更新", RETRY_DELAY, || updater.check())
        .await?
        .ok_or_else(|| tr("这个版本没有提供本机能用的更新包"))?;
    // 包要在同一个 Release 里：latest.json 只是普通文件，不信任它指向别处。
    if !update.download_url.as_str().starts_with(prefix) {
        return Err(tr("更新包不在这个版本的 Release 里，没有下载"));
    }
    let bytes = with_network_retry("下载应用更新", RETRY_DELAY, || {
        let mut bytes = 0;
        let progress = move |chunk, total| {
            bytes += chunk as u64;
            let state = app.state::<Updates>();
            let mut state = state.lock();
            if state.generation == generation {
                if let Stage::Downloading { received, total: known, .. } = &mut state.stage {
                    *received = bytes;
                    *known = total;
                }
            }
        };
        update.download(progress, || {})
    })
    .await?;
    Ok((update, bytes))
}

/// 只重试下载清单和更新包时的暂时性网络错误。插件每次独立下载并验签，取消任务也会取消退避。
async fn with_network_retry<T, F>(
    stage: &'static str,
    delay: Duration,
    mut request: impl FnMut() -> F,
) -> Result<T, String>
where
    F: Future<Output = Result<T, tauri_plugin_updater::Error>>,
{
    use tauri_plugin_updater::Error as E;
    let mut retries = 0;
    loop {
        let (retryable, detail) = match request().await {
            Ok(value) => return Ok(value),
            Err(E::Reqwest(error)) => {
                let error = error.without_url();
                let mut retryable = error.is_connect()
                    || error.is_timeout()
                    || error.is_body()
                    || error.is_request()
                    || error.is_decode();
                let mut detail = format!("Reqwest: {error}");
                let mut cause = error.source();
                while let Some(inner) = cause {
                    // JSON 格式错误也用 reqwest 的 decode 表示，重新下载不会修好它。
                    if inner.is::<serde_json::Error>() {
                        retryable = false;
                    }
                    detail.push_str(&format!(": {inner}"));
                    cause = inner.source();
                }
                (retryable, detail)
            }
            // 锁定的 updater 插件用这一格式返回更新包的非成功 HTTP 状态。
            Err(E::Network(message)) => {
                let status = message
                    .strip_prefix("Download request failed with status: ")
                    .and_then(|s| s.split_ascii_whitespace().next())
                    .and_then(|s| s.parse::<u16>().ok());
                (matches!(status, Some(408 | 429 | 500..=599)), format!("Network: {message}"))
            }
            Err(other) => return Err(describe(other)),
        };
        illustash_terms::diagnostics::logs::update_network(stage, &detail);
        if !retryable || retries == 3 {
            return Err(tr("下载没有完成，请检查网络后再试"));
        }
        tokio::time::sleep(delay * (1 << retries)).await;
        retries += 1;
    }
}

/// POST /platform/update/cancel：停下正在进行的下载，或丢掉下好的、失败的结果。
pub fn cancel(app: &AppHandle) -> Value {
    {
        let state = app.state::<Updates>();
        let mut state = state.lock();
        if !matches!(state.stage, Stage::Installing { .. }) {
            if let Some(task) = state.task.take() {
                task.abort();
            }
            state.generation += 1;
            state.stage = Stage::Idle;
        }
    }
    status(app)
}

/// POST /platform/update/install：安装下好的版本并重启。先回答，再在别的线程里停掉本地服务、安装：页面随本地服务一起
/// 关掉，窗口换成启动页，显示正在安装。
pub fn install(app: &AppHandle) -> Result<Value, String> {
    let (version, update, bytes) = {
        let state = app.state::<Updates>();
        let mut state = state.lock();
        match std::mem::take(&mut state.stage) {
            Stage::Ready { version, update, bytes } => {
                state.stage = Stage::Installing { version: version.clone() };
                (version, update, bytes)
            }
            other => {
                state.stage = other;
                return Err(tr("还没有下好的更新"));
            }
        }
    };
    let handle = app.clone();
    thread::spawn(move || {
        // 让页面先收到回答。
        thread::sleep(Duration::from_millis(300));
        server::installing(&handle, &version);
        eprintln!("[updater] 安装 {version}");
        // Windows 上安装器启动成功后插件直接退出进程，不会回到这里。
        match update.install(&bytes) {
            Ok(()) => relaunch(&handle),
            Err(error) => {
                let error = describe(error);
                eprintln!("[updater] {version} 没有装上：{error}");
                handle.state::<Updates>().lock().stage = Stage::Failed { version, error };
                server::restart(&handle, None);
            }
        }
    });
    Ok(status(app))
}

/// 装好后重新打开应用。
///
/// macOS（#522）：Tauri 自带的重启在旧进程退出时直接运行新的可执行文件。新进程启动时会请求成为活动应用，但 macOS 14
/// 起的协作式激活只让用户启动的、或当前活动应用让出前台的进程成为活动应用：这时旧进程已经退出，前台交给了访达，新窗口
/// 虽然在最前面却不是活动窗口，第一下点击只用来激活窗口（WKWebView 不接受激活窗口的那一下点击）。所以改为在旧进程
/// 仍是活动应用时经 LaunchServices（`open -n`）启动新版本，与用户在访达里打开一样，新进程直接成为活动应用；等 `open`
/// 返回（新进程已经启动）再退出。本地服务在安装前已经停掉（`server::installing`），两个进程短暂并存不会争图库。
/// `open` 不可用或失败时退回 Tauri 的重启。
fn relaunch(app: &AppHandle) {
    #[cfg(target_os = "macos")]
    // 能走到安装说明程序在 `….app/Contents/MacOS/` 里（`packaged`），往上三层就是 .app。
    if let Some(bundle) = std::env::current_exe().ok().and_then(|exe| exe.ancestors().nth(3).map(|p| p.to_owned())) {
        let args: Vec<_> = std::env::args_os().skip(1).collect();
        let mut open = std::process::Command::new("/usr/bin/open");
        open.arg("-n").arg(&bundle);
        if !args.is_empty() {
            open.arg("--args").args(&args);
        }
        match open.status() {
            Ok(status) if status.success() => {
                app.exit(0);
                return;
            }
            Ok(status) => eprintln!("[updater] open 没能打开新版本（{status}），改用直接重启"),
            Err(error) => eprintln!("[updater] 运行 open 失败（{error}），改用直接重启"),
        }
    }
    app.request_restart();
}

/// Windows 上插件把下载的安装包写进 `%TEMP%\<产品名>-<版本>-updater-<随机>\`，运行它之后就退出，不再删除（安装包
/// 运行时也删不掉）。新版本启动时在后台清掉这些目录，每次更新大约几十 MB。安装包在最后一步打开新版本，那一刻它自己
/// 还没退出、文件还占着，所以隔一会儿再删，删不掉的过一会儿再试；还剩下的留给下次启动。
#[cfg(windows)]
pub fn clean_leftovers(app: &AppHandle) {
    let product = app.package_info().name.clone();
    thread::spawn(move || {
        for wait in [10, 30, 120] {
            thread::sleep(Duration::from_secs(wait));
            let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else { return };
            let mut left = false;
            for entry in entries.flatten() {
                if leftover(&entry.file_name().to_string_lossy(), &product) && entry.path().is_dir() {
                    left |= std::fs::remove_dir_all(entry.path()).is_err();
                }
            }
            if !left {
                return;
            }
        }
    });
}
/// `name` 是插件给这个产品建的更新临时目录（`<产品名>-<版本>-updater-<随机>`）。
#[cfg(any(windows, test))]
fn leftover(name: &str, product: &str) -> bool {
    name.strip_prefix(product)
        .and_then(|rest| rest.strip_prefix('-'))
        .and_then(|rest| rest.split_once("-updater-"))
        .is_some_and(|(version, _)| !version.is_empty() && !version.contains(['/', '\\']))
}

/// 插件的错误换成给用户看的一句话。
fn describe(error: tauri_plugin_updater::Error) -> String {
    use tauri_plugin_updater::Error as E;
    match error {
        E::Minisign(_)
        | E::Base64(_)
        | E::SignatureUtf8(_)
        | E::SignedVersionMismatch { .. }
        | E::MissingSignedVersion => tr("更新包的签名不对，没有安装"),
        E::Reqwest(_) | E::Network(_) => tr("下载没有完成，请检查网络后再试"),
        E::ReleaseNotFound | E::TargetNotFound(_) | E::TargetsNotFound(_) | E::Serialization(_) => {
            tr("这个版本没有提供本机能用的更新包")
        }
        E::InsecureTransportProtocol => tr("更新地址不是 HTTPS，没有下载"),
        E::AuthenticationFailed => tr("没有得到替换应用所需的授权"),
        E::PreviousAppNotRestored(path) => {
            tr_with("没有装上，旧版本也没能放回原处，留在了 {path}", &[("path", &path.display())])
        }
        other => tr_with("没有装上：{error}", &[("error", &other)]),
    }
}

#[cfg(test)]
mod retry_tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use tauri_plugin_updater::Error as E;

    #[test]
    fn interrupted_response_is_requested_again_with_clean_bytes() {
        let server = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let url = format!("http://{}/test", server.local_addr().unwrap());
        let peer = thread::spawn(move || {
            for payload in ["x", "good"] {
                let (mut socket, _) = server.accept().unwrap();
                // 读完请求头后关写端，模拟一个声明四字节却只发了一字节的响应。
                for line in BufReader::new(&socket).lines() {
                    if line.unwrap().is_empty() {
                        break;
                    }
                }
                write!(socket, "HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\n{payload}").unwrap();
            }
        });
        tauri::async_runtime::block_on(async {
            // 测试直接构造客户端；生产入口由更新插件在 check 时安装同一提供者。
            let _ = rustls::crypto::ring::default_provider().install_default();
            let client = reqwest::Client::builder().no_proxy().build().unwrap();
            let bytes = with_network_retry("下载应用更新", Duration::ZERO, || async {
                Ok(client.get(&url).send().await?.bytes().await?)
            })
            .await
            .unwrap();
            assert_eq!(bytes.as_ref(), b"good");
        });
        peer.join().unwrap();
    }

    #[test]
    fn retries_are_bounded_and_signed_version_failure_is_final() {
        for (status, expected) in [(503, 4), (404, 1)] {
            let mut calls = 0;
            let result =
                tauri::async_runtime::block_on(with_network_retry("下载应用更新", Duration::ZERO, || {
                    calls += 1;
                    std::future::ready(Err::<(), _>(E::Network(format!(
                        "Download request failed with status: {status}"
                    ))))
                }));
            assert_eq!(calls, expected);
            assert_eq!(result.unwrap_err(), tr("下载没有完成，请检查网络后再试"));
        }
        let mut calls = 0;
        let result = tauri::async_runtime::block_on(with_network_retry("下载应用更新", Duration::ZERO, || {
            calls += 1;
            std::future::ready(Err::<(), _>(E::MissingSignedVersion))
        }));
        assert_eq!(calls, 1);
        assert_eq!(result.unwrap_err(), tr("更新包的签名不对，没有安装"));
    }
}

#[cfg(test)]
mod leftover_tests {
    use super::leftover;

    #[test]
    fn only_this_products_updater_directories_are_cleaned() {
        assert!(leftover("Illustash-0.2.0-updater-AbC123", "Illustash"));
        assert!(leftover("Illustash PR279-0.2.0-beta.1-updater-x", "Illustash PR279"));
        for other in [
            "Illustash",
            "Illustash-0.2.0",
            "Illustash--updater-x",
            "Other-0.2.0-updater-x",
            "IllustashX-0.2.0-updater-x",
        ] {
            assert!(!leftover(other, "Illustash"), "{other}");
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn only_an_installed_bundle_updates_itself() {
        assert_eq!(packaged(Path::new("/Applications/Illustash.app/Contents/MacOS/illustash-desktop")), None);
        assert_eq!(packaged(Path::new("/Users/a/Applications/Illustash.app/Contents/MacOS/illustash-desktop")), None);
        assert_eq!(
            packaged(Path::new(
                "/private/var/folders/x/AppTranslocation/1234/d/Illustash.app/Contents/MacOS/illustash-desktop"
            )),
            Some("translocated")
        );
        for dev in ["/repo/desktop/target/debug/illustash-desktop", "/x/Contents/MacOS/illustash-desktop"] {
            assert_eq!(packaged(Path::new(dev)), Some("unpackaged"), "{dev}");
        }
    }
}
