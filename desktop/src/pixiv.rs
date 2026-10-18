//! Pixiv 登录窗口：用户在这里亲手登录（有人机验证就自己点过）；之后原生账号模块要的 www.pixiv.net/ajax 只读请求，
//! 在这个窗口的 www.pixiv.net 页面里用 fetch 发出，状态码和正文原样交回。不改请求头、不伪装、不处理验证页。
//! 窗口用单独的网站数据存储（macOS 14+），解绑时只清它，不动主窗口的数据。
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tauri::webview::{NewWindowResponse, PageLoadEvent};
use tauri::{AppHandle, Manager, Url, WebviewUrl, WebviewWindow, WebviewWindowBuilder, WindowEvent};

const LABEL: &str = "pixiv";
const HOME: &str = "https://www.pixiv.net/";
const LOGIN: &str = "https://accounts.pixiv.net/login?return_to=https%3A%2F%2Fwww.pixiv.net%2F";
const HOST: &str = "www.pixiv.net";
#[cfg(target_os = "macos")]
const STORE: [u8; 16] = *b"illustash-pixiv\0";
const PAGE_TIMEOUT: Duration = Duration::from_secs(45);
const REPLY_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Default)]
pub struct Pixiv {
    pending: Mutex<HashMap<String, SyncSender<Value>>>,
    page: Mutex<Page>,
    loaded: Condvar,
    busy: Mutex<()>, // one request at a time; pacing belongs to the native caller
    login_open: AtomicBool,
}

/// What the Pixiv window shows: finished loads so far, and the host of the page that finished last (None while a
/// navigation is under way). Requests only go out from a finished www.pixiv.net page.
#[derive(Default)]
struct Page {
    loads: u64,
    host: Option<String>,
    warm: bool, // a pixiv.net page has finished loading in this launch
}

/// Called by the page in the Pixiv window with the result of one request (the only command it may call).
#[tauri::command]
pub fn pixiv_reply(webview: tauri::Webview, state: tauri::State<Pixiv>, id: String, reply: Value) {
    if webview.label() != LABEL {
        return;
    }
    if let Some(tx) = state.pending.lock().unwrap().remove(&id) {
        let _ = tx.try_send(reply);
    }
}

fn window(app: &AppHandle) -> Result<WebviewWindow, String> {
    if let Some(win) = app.get_webview_window(LABEL) {
        return Ok(win);
    }
    let blank = WebviewUrl::External(Url::parse("about:blank").unwrap());
    let builder = WebviewWindowBuilder::new(app, LABEL, blank)
        .title(crate::i18n::tr("登录 Pixiv"))
        .inner_size(1000.0, 780.0)
        .visible(false)
        .on_new_window(|_, _| NewWindowResponse::Allow) // 登录页可能开第三方登录的弹窗
        .on_page_load(|win, payload| {
            let state = win.state::<Pixiv>();
            let mut page = state.page.lock().unwrap();
            if payload.event() == PageLoadEvent::Started {
                page.host = None;
                return;
            }
            page.loads += 1;
            page.host = payload.url().host_str().map(str::to_string);
            page.warm |= page.host.as_deref().is_some_and(|h| h == "pixiv.net" || h.ends_with(".pixiv.net"));
            eprintln!("[pixiv] 页面加载完成：{}", payload.url().host_str().unwrap_or(payload.url().scheme()));
            state.loaded.notify_all();
            // A cookie can outlive its server-side session. Keep the window visible until
            // the person closes it; page load alone cannot prove a successful login.
        });
    #[cfg(target_os = "macos")]
    let builder = builder.data_store_identifier(STORE);
    let win = builder.build().map_err(|e| crate::i18n::tr_with("打不开 Pixiv 窗口：{error}", &[("error", &e)]))?;
    let hide = win.clone();
    win.on_window_event(move |event| {
        if let WindowEvent::CloseRequested { api, .. } = event {
            api.prevent_close(); // 窗口里的页面还要用来发请求，关掉只是收起
            hide.state::<Pixiv>().login_open.store(false, Ordering::SeqCst);
            let _ = hide.hide();
        }
    });
    Ok(win)
}

fn on_pixiv(win: &WebviewWindow) -> bool {
    win.state::<Pixiv>().page.lock().unwrap().host.as_deref() == Some(HOST)
}

/// Load `url` and wait until a page on its host has finished loading. A load finishing elsewhere (the window's first
/// about:blank, say) does not count: a request sent from it would go out before the Pixiv page, and its cookies, are there.
fn open(win: &WebviewWindow, url: &str) -> Result<(), String> {
    let target = Url::parse(url).unwrap();
    let state = win.state::<Pixiv>();
    let before = state.page.lock().unwrap().loads;
    win.navigate(target.clone()).map_err(|e| e.to_string())?;
    let deadline = Instant::now() + PAGE_TIMEOUT;
    let mut page = state.page.lock().unwrap();
    while page.loads == before || page.host.as_deref() != target.host_str() {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(crate::i18n::tr("Pixiv 页面加载超时"));
        }
        page = state.loaded.wait_timeout(page, left).unwrap().0;
    }
    Ok(())
}

