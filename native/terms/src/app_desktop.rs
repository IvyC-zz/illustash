//! Shared authenticated desktop loopback transport. Never expose its token to the web UI.
use crate::{http_json::Json, Error, Result};
use std::{io::Read, time::Duration};
fn invalid(message: &str) -> Error {
    Error::Invalid(message.into())
}
/// Every failed bridge call starts with this text, followed by which step failed (#147). Callers that retry or tolerate
/// a bridge failure match the prefix (`unanswered`), never the whole message.
pub const UNANSWERED: &str = "桌面 app 没有响应或拒绝请求";
/// The error is a failed bridge call (any cause).
pub fn unanswered(message: &str) -> bool {
    message.starts_with(UNANSWERED)
}
pub fn available() -> bool {
    ["ILLUSTASH_DESKTOP_BRIDGE", "ILLUSTASH_DESKTOP_TOKEN"]
        .iter()
        .all(|k| std::env::var(k).is_ok_and(|v| !v.is_empty()))
}
/// Existing authenticated loopback bridge. No proxy, redirects, arbitrary hosts or methods.
pub fn request(method: &str, route: &str, timeout: u64) -> Result<Json> {
    if !available() {
        return Err(invalid("桌面平台服务不可用"));
    }
    let base = std::env::var("ILLUSTASH_DESKTOP_BRIDGE").expect("app_desktop internal invariant");
    let port = base
        .trim_end_matches('/')
        .strip_prefix("http://127.0.0.1:")
        .and_then(|v| v.parse::<u16>().ok())
        .filter(|p| *p > 0)
        .ok_or_else(|| invalid("本机桥地址不正确"))?;
    let token = std::env::var("ILLUSTASH_DESKTOP_TOKEN").expect("app_desktop internal invariant");
    send(port, &token, method, route, timeout)
}
fn send(port: u16, token: &str, method: &str, route: &str, timeout: u64) -> Result<Json> {
    if token.contains(['\r', '\n'])
        || !["GET", "POST"].contains(&method)
        || !(route.starts_with("/pixiv/") || route.starts_with("/platform/choose?"))
    {
        return Err(invalid("本机桥参数不正确"));
    }
    let agent = ureq::AgentBuilder::new().redirects(0).timeout(Duration::from_secs(timeout)).build();
    let result =
        agent.request(method, &format!("http://127.0.0.1:{port}{route}")).set("X-Illustash-Token", token).call();
    let response = result.map_err(|e| invalid(&format!("{UNANSWERED}：{}", failure(e, timeout))))?;
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(5 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| invalid("账号服务响应读取失败"))?;
    if bytes.len() > 5 * 1024 * 1024 {
        return Err(invalid("账号服务响应超过限额"));
    }
    let text = std::str::from_utf8(&bytes).map_err(|_| invalid("账号服务响应编码不正确"))?;
    crate::http_json::parse_python(text).map_err(|_| invalid("账号服务响应不正确"))
}
/// Which step of the bridge call failed. The desktop answers a refusal or a failed action with `{"error": …}`
/// (desktop/src/bridge.rs): short local text, never a token, cookie or selected path.
fn failure(error: ureq::Error, timeout: u64) -> String {
    match error {
        ureq::Error::Status(code, response) => {
            let mut body = String::new();
            let _ = response.into_reader().take(4096).read_to_string(&mut body);
            let said = crate::http_json::parse_python(&body)
                .ok()
                .and_then(|v| v.get("error").map(|e| e.text().trim().to_owned()))
                .filter(|s| !s.is_empty())
                .map(|s| s.chars().take(200).collect::<String>());
            match said {
                Some(said) => format!("桌面 app 回复失败（{code}）：{said}"),
                None => format!("桌面 app 回复失败（{code}）"),
            }
        }
        ureq::Error::Transport(transport) => {
            let io = std::error::Error::source(&transport).and_then(|e| e.downcast_ref::<std::io::Error>());
            if io.is_some_and(|e| matches!(e.kind(), std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock)) {
                format!("等待超过 {timeout} 秒")
            } else if transport.kind() == ureq::ErrorKind::ConnectionFailed {
                "连不上桌面 app 的本机通道".into()
            } else {
                format!("本机通道连接中断（{kind}）", kind = transport.kind())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn message(result: Result<Json>) -> String {
        match result {
            Err(Error::Invalid(m)) => m,
            other => panic!("expected a bridge failure, got {other:?}"),
        }
    }

    /// One local stand-in for the desktop bridge that answers a single request with `status` and `body`.
    fn bridge(status: u16, body: &'static str) -> u16 {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let port = server.server_addr().to_ip().unwrap().port();
        std::thread::spawn(move || {
            if let Ok(request) = server.recv() {
                let _ = request.respond(tiny_http::Response::from_string(body).with_status_code(status));
            }
        });
        port
    }

    #[test]
    fn bridge_failures_say_which_step_failed() {
        // The desktop's own refusal text comes through (a failed picker, a wrong token).
        let port = bridge(502, r#"{"error": "选择结果不是本地路径"}"#);
        let m = message(send(port, "t", "POST", "/platform/choose?kind=folder&multiple=1", 5));
        assert_eq!(m, "桌面 app 没有响应或拒绝请求：桌面 app 回复失败（502）：选择结果不是本地路径");
        let port = bridge(403, r#"{"error": "口令不对"}"#);
        assert!(message(send(port, "t", "GET", "/pixiv/session", 5)).ends_with("（403）：口令不对"));
        let port = bridge(500, "not json");
        assert!(message(send(port, "t", "GET", "/pixiv/session", 5)).ends_with("桌面 app 回复失败（500）"));
        // Nothing listening on the port.
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let m = message(send(port, "t", "GET", "/pixiv/session", 5));
        assert_eq!(m, "桌面 app 没有响应或拒绝请求：连不上桌面 app 的本机通道");
        // Accepted but never answered.
        let silent = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = silent.local_addr().unwrap().port();
        let m = message(send(port, "t", "GET", "/pixiv/session", 1));
        assert_eq!(m, "桌面 app 没有响应或拒绝请求：等待超过 1 秒");
        drop(silent);
        assert!(unanswered(&m) && !unanswered("Pixiv 请求失败"));
    }
}
