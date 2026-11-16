//! 给原生账号模块用的本机登录通道（只听 127.0.0.1，每个请求要带启动时生成的口令）：
//!   GET  /pixiv/session            登录窗口里有没有 Pixiv 登录、用户 ID
//!   GET  /pixiv/fetch?path=/ajax/… 在登录窗口的 www.pixiv.net 页面里发一个 GET，返回 {status, body, cfMitigated}
//!   POST /pixiv/login              打开登录窗口
//!   POST /pixiv/logout             清掉登录窗口的数据（解绑）
//!   GET  /platform/update/status   应用内更新的状态（#279，updater.rs）
//!   POST /platform/update/download?version=X.Y.Z 下载并校验这个版本；cancel 取消；install 安装并重启
//!   POST /platform/diagnostics/open-logs 打开应用自己的日志目录（无路径参数）
//!   POST /platform/files/open   {path}         用默认应用打开一张图（#450，只放行图片扩展名）
//!   POST /platform/files/reveal {path}         在访达或资源管理器里显示一张图
//!   POST /platform/files/folder {path}         打开导出到的文件夹
//!   POST /platform/files/drag   {paths, icon}  把几张图拖到别的应用，只允许复制
//! 文件路由的路径都由宿主按资产 ID 从图库里查出来（native/terms/src/app_asset_files.rs）；这里只收存在的普通文件，
//! 不记录路径。
use std::{io::Read, path::PathBuf, thread};

use serde_json::{json, Value};
use tauri::{AppHandle, Manager};
use tiny_http::{Header, Method, Request, Response, Server};

use crate::{drag, picker, pixiv, updater};
use illustash_platform::platform_files;

pub const TOKEN_HEADER: &str = "X-Illustash-Token";

