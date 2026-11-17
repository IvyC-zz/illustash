//! 给原生账号模块用的本机登录通道（只听 127.0.0.1，每个请求要带启动时生成的口令）：
//!   GET  /pixiv/session            登录窗口里有没有 Pixiv 登录、用户 ID
//!   GET  /pixiv/fetch?path=/ajax/… 在登录窗口的 www.pixiv.net 页面里发一个 GET，返回 {status, body, cfMitigated}
//!   POST /pixiv/login              打开登录窗口
//!   POST /pixiv/logout             清掉登录窗口的数据（解绑）
//!   GET  /platform/update/status   应用内更新的状态（#279，updater.rs）
//!   POST /platform/update/download?version=X.Y.Z 下载并校验这个版本；cancel 取消；install 安装并重启
//!   POST /platform/diagnostics/open-logs 打开应用自己的日志目录（无路径参数）
//!   POST /platform/activity        宿主报来的后台任务进行状态（#451，background.rs），正文是 JSON
use std::io::Read;
use std::thread;

use serde_json::{json, Value};
use tauri::AppHandle;
use tiny_http::{Header, Method, Request, Response, Server};

use crate::{background, picker, pixiv, updater};

pub const TOKEN_HEADER: &str = "X-Illustash-Token";
/// 任务状态只有几个任务的种类和数字，用不了这么多。
const ACTIVITY_LIMIT: u64 = 64 * 1024;

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
            (Method::Get, "/platform/update/status") => Ok(updater::status(app)),
            (Method::Post, "/platform/update/download") => match query(request.url(), "version") {
                Some(version) => updater::download(app, &version),
                None => Err("缺少 version".to_string()),
            },
            (Method::Post, "/platform/update/cancel") => Ok(updater::cancel(app)),
            (Method::Post, "/platform/update/install") => updater::install(app),
            (Method::Post, "/platform/activity") if request.url() == route => {
                let mut body = String::new();
                match request.as_reader().take(ACTIVITY_LIMIT + 1).read_to_string(&mut body) {
                    Ok(n) if n as u64 <= ACTIVITY_LIMIT => background::activity(app, &body),
                    _ => Err("任务状态读不了或太长".to_string()),
                }
            }
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
            // 下载时页面每秒问一次状态；任务进度一变就报一次。
            Ok(_) if route == "/platform/update/status" || route == "/platform/activity" => {}
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
