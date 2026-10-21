//! Entering the native host as a browser does: its ready line prints the address with a one-time code, and the
//! first visit trades it for the session cookie every later request needs.
#![allow(dead_code)] // each test binary uses its own part
use std::{collections::BTreeMap, sync::Mutex};

pub const READY: &str = "图库已启动：";
static COOKIES: Mutex<BTreeMap<String, String>> = Mutex::new(BTreeMap::new());

/// The address in a ready line, if it is one.
pub fn address(line: &str) -> Option<&str> {
    line.strip_prefix(READY).map(str::trim)
}

/// Visits the ready line's address and keeps the cookie for its origin; returns the `Cookie` header value.
pub fn enter(line: &str) -> String {
    let address = address(line).expect("a ready line");
    let origin = address.split("/?").next().unwrap().to_owned();
    let response = ureq::get(address).call().expect("entering the host");
    let cookie = response.header("Set-Cookie").expect("the session cookie").split(';').next().unwrap().to_owned();
    COOKIES.lock().unwrap().insert(origin, cookie.clone());
    cookie
}

/// The `Cookie` header value for requests to `origin` (empty before entering).
pub fn cookie(origin: &str) -> String {
    COOKIES.lock().unwrap().get(origin).cloned().unwrap_or_default()
}
