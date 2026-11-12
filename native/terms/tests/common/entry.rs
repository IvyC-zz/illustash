//! 像浏览器那样进入原生宿主：它的就绪行打印带一次性码的地址，第一次访问时用这个码换取会话 cookie，
//! 之后的每个请求都要带上它。
#![allow(dead_code)] // 每个测试二进制只用到其中一部分
use std::{collections::BTreeMap, sync::Mutex};

pub const READY: &str = "图库已启动：";
static COOKIES: Mutex<BTreeMap<String, String>> = Mutex::new(BTreeMap::new());

/// 就绪行里的地址；不是就绪行时返回 None。
pub fn address(line: &str) -> Option<&str> {
    line.strip_prefix(READY).map(str::trim)
}

/// 访问就绪行里的地址，按 origin 保存 cookie；返回 `Cookie` 请求头的值。
pub fn enter(line: &str) -> String {
    let address = address(line).expect("a ready line");
    let origin = address.split("/?").next().unwrap().to_owned();
    let response = ureq::get(address).call().expect("entering the host");
    let cookie = response.header("Set-Cookie").expect("the session cookie").split(';').next().unwrap().to_owned();
    COOKIES.lock().unwrap().insert(origin, cookie.clone());
    cookie
}

/// 发往 `origin` 的请求所用的 `Cookie` 请求头的值（进入之前为空）。
pub fn cookie(origin: &str) -> String {
    COOKIES.lock().unwrap().get(origin).cloned().unwrap_or_default()
}
