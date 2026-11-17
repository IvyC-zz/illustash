//! 共用的、带认证的桌面本机回环通道。不要把它的 token 暴露给网页界面。
use crate::{http_json::Json, Error, Result};
use std::{io::Read, time::Duration};
/// 每次桥接调用失败都以这段文字开头，后面跟着是哪一步失败（#147）。要重试或容忍桥接失败的调用方匹配这个前缀
/// （`unanswered`），从不匹配整条消息。
pub const UNANSWERED: &str = "桌面 app 没有响应或拒绝请求";
/// 这个错误是一次失败的桥接调用（任何原因）。
pub fn unanswered(message: &str) -> bool {
    message.starts_with(UNANSWERED)
}
pub fn available() -> bool {
    ["ILLUSTASH_DESKTOP_BRIDGE", "ILLUSTASH_DESKTOP_TOKEN"]
        .iter()
        .all(|k| std::env::var(k).is_ok_and(|v| !v.is_empty()))
}
/// 现有的带认证本机回环桥接。不走代理，不跟随重定向，不允许任意主机或方法。
pub fn request(method: &str, route: &str, timeout: u64) -> Result<Json> {
    if !available() {
        return Err(Error::invalid("桌面平台服务不可用"));
    }
    let (port, token) = endpoint()?;
    send(port, &token, method, route, timeout, None)
}
/// 带 JSON 正文的 POST：宿主主动告诉外壳的事（任务进行状态，#451）和用图动作（#450）。
pub fn post(route: &str, body: &str, timeout: u64) -> Result<Json> {
    if !available() {
        return Err(Error::invalid("桌面平台服务不可用"));
    }
    let (port, token) = endpoint()?;
    send(port, &token, "POST", route, timeout, Some(body))
}
/// 用图动作的文件路由（#450）：POST，正文是 JSON。路径只放在正文里，不进地址，免得出现在日志和诊断里。
pub(crate) fn post_files(route: &str, body: &serde_json::Value, timeout: u64) -> Result<Json> {
    post(route, &body.to_string(), timeout)
}
/// 本机桥上用图动作的路由（desktop/src/bridge.rs）。只收宿主按资产 ID 查出来的路径。
pub(crate) const FILE_ROUTES: [&str; 4] =
    ["/platform/files/open", "/platform/files/reveal", "/platform/files/drag", "/platform/files/folder"];
