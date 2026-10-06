//! Shared authenticated desktop loopback transport. Never expose its token to the web UI.
use crate::{http_json::Json, Error, Result};
use std::{io::Read, time::Duration};
fn invalid(message: &str) -> Error {
    Error::Invalid(message.into())
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
    if token.contains(['\r', '\n'])
        || !["GET", "POST"].contains(&method)
        || !(route.starts_with("/pixiv/") || route.starts_with("/platform/choose?"))
    {
        return Err(invalid("本机桥参数不正确"));
    }
    let agent = ureq::AgentBuilder::new().redirects(0).timeout(Duration::from_secs(timeout)).build();
    let result =
        agent.request(method, &format!("http://127.0.0.1:{port}{route}")).set("X-Illustash-Token", &token).call();
    let response = result.map_err(|_| invalid("桌面 app 没有响应或拒绝请求"))?;
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
