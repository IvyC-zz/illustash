//! App-wide credentials and desktop login; explicit account actions authorize only
//! their validation requests. Saving a credential never authorizes background I/O.
use crate::{app_library::Library, http_json::Json, Error, Result};
use serde_json::{json, Value};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};

static ACCOUNT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
pub const PIXIV_EXPIRED: &str = "Pixiv 登录已过期，请到设置页「绑定的账号」重新登录";
pub const PIXIV_NO_WINDOW: &str = "Pixiv 登录在桌面 app 的登录窗口里，现在不是从桌面 app 启动的";
fn invalid(message: impl Into<String>) -> Error {
    Error::Invalid(message.into())
}
/// Non-secret facts for background consumers. Authorization is passed by the
/// caller's task policy and is never derived from presence of credentials.
pub fn network_state(authorized: bool) -> Result<Value> {
    let _guard = ACCOUNT_LOCK.lock().expect("app_accounts mutex poisoned");
    let m = meta()?;
    let in_app = app(&m);
    let user_id = if in_app {
        m.get("pixiv").and_then(|p| p.get("user_id")).map(|v| v.text().to_owned())
    } else {
        secret_inner("pixiv")?.map(|s| s.split('_').next().unwrap_or("").to_owned())
    };
    Ok(json!({"authorized":authorized,"desktop":desktop_available(),"pixiv_in_app":in_app,
        "pixiv_user_id":user_id,"pixiv_expired":m.get("pixiv").and_then(|p|p.get("expired")).map(Json::text),
        "saucenao_bound":secret_inner("saucenao")?.is_some()}))
}
/// Sensitive fields deliberately have no Debug/Serialize implementation.
pub struct ModelCredentials {
    pub key: Option<String>,
    pub model: String,
    pub base: String,
}
pub fn model_credentials(c: &rusqlite::Connection) -> Result<ModelCredentials> {
    let v = c
        .prepare("SELECT key,value FROM name_settings")?
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<std::collections::HashMap<_, _>>>()?;
    Ok(ModelCredentials {
        key: v.get("model_key").or_else(|| v.get("deepseek_key")).filter(|s| !s.is_empty()).cloned(),
        model: v
            .get("model_name")
            .or_else(|| v.get("deepseek_model"))
            .cloned()
            .unwrap_or_else(|| "deepseek-chat".into()),
        base: v.get("model_base").cloned().unwrap_or_else(|| "https://api.deepseek.com".into()),
    })
}
fn support() -> Result<PathBuf> {
    crate::platform_credentials::support()
}
fn check(kind: &str) -> Result<()> {
    if !["pixiv", "saucenao"].contains(&kind) {
        return Err(invalid("未知的账号类型"));
    }
    Ok(())
}
/// Secret-bearing internal interface, never serialized by account HTTP responses.
pub fn secret(kind: &str) -> Result<Option<String>> {
    let _guard = ACCOUNT_LOCK.lock().expect("app_accounts mutex poisoned");
    secret_inner(kind)
}
fn secret_inner(kind: &str) -> Result<Option<String>> {
    check(kind)?;
    crate::platform_credentials::read(kind)
}
fn put(kind: &str, value: &str) -> Result<()> {
    check(kind)?;
    crate::platform_credentials::write(kind, value)
}
fn remove_secret(kind: &str) -> Result<()> {
    check(kind)?;
    crate::platform_credentials::remove(kind)
}
fn meta() -> Result<Json> {
    Ok(fs::read_to_string(support()?.join("accounts.json"))
        .ok()
        .and_then(|s| crate::http_json::parse_python(&s).ok())
        .filter(|j| matches!(j, Json::Object(_)))
        .unwrap_or(Json::Object(vec![])))
}
fn save(meta: &Json) -> Result<()> {
    let dir = support()?;
    fs::create_dir_all(&dir)?;
    let tmp = dir.join("accounts.tmp");
    fs::write(&tmp, format!("{}\n", meta.dump()))?;
    fs::rename(tmp, dir.join("accounts.json"))?;
    Ok(())
}
fn app(meta: &Json) -> bool {
    meta.get("pixiv").and_then(|j| j.get("via")).is_some_and(|j| j.text() == "app")
}
fn checked(mut entry: Json) -> Json {
    entry.set("checked_at", chrono::Local::now().format("%Y-%m-%d %H:%M").to_string().into());
    entry
}
fn erase(entry: &mut Json, key: &str) {
    if let Json::Object(fields) = entry {
        fields.retain(|(k, _)| k != key);
    }
}

