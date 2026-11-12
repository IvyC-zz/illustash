//! 给原生账号模块用的本机登录通道（只听 127.0.0.1，每个请求要带启动时生成的口令）：
//!   GET  /pixiv/session            登录窗口里有没有 Pixiv 登录、用户 ID
//!   GET  /pixiv/fetch?path=/ajax/… 在登录窗口的 www.pixiv.net 页面里发一个 GET，返回 {status, body, cfMitigated}
//!   POST /pixiv/login              打开登录窗口
//!   POST /pixiv/logout             清掉登录窗口的数据（解绑）
use std::thread;

use serde_json::{json, Value};
use tauri::AppHandle;
use tiny_http::{Header, Method, Request, Response, Server};

use crate::{pixiv, picker};

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

fn handle(app: &AppHandle, token: &str, request: Request) {
    let authorized = request
        .headers()
        .iter()
        .any(|h| h.field.equiv(TOKEN_HEADER) && h.value.as_str() == token);
    let route = request.url().split('?').next().unwrap_or("").to_string();
    let (code, body): (u16, Value) = if !authorized {
        (403, json!({"error": "口令不对"}))
    } else {
        let result = match (request.method(), route.as_str()) {
            (Method::Post, "/platform/choose") => picker::choose(app, query(request.url(), "kind").as_deref().unwrap_or(""), query(request.url(), "multiple").as_deref() == Some("1")),
            (Method::Get, "/pixiv/session") => Ok(pixiv::session(app)),
            (Method::Get, "/pixiv/fetch") => match query(request.url(), "path") {
                Some(path) => pixiv::fetch(app, &path),
                None => Err("缺少 path".to_string()),
            },
            (Method::Post, "/pixiv/login") => pixiv::login(app, query(request.url(), "force").as_deref() == Some("1")).map(|_| json!({"ok": true})),
            (Method::Post, "/pixiv/logout") => pixiv::logout(app).map(|_| json!({"ok": true})),
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