/// The window's data store hands out its saved cookies only after a pixiv.net page has loaded in this launch: before
/// that it reads as logged out (2026-09-28: a valid login was judged expired right after start). Load the home page once
/// first, unless the person is using the window to log in. The caller holds `busy`.
fn warm(win: &WebviewWindow) {
    let state = win.state::<Pixiv>();
    if !state.page.lock().unwrap().warm && !state.login_open.load(Ordering::SeqCst) {
        if let Err(error) = open(win, HOME) {
            eprintln!("[pixiv] 预热失败：{error}");
        }
    }
}

/// The logged-in Pixiv user: PHPSESSID looks like `<user id>_<random>`. Only the id leaves this function.
pub fn user_id(app: &AppHandle) -> Option<String> {
    let win = window(app).ok()?;
    {
        let state = app.state::<Pixiv>();
        let _one = state.busy.lock().unwrap();
        warm(&win);
    }
    // cookies_for_url only matches the exact host, and PHPSESSID lives on .pixiv.net; this store holds nothing but Pixiv.
    let cookies = win.cookies().ok()?;
    let sid = cookies
        .iter()
        .find(|c| c.name() == "PHPSESSID" && c.domain().is_some_and(|d| d.trim_start_matches('.') == "pixiv.net"))?
        .value()
        .to_string();
    let uid = sid.split('_').next()?;
    (sid.contains('_') && !uid.is_empty() && uid.bytes().all(|b| b.is_ascii_digit())).then(|| uid.to_string())
}

pub fn session(app: &AppHandle) -> Value {
    let uid = user_id(app);
    json!({"loggedIn": uid.is_some(), "userId": uid,
           "loginOpen": app.state::<Pixiv>().login_open.load(Ordering::SeqCst)})
}

/// One read-only GET of `path` (must start with /ajax/) from the www.pixiv.net page: {status, body, cfMitigated}.
pub fn fetch(app: &AppHandle, path: &str) -> Result<Value, String> {
    if !path.starts_with("/ajax/") || path.contains("..") || path.contains("//") {
        return Err("只允许 www.pixiv.net/ajax/ 下的路径".into());
    }
    let state = app.state::<Pixiv>();
    let _one = state.busy.lock().unwrap();
    let win = window(app)?;
    if !on_pixiv(&win) {
        if state.login_open.load(Ordering::SeqCst) {
            return Err(crate::i18n::tr("用户正在 Pixiv 窗口里登录")); // 不把用户正在用的页面换走
        }
        open(&win, HOME)?;
    }
    let mut raw = [0u8; 12];
    getrandom::fill(&mut raw).map_err(|e| e.to_string())?;
    let id: String = raw.iter().map(|b| format!("{b:02x}")).collect();
    let (tx, rx) = sync_channel(1);
    state.pending.lock().unwrap().insert(id.clone(), tx);
    let js = format!(
        "(async () => {{ let r; try {{ const res = await fetch({path}, {{credentials: 'same-origin', headers: {{Accept: 'application/json'}}}}); \
         r = {{status: res.status, cfMitigated: res.headers.get('cf-mitigated'), body: await res.text()}}; }} \
         catch (e) {{ r = {{status: 0, error: String(e)}}; }} \
         window.__TAURI_INTERNALS__.invoke('pixiv_reply', {{id: {id}, reply: r}}); }})();",
        path = serde_json::to_string(path).unwrap(),
        id = serde_json::to_string(&id).unwrap()
    );
    let sent = win.eval(&js).map_err(|e| e.to_string());
    let reply = sent.and_then(|_| rx.recv_timeout(REPLY_TIMEOUT).map_err(|_| crate::i18n::tr("Pixiv 窗口没有返回结果")));
    state.pending.lock().unwrap().remove(&id);
    reply
}

/// Show the window for the user to log in (or, already logged in, to look at their Pixiv).
pub fn login(app: &AppHandle, force: bool) -> Result<(), String> {
    if force {
        logout(app)?; // Remove the rejected session before showing the login form.
    }
    let win = window(app)?;
    let state = app.state::<Pixiv>();
    state.login_open.store(true, Ordering::SeqCst);
    let target = if !force && user_id(app).is_some() { HOME } else { LOGIN };
    win.navigate(Url::parse(target).unwrap()).map_err(|e| e.to_string())?;
    win.show().map_err(|e| e.to_string())?;
    let _ = win.set_focus();
    Ok(())
}

/// Forget the login: clear this window's own data store (cookies included), leave the page blank.
pub fn logout(app: &AppHandle) -> Result<(), String> {
    let win = window(app)?;
    let state = app.state::<Pixiv>();
    let _one = state.busy.lock().unwrap();
    warm(&win); // otherwise the saved cookies are not loaded yet and survive the clearing
    win.clear_all_browsing_data().map_err(|e| e.to_string())?;
    state.login_open.store(false, Ordering::SeqCst);
    let _ = win.navigate(Url::parse("about:blank").unwrap());
    let _ = win.hide();
    Ok(())
}