pub fn desktop_available() -> bool {
    crate::app_desktop::available()
}
pub fn desktop(method: &str, route: &str, timeout: u64) -> Result<Json> {
    if !desktop_available() {
        return Err(invalid("不是从 Illustash 桌面 app 启动的，用不了 Pixiv 登录窗口"));
    }
    crate::app_desktop::request(method, route, timeout)
}
fn read_json(response: ureq::Response) -> Result<Json> {
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(5 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| invalid("账号服务响应读取失败"))?;
    if bytes.len() > 5 * 1024 * 1024 {
        return Err(invalid("账号服务响应超过限额"));
    }
    let s = std::str::from_utf8(&bytes).map_err(|_| invalid("账号服务响应编码不正确"))?;
    crate::http_json::parse_python(s).map_err(|_| invalid("账号服务响应不正确"))
}
/// Authorized query transport for #51; callers must provide explicit network authorization.
/// A saved login does not imply authorization; app-backed requests never fall back to cookies.
pub fn pixiv_get(path: &str, authorized: bool, session: Option<&str>, in_app: bool) -> Result<Json> {
    if !authorized {
        return Err(invalid("尚未授权联网"));
    }
    if !path.starts_with("/ajax/") || path.contains(['\r', '\n']) {
        return Err(invalid("只允许 Pixiv ajax 查询"));
    }
    if in_app {
        let route =
            format!("/pixiv/fetch?path={}", url::form_urlencoded::byte_serialize(path.as_bytes()).collect::<String>());
        let reply = desktop("GET", &route, 120)?;
        let status = reply.get("status").map(Json::integer).unwrap_or(0);
        if status == 401 {
            return Err(invalid("Pixiv 没有接受这个登录状态"));
        }
        if status == 404 {
            return Ok(Json::Scalar(Value::Null));
        }
        if status == 403
            || status == 429
            || (status == 503 && reply.get("body").map(Json::text).unwrap_or("").to_lowercase().contains("maintenance"))
        {
            return Err(invalid("Pixiv 服务拒绝请求"));
        }
        if !(200..300).contains(&status) {
            return Err(invalid("Pixiv 窗口里的请求失败"));
        }
        return crate::http_json::parse_python(reply.get("body").map(Json::text).unwrap_or(""))
            .map_err(|_| invalid("Pixiv 响应不正确"));
    }
    let base = match std::env::var("ILLUSTASH_PIXIV_BASE") {
        Ok(base) => {
            let u = url::Url::parse(&base).map_err(|_| invalid("合成 Pixiv 服务地址不正确"))?;
            if u.scheme() != "http"
                || u.host_str() != Some("127.0.0.1")
                || u.port().is_none()
                || !u.username().is_empty()
                || u.password().is_some()
                || u.path() != "/"
                || u.query().is_some()
                || u.fragment().is_some()
            {
                return Err(invalid("Pixiv 替身只允许本机带端口 HTTP"));
            }
            base.trim_end_matches('/').to_owned()
        }
        Err(_) => "https://www.pixiv.net".into(),
    };
    let agent = ureq::AgentBuilder::new().redirects(0).timeout(Duration::from_secs(25)).build();
    let mut req = agent
        .get(&format!("{base}{path}"))
        .set("Referer", "https://www.pixiv.net/")
        .set("User-Agent", "Illustash/0.1 (+local library; provenance lookups)");
    if let Some(s) = session {
        req = req.set("Cookie", &format!("PHPSESSID={s}"));
    }
    match req.call() {
        Ok(r) => read_json(r),
        Err(ureq::Error::Status(401, _)) => Err(invalid("Pixiv 没有接受这个登录状态")),
        Err(ureq::Error::Status(404, _)) => Ok(Json::Scalar(Value::Null)),
        Err(ureq::Error::Status(403 | 429, _)) => Err(invalid("Pixiv 服务拒绝请求")),
        Err(ureq::Error::Status(503, response)) => {
            let mut reason = String::new();
            let _ = response.into_reader().take(4096).read_to_string(&mut reason);
            Err(invalid(if reason.to_lowercase().contains("maintenance") {
                "Pixiv 服务拒绝请求"
            } else {
                "Pixiv 请求失败"
            }))
        }
        Err(_) => Err(invalid("Pixiv 请求失败")),
    }
}
// Account validation owns one Net-equivalent policy across its two queries.
// pixiv_get remains the one-request transport for task consumers to coordinate.
fn account_query(
    path: &str,
    session: Option<&str>,
    in_app: bool,
    last: &mut Option<std::time::Instant>,
) -> Result<Json> {
    for attempt in 0..3 {
        if let Some(started) = last {
            let elapsed = started.elapsed();
            if elapsed < Duration::from_secs(2) {
                std::thread::sleep(Duration::from_secs(2) - elapsed);
            }
        }
        *last = Some(std::time::Instant::now());
        match pixiv_get(path, true, session, in_app) {
            Err(Error::Invalid(ref message))
                if attempt < 2
                    && ["Pixiv 请求失败", "Pixiv 窗口里的请求失败", "桌面 app 没有响应或拒绝请求"]
                        .contains(&message.as_str()) =>
            {
                std::thread::sleep(Duration::from_secs(2 * (attempt + 1)));
            }
            result => return result,
        }
    }
    unreachable!()
}
fn who(uid: &str, session: Option<&str>, in_app: bool) -> Result<Json> {
    let mut last = None;
    let extra = match account_query("/ajax/user/extra?lang=ja", session, in_app, &mut last) {
        Err(Error::Invalid(message)) if message == "Pixiv 没有接受这个登录状态" => {
            return Err(invalid("Pixiv 没有接受这个登录状态（可能已退出或过期）（Pixiv 返回 HTTP 401）"));
        }
        result => result?,
    };
    if matches!(extra, Json::Scalar(Value::Null)) || extra.get("error").is_some_and(crate::app_service::truthy) {
        let why = if matches!(extra, Json::Scalar(Value::Null)) {
            "HTTP 404".to_string()
        } else {
            format!(
                "error：{}",
                extra
                    .get("message")
                    .map(Json::text)
                    .filter(|s| !s.is_empty())
                    .unwrap_or("无说明")
                    .chars()
                    .take(80)
                    .collect::<String>()
            )
        };
        return Err(invalid(format!("Pixiv 没有接受这个登录状态（可能已退出或过期）（Pixiv 返回 {why}）")));
    }
    let user = account_query(&format!("/ajax/user/{uid}?lang=ja"), session, in_app, &mut last)?;
    Ok(Json::object([
        ("user_id", uid.into()),
        (
            "name",
            user.get("body")
                .and_then(|b| b.get("name"))
                .filter(|v| crate::app_service::truthy(v))
                .cloned()
                .unwrap_or("".into()),
        ),
        ("following", extra.get("body").and_then(|b| b.get("following")).cloned().unwrap_or(Value::Null.into())),
    ]))
}
fn clean(value: &str) -> Result<String> {
    let re = regex::Regex::new(r"PHPSESSID=([^;\s]+)").expect("constant regular expression is valid");
    let raw = re.captures(value).map(|c| c[1].to_owned()).unwrap_or_else(|| value.trim().to_owned());
    if !regex::Regex::new(r"^\d+_[A-Za-z0-9]+$").expect("constant regular expression is valid").is_match(&raw) {
        return Err(invalid("PHPSESSID 的格式不对"));
    }
    Ok(raw)
}
pub fn status() -> Result<Json> {
    let _guard = ACCOUNT_LOCK.lock().expect("app_accounts mutex poisoned");
    let meta = meta()?;
    let mut out = Json::Object(vec![]);
    for kind in ["pixiv", "saucenao"] {
        let bound = secret_inner(kind)?.is_some();
        let mut entry = Json::object([("bound", bound.into())]);
        if let Some(Json::Object(fields)) = meta.get(kind) {
            for (k, v) in fields {
                entry.set(k, v.clone());
            }
        }
        if kind == "pixiv" {
            entry.set("bound", (bound || app(&meta)).into());
        }
        out.set(kind, entry);
    }
    let available = desktop_available();
    out.set("desktop", available.into());
    if available && out.get("pixiv").and_then(|p| p.get("expired")).is_some_and(crate::app_service::truthy) {
        if let Ok(s) = desktop("GET", "/pixiv/session", 30) {
            let mut p = out.get("pixiv").expect("app_accounts internal invariant").clone();
            p.set("window_logged_in", s.get("loggedIn").is_some_and(crate::app_service::truthy).into());
            out.set("pixiv", p);
        }
    }
    Ok(out)
}
/// Metadata only: no network, no desktop call and no SauceNAO/keychain lookup.
pub fn pixiv_binding() -> Result<Value> {
    let _guard = ACCOUNT_LOCK.lock().expect("app_accounts mutex poisoned");
    serde_json::from_str(&meta()?.get("pixiv").cloned().unwrap_or(Json::Object(vec![])).dump())
        .map_err(|_| invalid("账号元数据格式不正确"))
}
fn updated_pixiv(mut meta: Json, who: Option<&Value>, problem: Option<&str>, detail: Option<&str>) -> Json {
    let mut entry = meta.get("pixiv").cloned().unwrap_or(Json::Object(vec![]));
    if let Some(Value::Object(fields)) = who {
        // _who's established insertion order must also hold for a fresh entry.
        for key in ["user_id", "name", "following"] {
            if let Some(value) = fields.get(key) {
                entry.set(key, value.clone().into());
            }
        }
        for (key, value) in fields {
            if !["user_id", "name", "following"].contains(&key.as_str()) {
                entry.set(key, value.clone().into());
            }
        }
    }
    entry = checked(entry);
    if let Some(problem) = problem.filter(|p| !p.is_empty()) {
        entry.set("expired", problem.into());
        entry.set("expired_detail", detail.unwrap_or("").into());
    } else {
        erase(&mut entry, "expired");
        erase(&mut entry, "expired_detail");
    }
    meta.set("pixiv", entry);
    meta
}
/// The caller's authorized Net owns status/bytes, expiry diagnosis and retry
/// policy; this only records the same non-secret facts as legacy mark_pixiv.
pub fn mark_pixiv(who: Option<&Value>, problem: Option<&str>, detail: Option<&str>) -> Result<Value> {
    let _guard = ACCOUNT_LOCK.lock().expect("app_accounts mutex poisoned");
    let m = updated_pixiv(meta()?, who, problem, detail);
    save(&m)?;
    serde_json::from_str(&m.get("pixiv").expect("app_accounts internal invariant").dump())
        .map_err(|_| invalid("账号元数据格式不正确"))
}
/// HTTP mutation is serialized with the current session by the native host. No
/// background task survives the request, and a switch waits for bounded I/O.
pub(crate) fn action(library: &Library, action: &str, body: &Json) -> Result<Json> {
    // Convert I/O/SQLite/archive failures to the legacy Invalid (400) contract.
    // X only mutates its captured package; do not hold the account-store lock
    // while taking its SQLite writer lock.
    let _guard =
        if action == "x/import" { None } else { Some(ACCOUNT_LOCK.lock().expect("app_accounts mutex poisoned")) };
    action_inner(library, action, body).map_err(|e| match e {
        Error::Invalid(_) => e,
        _ => invalid("账号操作失败"),
    })
}
fn action_inner(library: &Library, action: &str, body: &Json) -> Result<Json> {
    let value = body.get("value").map(Json::text).unwrap_or("");
    let mut meta = meta()?;
    match action {
        "saucenao" => {
            let key = value.trim();
            if key.len() != 40 || !key.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(invalid("SauceNAO 的 API key 应该是 40 位十六进制字符"));
            }
            put("saucenao", key)?;
            let entry = checked(Json::Object(vec![]));
            meta.set("saucenao", entry.clone());
            save(&meta)?;
            Ok(entry)
        }
        "pixiv" => {
            let s = clean(value)?;
            let entry = checked(who(s.split('_').next().expect("app_accounts internal invariant"), Some(&s), false)?);
            put("pixiv", &s)?;
            meta.set("pixiv", entry.clone());
            save(&meta)?;
            Ok(entry)
        }
        "pixiv/login" => {
            let force = meta.get("pixiv").and_then(|p| p.get("expired")).is_some_and(crate::app_service::truthy);
            desktop("POST", if force { "/pixiv/login?force=1" } else { "/pixiv/login" }, 30)?;
            Ok(json!({"ok":true}).into())
        }
        "pixiv/connect" => {
            let s = desktop("GET", "/pixiv/session", 30)?;
            let mut who_entry = None;
            if s.get("loggedIn").is_some_and(crate::app_service::truthy) {
                let uid = s.get("userId").map(Json::text).unwrap_or("");
                let w = who(uid, None, true);
                match w {
                    Err(Error::Invalid(ref msg))
                        if (msg == "Pixiv 窗口里的请求失败" || msg == "桌面 app 没有响应或拒绝请求")
                            && s.get("loginOpen").is_some_and(crate::app_service::truthy) => {}
                    Err(e) => return Err(e),
                    Ok(mut w) => {
                        w.set("via", "app".into());
                        let entry = checked(w);
                        remove_secret("pixiv")?;
                        meta.set("pixiv", entry.clone());
                        save(&meta)?;
                        who_entry = Some(entry);
                    }
                }
            }
            let mut out = Json::object([("bound", who_entry.is_some().into())]);
            if let Some(Json::Object(fields)) = who_entry {
                for (k, v) in fields {
                    out.set(&k, v);
                }
            }
            out.set(
                "login_open",
                desktop("GET", "/pixiv/session", 30)?.get("loginOpen").is_some_and(crate::app_service::truthy).into(),
            );
            Ok(out)
        }
        "pixiv/check" => {
            let in_app = app(&meta);
            let s = if in_app { None } else { secret_inner("pixiv")? };
            if !in_app && s.is_none() {
                return Err(invalid("还没有绑定 Pixiv"));
            }
            if in_app && !desktop_available() {
                return Err(invalid(PIXIV_NO_WINDOW));
            }
            let mut result = None;
            let mut expired_detail = String::new();
            for attempt in 0..if in_app { 2 } else { 1 } {
                let w = if in_app {
                    let state = desktop("GET", "/pixiv/session", 30)?;
                    if !state.get("loggedIn").is_some_and(crate::app_service::truthy) {
                        Err(invalid("登录窗口里没有 Pixiv 登录"))
                    } else {
                        who(state.get("userId").map(Json::text).unwrap_or(""), None, true)
                    }
                } else {
                    who(
                        s.as_ref()
                            .expect("app_accounts internal invariant")
                            .split('_')
                            .next()
                            .expect("app_accounts internal invariant"),
                        s.as_deref(),
                        false,
                    )
                };
                match w {
                    Ok(w) => {
                        result = Some(w);
                        break;
                    }
                    Err(Error::Invalid(ref msg))
                        if msg.starts_with("Pixiv 没有接受这个登录状态") || msg == "登录窗口里没有 Pixiv 登录" =>
                    {
                        expired_detail = msg.clone();
                        if in_app && attempt == 0 {
                            std::thread::sleep(Duration::from_secs(5));
                        }
                    }
                    Err(e) => return Err(e),
                }
            }
            let ok = result.is_some();
            let who_value = result
                .as_ref()
                .map(|v| serde_json::from_str::<Value>(&v.dump()).expect("app_accounts internal invariant"));
            meta = updated_pixiv(
                meta,
                who_value.as_ref(),
                if ok { None } else { Some(PIXIV_EXPIRED) },
                if in_app { Some(&expired_detail) } else { None },
            );
            let entry = meta.get("pixiv").expect("app_accounts internal invariant").clone();
            save(&meta)?;
            let mut out = Json::object([("ok", ok.into()), ("bound", true.into())]);
            if let Json::Object(fields) = entry {
                for (k, v) in fields {
                    out.set(&k, v);
                }
            }
            Ok(out)
        }
        "pixiv/unbind" | "saucenao/unbind" => {
            let kind = action.split('/').next().expect("app_accounts internal invariant");
            remove_secret(kind)?;
            if kind == "pixiv" && app(&meta) && desktop_available() {
                desktop("POST", "/pixiv/logout", 30)?;
            }
            erase(&mut meta, kind);
            save(&meta)?;
            Ok(json!({"ok":true}).into())
        }
        "x/import" => {
            let path = body.get("path").map(Json::text).unwrap_or("");
            let expanded = if let Some(suffix) = path.strip_prefix("~/") {
                crate::app_paths::user_home()?.join(suffix)
            } else {
                PathBuf::from(path)
            };
            let n = import_archive(library, &expanded)?;
            Ok(Json::object([("new", n.into()), ("path", path.into())]))
        }
        _ => Err(invalid("未知操作")),
    }
}
fn following_file(path: &Path) -> Result<PathBuf> {
    let mut found = Vec::new();
    let mut todo = vec![path.to_owned()];
    while let Some(dir) = todo.pop() {
        for e in fs::read_dir(&dir)? {
            let e = e?;
            let ty = e.file_type()?;
            if ty.is_dir() {
                todo.push(e.path());
            } else if ty.is_file()
                && e.file_name() == "following.js"
                && e.path().parent().and_then(Path::file_name).is_some_and(|n| n == "data")
            {
                found.push(e.path());
            }
        }
    }
    found.sort();
    found.into_iter().next().ok_or_else(|| invalid("没有找到 data/following.js"))
}
pub fn import_archive(library: &Library, path: &Path) -> Result<i64> {
    const LIMIT: u64 = 64 * 1024 * 1024;
    let mut text = String::new();
    if path.is_dir() {
        fs::File::open(following_file(path)?)?.take(LIMIT + 1).read_to_string(&mut text)?;
    } else {
        let mut zip = zip::ZipArchive::new(fs::File::open(path)?).map_err(|_| invalid("归档格式不正确"))?;
        let index = (0..zip.len())
            .find(|i| zip.by_index(*i).is_ok_and(|f| f.name().ends_with("data/following.js")))
            .ok_or_else(|| invalid("归档里没有 data/following.js"))?;
        zip.by_index(index).map_err(|_| invalid("归档读取失败"))?.take(LIMIT + 1).read_to_string(&mut text)?;
    }
    if text.len() as u64 > LIMIT {
        return Err(invalid("关注列表超过限额"));
    }
    let (_, data) = text.split_once('=').ok_or_else(|| invalid("关注列表格式不正确"))?;
    let records: Vec<Value> = serde_json::from_str(data).map_err(|_| invalid("关注列表格式不正确"))?;
    let mut ids = Vec::new();
    for item in records {
        if let Some(id) = item.get("following").and_then(|v| v.get("accountId")) {
            let s = match id {
                Value::String(s) => s.clone(),
                Value::Number(n) => n.to_string(),
                Value::Null => continue,
                _ => return Err(invalid("关注账号 ID 不正确")),
            };
            if s.is_empty() {
                continue;
            }
            if !s.bytes().all(|b| b.is_ascii_digit()) {
                return Err(invalid("关注账号 ID 不正确"));
            }
            ids.push(s);
        }
    }
    let mut c = crate::sqlite::open(library.instance_path().join("provenance.sqlite"))?;
    c.busy_timeout(Duration::from_secs(30))?;
    c.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
    let version: i64 = c.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version > 5 {
        return Err(invalid("provenance.sqlite 版本比程序新"));
    }
    let tx = c.transaction()?;
    if version < 5 {
        tx.execute_batch(include_str!("app_migration_sql/provenance.sql"))?;
        let colour = tx
            .prepare("PRAGMA table_info(ref_sigs)")?
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .iter()
            .any(|s| s == "colour");
        if !colour {
            tx.execute_batch("ALTER TABLE ref_sigs ADD COLUMN colour BLOB")?;
        }
        tx.execute_batch("PRAGMA user_version=5")?;
    }
    // One prepared bulk statement preserves input order/OR IGNORE semantics,
    // and avoids repeatedly crossing SQLite for existing archive IDs.
    let data = serde_json::to_string(&ids).map_err(|_| invalid("关注列表格式不正确"))?;
    let count = tx.execute(
        "INSERT OR IGNORE INTO leads(source,system,key) SELECT 'x_following','x_id',value FROM json_each(?)",
        [data],
    )? as i64;
    tx.commit()?;
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unauthorized_transport_stops_before_reading_any_credentials_or_bridge() {
        assert!(
            matches!(pixiv_get("/ajax/user/extra", false, Some("synthetic"), true), Err(Error::Invalid(message)) if message=="尚未授权联网")
        );
    }
    #[test]
    fn mark_merges_identity_and_preserves_other_account_and_field_order() {
        let original=crate::http_json::parse_python(r#"{"pixiv":{"user_id":"123","name":"old","following":1,"via":"app","expired":"old","expired_detail":"old"},"saucenao":{"checked_at":"synthetic"}}"#).unwrap();
        let m = updated_pixiv(original, Some(&json!({"user_id":"123","name":"new","following":7})), None, None);
        assert_eq!(m.get("saucenao").unwrap().dump(), r#"{"checked_at": "synthetic"}"#);
        let p = m.get("pixiv").unwrap();
        assert_eq!(p.get("name").unwrap().text(), "new");
        assert_eq!(p.get("via").unwrap().text(), "app");
        assert!(p.get("expired").is_none());
        assert!(p.dump().starts_with(r#"{"user_id": "123", "name": "new", "following": 7, "via": "app""#));
        let m = updated_pixiv(m, None, Some(PIXIV_EXPIRED), Some("HTTP 401"));
        assert_eq!(m.get("pixiv").unwrap().get("expired_detail").unwrap().text(), "HTTP 401");
    }
    #[test]
    fn model_credentials_keep_legacy_fallback_without_network_authorization() {
        let c = rusqlite::Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE name_settings(key TEXT PRIMARY KEY,value TEXT); INSERT INTO name_settings VALUES ('deepseek_key','synthetic-key'),('deepseek_model','synthetic-model');").unwrap();
        let model = model_credentials(&c).unwrap();
        assert_eq!(model.key.as_deref(), Some("synthetic-key"));
        assert_eq!(model.model, "synthetic-model");
        assert_eq!(model.base, "https://api.deepseek.com");
    }
}