fn endpoint() -> Result<(u16, String)> {
    let base = std::env::var("ILLUSTASH_DESKTOP_BRIDGE").expect("app_desktop internal invariant");
    let port = base
        .trim_end_matches('/')
        .strip_prefix("http://127.0.0.1:")
        .and_then(|v| v.parse::<u16>().ok())
        .filter(|p| *p > 0)
        .ok_or_else(|| Error::invalid("本机桥地址不正确"))?;
    Ok((port, std::env::var("ILLUSTASH_DESKTOP_TOKEN").expect("app_desktop internal invariant")))
}
fn send(port: u16, token: &str, method: &str, route: &str, timeout: u64, body: Option<&str>) -> Result<Json> {
    // 带正文的只有任务进行状态（#451）和用图动作的文件路由（#450）；它们也只能带正文、用 POST 发。
    let activity = method == "POST" && (route == crate::app_activity::ROUTE || FILE_ROUTES.contains(&route));
    let allowed = route.starts_with("/pixiv/")
        || route.starts_with("/platform/choose?")
        || (method == "POST" && route == "/platform/diagnostics/open-logs")
        || route.starts_with("/platform/update/");
    if token.contains(['\r', '\n'])
        || !["GET", "POST"].contains(&method)
        || !(if body.is_some() { activity } else { allowed })
    {
        return Err(Error::invalid("本机桥参数不正确"));
    }
    let agent = ureq::AgentBuilder::new().redirects(0).timeout(Duration::from_secs(timeout)).build();
    let call = agent.request(method, &format!("http://127.0.0.1:{port}{route}")).set("X-Illustash-Token", token);
    let result = match body {
        Some(body) => call.set("Content-Type", "application/json").send_string(body),
        None => call.call(),
    };
    let response = result.map_err(|e| Error::invalid(format!("{UNANSWERED}：{}", failure(e, timeout))))?;
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(5 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::invalid("账号服务响应读取失败"))?;
    if bytes.len() > 5 * 1024 * 1024 {
        return Err(Error::invalid("账号服务响应超过限额"));
    }
    let text = std::str::from_utf8(&bytes).map_err(|_| Error::invalid("账号服务响应编码不正确"))?;
    crate::http_json::parse_python(text).map_err(|_| Error::invalid("账号服务响应不正确"))
}
/// 桥接调用是哪一步失败的。桌面端对拒绝或失败的操作回复 `{"error": …}`（desktop/src/bridge.rs）：简短的本机文字，
/// 从不含 token、cookie 或所选路径。
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

    /// 一个本地替身，代替桌面桥接，用 `status` 和 `body` 回答一次请求。
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
        // 桌面端自己的拒绝文字会原样传过来（选择器失败、token 不对）。
        let port = bridge(502, r#"{"error": "选择结果不是本地路径"}"#);
        let m = message(send(port, "t", "POST", "/platform/choose?kind=folder&multiple=1", 5, None));
        assert_eq!(m, "桌面 app 没有响应或拒绝请求：桌面 app 回复失败（502）：选择结果不是本地路径");
        let port = bridge(403, r#"{"error": "口令不对"}"#);
        assert!(message(send(port, "t", "GET", "/pixiv/session", 5, None)).ends_with("（403）：口令不对"));
        let port = bridge(500, "not json");
        assert!(message(send(port, "t", "GET", "/pixiv/session", 5, None)).ends_with("桌面 app 回复失败（500）"));
        // 端口上没有监听。
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let m = message(send(port, "t", "GET", "/pixiv/session", 5, None));
        assert_eq!(m, "桌面 app 没有响应或拒绝请求：连不上桌面 app 的本机通道");
        // 接受了连接但一直不回答。
        let silent = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = silent.local_addr().unwrap().port();
        let m = message(send(port, "t", "GET", "/pixiv/session", 1, None));
        assert_eq!(m, "桌面 app 没有响应或拒绝请求：等待超过 1 秒");
        drop(silent);
        assert!(unanswered(&m) && !unanswered("Pixiv 请求失败"));
    }

    #[test]
    fn activity_route_only_goes_out_as_a_post_with_a_body() {
        let route = crate::app_activity::ROUTE;
        let port = bridge(200, r#"{"ok":true}"#);
        assert!(send(port, "t", "POST", route, 1, Some(r#"{"busy":[]}"#)).is_ok());
        assert_eq!(message(send(0, "t", "POST", route, 1, None)), "本机桥参数不正确");
        assert_eq!(message(send(0, "t", "GET", route, 1, Some("{}"))), "本机桥参数不正确");
        assert_eq!(message(send(0, "t", "POST", "/pixiv/login", 1, Some("{}"))), "本机桥参数不正确");
    }

    #[test]
    fn logs_folder_bridge_only_accepts_the_fixed_post_route() {
        let route = "/platform/diagnostics/open-logs";
        let port = bridge(200, r#"{"ok":true}"#);
        let reply = send(port, "t", "POST", route, 1, None).unwrap();
        assert!(matches!(reply.get("ok"), Some(Json::Scalar(serde_json::Value::Bool(true)))));
        for (method, route) in [("GET", route), ("POST", "/platform/diagnostics/open-logs?path=synthetic")] {
            assert_eq!(message(send(0, "t", method, route, 1, None)), "本机桥参数不正确");
        }
    }

    #[test]
    fn file_routes_only_take_a_post_body() {
        let port = bridge(200, r#"{"ok":true}"#);
        assert!(send(port, "t", "POST", "/platform/files/reveal", 1, Some(r#"{"path":"/x.jpg"}"#)).is_ok());
        for (method, route, body) in [
            ("POST", "/platform/files/reveal", None),
            ("GET", "/platform/files/open", Some("{}")),
            ("POST", "/platform/files/open?path=/x.jpg", Some("{}")),
            ("POST", "/platform/files/run", Some("{}")),
            ("POST", "/pixiv/login", Some("{}")),
        ] {
            assert_eq!(message(send(0, "t", method, route, 1, body)), "本机桥参数不正确");
        }
    }
}