pub fn token() -> String {
    let mut raw = [0u8; 24];
    getrandom::fill(&mut raw).expect("系统随机数不可用");
    raw.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn start(app: AppHandle, token: String) -> Result<u16, Box<dyn std::error::Error>> {
    let server = Server::http("127.0.0.1:0").map_err(|e| e.to_string())?;
    let port = server.server_addr().to_ip().map(|a| a.port()).ok_or_else(|| crate::i18n::tr("通道没有端口"))?;
    thread::spawn(move || {
        for request in server.incoming_requests() {
            let (app, token) = (app.clone(), token.clone());
            thread::spawn(move || handle(&app, &token, request));
        }
    });
    Ok(port)
}

fn query(url: &str, key: &str) -> Option<String> {
    let url = tauri::Url::parse(&format!("http://127.0.0.1{url}")).ok()?;
    let found = url.query_pairs().find(|(k, _)| k == key).map(|(_, v)| v.into_owned());
    found
}

fn handle(app: &AppHandle, token: &str, mut request: Request) {
    let authorized = request.headers().iter().any(|h| h.field.equiv(TOKEN_HEADER) && h.value.as_str() == token);
    let route = request.url().split('?').next().unwrap_or("").to_string();
    let (code, body): (u16, Value) = if !authorized {
        (403, json!({"error": "口令不对"}))
    } else {
        let method = request.method().clone();
        let result = match (&method, route.as_str()) {
            (Method::Post, "/platform/choose") => picker::choose(
                app,
                query(request.url(), "kind").as_deref().unwrap_or(""),
                query(request.url(), "multiple").as_deref() == Some("1"),
            ),
            (Method::Get, "/pixiv/session") => Ok(pixiv::session(app)),
            (Method::Get, "/pixiv/fetch") => match query(request.url(), "path") {
                Some(path) => pixiv::fetch(app, &path),
                None => Err("缺少 path".to_string()),
            },
            (Method::Post, "/pixiv/login") => {
                pixiv::login(app, query(request.url(), "force").as_deref() == Some("1")).map(|_| json!({"ok": true}))
            }
            (Method::Post, "/pixiv/logout") => pixiv::logout(app).map(|_| json!({"ok": true})),
            (Method::Post, "/platform/diagnostics/open-logs") if request.url() == route => {
                open_logs().map(|()| json!({"ok": true})).map_err(|_| "无法打开日志所在文件夹".into())
            }
            (Method::Post, route) if route.starts_with("/platform/files/") && request.url() == route => {
                files(app, route, &mut request).map(|()| json!({"ok": true}))
            }
            (Method::Get, "/platform/update/status") => Ok(updater::status(app)),
            (Method::Post, "/platform/update/download") => match query(request.url(), "version") {
                Some(version) => updater::download(app, &version),
                None => Err("缺少 version".to_string()),
            },
            (Method::Post, "/platform/update/cancel") => Ok(updater::cancel(app)),
            (Method::Post, "/platform/update/install") => updater::install(app),
            _ => Err("未知路径".to_string()),
        };
        // 给终端 / 控制台看的诊断信息：哪个调用，Pixiv 或窗口怎样回应（从不输出正文或 cookie）。
        match &result {
            Ok(value) if route == "/pixiv/fetch" => {
                if value["status"].as_u64() != Some(200) {
                    eprintln!("[bridge] fetch → HTTP {} {}", value["status"], value["error"].as_str().unwrap_or(""));
                }
            }
            Ok(_) if route == "/platform/choose" => eprintln!("[bridge] 选择窗口完成"),
            // 下载时页面每秒问一次状态。
            Ok(_) if route == "/platform/update/status" => {}
            Ok(value) => eprintln!("[bridge] {route} → {value}"),
            Err(error) => eprintln!("[bridge] {route} 失败：{error}"),
        }
        match result {
            Ok(value) => (200, value),
            Err(error) => (502, json!({"error": error})),
        }
    };
    let header = Header::from_bytes("Content-Type", "application/json; charset=utf-8").expect("constant header");
    let _ = request.respond(Response::from_string(body.to_string()).with_status_code(code).with_header(header));
}

/// 一次最多拖出几张：与宿主的 `app_asset_files::DRAG_LIMIT` 相同。
const DRAG_LIMIT: usize = 100;

/// 用图动作的文件路由。正文是宿主发来的 JSON；路径都要是存在的普通文件。
fn files(app: &AppHandle, route: &str, request: &mut Request) -> Result<(), String> {
    let mut text = String::new();
    request.as_reader().take(1 << 20).read_to_string(&mut text).map_err(|_| "正文不正确".to_string())?;
    let body: Value = serde_json::from_str(&text).map_err(|_| "正文不正确".to_string())?;
    let path = |value: &Value| -> Result<PathBuf, String> {
        let path = PathBuf::from(value.as_str().unwrap_or_default());
        if platform_files::regular_file(&path) {
            Ok(path)
        } else {
            Err(crate::i18n::tr("文件不在了"))
        }
    };
    match route {
        "/platform/files/open" => {
            platform_files::open(&path(&body["path"])?).map_err(|_| crate::i18n::tr("无法打开这个文件"))
        }
        "/platform/files/reveal" => {
            platform_files::reveal(&path(&body["path"])?).map_err(|_| crate::i18n::tr("无法显示这个文件"))
        }
        "/platform/files/folder" => {
            platform_files::show_folder(&PathBuf::from(body["path"].as_str().unwrap_or_default()))
                .map_err(|_| crate::i18n::tr("无法打开这个文件夹"))
        }
        "/platform/files/drag" => {
            let paths = body["paths"].as_array().map(|list| list.iter().map(path).collect::<Result<Vec<_>, _>>());
            let paths = paths.unwrap_or_else(|| Err("缺少 paths".into()))?;
            if paths.is_empty() || paths.len() > DRAG_LIMIT {
                return Err("拖出的张数不正确".into());
            }
            let icon = body["icon"].as_str().map(PathBuf::from).filter(|p| platform_files::regular_file(p));
            let main = app.get_webview_window("main").ok_or_else(|| "没有主窗口".to_string())?;
            drag::start(&main, paths, icon)
        }
        _ => Err("未知路径".into()),
    }
}

fn open_logs() -> std::io::Result<()> {
    let directory = std::path::absolute(illustash_terms::diagnostics::logs::directory()?)?;
    #[cfg(target_os = "macos")]
    {
        let status = std::process::Command::new("/usr/bin/open").arg(directory).status()?;
        if status.success() {
            Ok(())
        } else {
            Err(std::io::Error::other("open failed"))
        }
    }
    #[cfg(windows)]
    {
        use std::{ffi::c_void, os::windows::ffi::OsStrExt, ptr::null};
        #[link(name = "ole32")]
        extern "system" {
            fn CoInitializeEx(reserved: *const c_void, flags: u32) -> i32;
            fn CoUninitialize();
        }
        #[link(name = "shell32")]
        extern "system" {
            fn ShellExecuteW(
                window: *const c_void,
                verb: *const u16,
                file: *const u16,
                parameters: *const u16,
                directory: *const u16,
                show: i32,
            ) -> isize;
        }
        let path: Vec<_> = directory.as_os_str().encode_wide().chain(Some(0)).collect();
        let open: Vec<_> = "open".encode_utf16().chain(Some(0)).collect();
        // SAFETY: 桥接在独立线程处理请求，首个参数按约定为空。外壳扩展可能需要 STA。
        let init = unsafe { CoInitializeEx(null(), 0x2 | 0x4) }; // COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE
        if init < 0 {
            return Err(std::io::Error::other("CoInitializeEx failed"));
        }
        // SAFETY: path 和 open 在调用期间有效且以 NUL 结尾。CoUninitialize 与上面的成功调用配对。
        let status = unsafe {
            let status = ShellExecuteW(null(), open.as_ptr(), path.as_ptr(), null(), null(), 1);
            CoUninitialize();
            status
        };
        if status > 32 {
            Ok(())
        } else {
            Err(std::io::Error::other("ShellExecuteW failed"))
        }
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = directory;
        Err(std::io::ErrorKind::Unsupported.into())
    }
}
