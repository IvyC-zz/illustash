//! 原生宿主的入口：每个请求都需要会话 cookie，而它只由就绪行里的一次性地址发放，所以本机其他进程（可以随意设置
//! Host、省略 Origin）什么都读不到、写不了；写入仍需要 Origin 和页面 token，换了新的模型地址会丢弃已存的密钥。
//! 只用合成图库包。
#![allow(clippy::unwrap_used, reason = "集成测试里 unwrap 就是断言")]
use illustash_terms::{app_create::NewSource, app_service, store};
use serde_json::{json, Value};
use std::{
    fs,
    io::{BufRead, BufReader},
    net::TcpListener,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread,
    time::Duration,
};

use crate::common::entry;

struct Sandbox(PathBuf);
impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
struct Host(Child);
impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// 只带这些请求头发出请求（Host 由 ureq 自己加），返回状态码、所需的响应头和响应体。
fn call(method: &str, url: &str, headers: &[(&str, &str)], body: Option<&Value>) -> (u16, Vec<String>, String) {
    let mut request = ureq::request(method, url);
    for (name, value) in headers {
        request = request.set(name, value);
    }
    let result = match body {
        Some(body) => request.send_json(body),
        None => request.call(),
    };
    let response = match result {
        Ok(r) | Err(ureq::Error::Status(_, r)) => r,
        Err(e) => panic!("HTTP {url}: {e}"),
    };
    let status = response.status();
    let cookies = response.all("Set-Cookie").into_iter().map(str::to_owned).collect();
    (status, cookies, response.into_string().unwrap())
}

#[test]
fn every_request_needs_the_cookie_from_the_one_time_address() {
    let _env = crate::exclusive();
    let root =
        std::env::temp_dir().join(format!("illustash-entry-{}-{}", std::process::id(), store::new_key().unwrap()));
    fs::create_dir_all(root.join("source")).unwrap();
    let _sandbox = Sandbox(root.clone());
    let root = illustash_platform::app_paths::canonical(&root).unwrap();
    for dir in ["home", "support", "shared"] {
        fs::create_dir_all(root.join(dir)).unwrap();
    }
    // 持有 `crate::exclusive()`：此期间没有别的测试在跑，也就没有别的代码读进程环境。
    std::env::set_var("ILLUSTASH_APP_SUPPORT", root.join("support"));
    std::env::set_var("ILLUSTASH_SHARED", root.join("shared"));
    std::env::set_var("ILLUSTASH_NO_KEYCHAIN", "1");
    let package = app_service::create(
        &root,
        &root.join("synthetic.pilib"),
        "synthetic",
        vec![NewSource::Folder { path: root.join("source"), preset: "other".into() }],
    )
    .unwrap();

    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let mut command = Command::new(env!("CARGO_BIN_EXE_illustash-terms"));
    command
        .env_clear()
        .env("HOME", root.join("home"))
        .env("USERPROFILE", root.join("home"))
        .env("ILLUSTASH_APP_SUPPORT", root.join("support"))
        .env("ILLUSTASH_NO_KEYCHAIN", "1")
        .env("ILLUSTASH_SHARED", root.join("shared"));
    #[cfg(windows)]
    command.env("SystemRoot", std::env::var_os("SystemRoot").expect("SystemRoot"));
    let mut child = command
        .args(["http", "--no-sync", "--no-index-check", "--no-provenance", "--library"])
        .arg(&package)
        .arg("--root")
        .arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .args(["--port", &port.to_string()])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let _host = Host(child);
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if entry::address(&line).is_some() {
                let _ = tx.send(line);
            }
        }
    });
    let line = rx.recv_timeout(crate::common::wait::limit()).expect("ready line");
    let origin = format!("http://127.0.0.1:{port}");
    let address = entry::address(&line).unwrap().to_owned();
    let code = address.strip_prefix(&format!("{origin}/?k=")).expect("the address carries a code");
    assert!(code.len() == 64 && code.bytes().all(|b| b.is_ascii_hexdigit()), "{address}");

    // 进入之前：本机其他进程不管发什么（正确的 Host，不带 Origin 或带正确的 Origin，猜的 cookie，错误的码），
    // 都读不到任何东西，页面和图片也不行。
    let host = origin.trim_start_matches("http://");
    let forged = format!("illustash-{port}=0000");
    for url in ["/api/bootstrap", "/", "/app.js", "/index.html", "/image?id=synthetic&size=full", "/api/names", "/?k=0"]
    {
        for headers in [vec![], vec![("Origin", origin.as_str())], vec![("Cookie", forged.as_str())]] {
            let (status, cookies, body) = call("GET", &format!("{origin}{url}"), &headers, None);
            assert_eq!(status, 403, "{url} {headers:?}: {body}");
            assert!(cookies.is_empty(), "{url}");
        }
    }
    let write = [("Origin", origin.as_str()), ("Host", host), ("X-CSRF-Token", "0")];
    let (status, _, _) = call(
        "POST",
        &format!("{origin}/api/names"),
        &write,
        Some(&json!({"action": "model_api", "base": "https://synthetic.invalid"})),
    );
    assert_eq!(status, 403);

    // 像桌面外壳那样进入（它会加上 `launched`）：设置好 cookie，页面再自己刷新一次继续加载，地址里不再带码。
    let (status, cookies, body) = call("GET", &format!("{address}&launched=1"), &[], None);
    assert_eq!(status, 200, "{body}");
    let [set] = &cookies[..] else { panic!("{cookies:?}") };
    let (pair, attributes) = set.split_once("; ").unwrap();
    assert!(pair.starts_with(&format!("illustash-{port}=")), "{set}");
    assert_eq!(attributes, "HttpOnly; SameSite=Strict; Path=/");
    assert!(body.contains(r#"<meta http-equiv="refresh" content="0;url=/?launched=1">"#), "{body}");
    assert!(!body.contains(code));
    // 码只能用一次。
    for url in [address.clone(), format!("{address}&launched=1")] {
        let (status, cookies, _) = call("GET", &url, &[], None);
        assert_eq!((status, cookies.len()), (403, 0), "{url}");
    }

    // 有了 cookie，读取都和以前一样；写入仍需要 Origin 和页面 token。
    let with = [("Cookie", pair)];
    let (status, _, body) = call("GET", &format!("{origin}/api/bootstrap"), &with, None);
    assert_eq!(status, 200, "{body}");
    let token = serde_json::from_str::<Value>(&body).unwrap()["token"].as_str().unwrap().to_owned();
    human_conclusion_routes(&root, &origin, &[("Cookie", pair), ("Origin", &origin), ("X-CSRF-Token", &token)]);
    assert_eq!(call("GET", &format!("{origin}/app.js"), &with, None).0, 200);
    assert_eq!(call("GET", &format!("{origin}/?launched=1"), &with, None).0, 200);
    assert_eq!(call("GET", &format!("{origin}/image?id=synthetic&size=full"), &with, None).0, 404);
    let names = format!("{origin}/api/names");
    let model = |body: Value| {
        call("POST", &names, &[("Cookie", pair), ("Origin", &origin), ("X-CSRF-Token", &token)], Some(&body))
    };
    for headers in
        [vec![("Cookie", pair), ("X-CSRF-Token", token.as_str())], vec![("Cookie", pair), ("Origin", origin.as_str())]]
    {
        let (status, _, _) =
            call("POST", &names, &headers, Some(&json!({"action": "model_api", "key": "sk-synthetic-0000"})));
        assert_eq!(status, 403, "{headers:?}");
    }
    // 带着 cookie 时浏览器相关的检查同样有效：别的网站的页面，或解析到本机的其他域名。
    assert_eq!(
        call("GET", &format!("{origin}/api/bootstrap"), &[("Cookie", pair), ("Origin", "http://evil.test")], None).0,
        403
    );
    assert_eq!(
        call("GET", &format!("{origin}/api/bootstrap"), &[("Cookie", pair), ("Host", "evil.test")], None).0,
        403
    );

    // 模型密钥只发给填写它时对应的地址。
    let configured = |reply: &str| serde_json::from_str::<Value>(reply).unwrap()["model_api"]["configured"].clone();
    let (status, _, body) =
        model(json!({"action": "model_api", "key": "sk-synthetic-0000", "base": "https://one.invalid"}));
    assert_eq!((status, configured(&body)), (200, json!(true)), "{body}");
    let (status, _, body) = model(json!({"action": "model_api", "base": "https://one.invalid/", "model": "synthetic"}));
    assert_eq!((status, configured(&body)), (200, json!(true)), "the same address keeps the key: {body}");
    let (status, _, body) = model(json!({"action": "model_api", "base": "https://attacker.invalid"}));
    assert_eq!((status, configured(&body)), (200, json!(false)), "a new address drops it: {body}");
    let (_, _, body) = call("GET", &names, &with, None);
    let state: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        (state["model_api"]["configured"].clone(), state["model_api"]["base"].clone()),
        (json!(false), json!("https://attacker.invalid"))
    );
}

fn human_conclusion_routes(root: &std::path::Path, origin: &str, headers: &[(&str, &str)]) {
    let get = |path: &str| {
        let (status, _, body) = call("GET", &format!("{origin}{path}"), headers, None);
        assert_eq!(status, 200, "{body}");
        serde_json::from_str::<Value>(&body).unwrap()
    };
    let post = |path: &str, body: Value| {
        let (status, _, text) = call("POST", &format!("{origin}{path}"), headers, Some(&body));
        (status, serde_json::from_str::<Value>(&text).unwrap())
    };
    let poll = |path: &str, export: bool| {
        let deadline = crate::common::wait::deadline();
        loop {
            let mut value = get(path);
            if export {
                value = value["export"].clone();
            }
            if !matches!(value["status"].as_str(), Some("running" | "stopping")) {
                break value;
            }
            assert!(std::time::Instant::now() < deadline, "{value}");
            thread::sleep(Duration::from_millis(20));
        }
    };
    assert_eq!(post("/api/human-conclusions/export", json!({"unknown":1})).0, 400);
    let (code, started) = post("/api/human-conclusions/export", json!({}));
    assert_eq!(code, 200, "{started}");
    assert_eq!(started["kind"], "human_export");
    let exported = poll("/api/human-conclusions", true);
    assert_eq!(exported["status"], "done", "{exported}");
    assert_eq!(exported["committed"], true);
    let summary = &exported["result"];
    assert_eq!(
        summary["detail_count"].as_u64().unwrap(),
        summary["counts"]["exported"].as_u64().unwrap()
            + summary["asset_counts"]["exported"].as_u64().unwrap()
            + summary["definition_counts"]["exported"].as_u64().unwrap()
    );
    let path = PathBuf::from(exported["result"]["path"].as_str().unwrap());
    assert_eq!(path.parent().unwrap(), root.join("home/Downloads"));
    assert!(path.is_file());
    let id = exported["result"]["report_id"].as_str().unwrap();
    assert_eq!(get(&format!("/api/human-conclusions/report?id={id}"))["report"]["committed"], true);
    assert_eq!(call("GET", &format!("{origin}/api/human-conclusions/report?id={id}&limit=501"), headers, None).0, 400);
    assert_eq!(post("/api/human-conclusions/export/cancel", json!({"id":"wrong"})).0, 409);
    let (code, started) = post("/api/human-conclusions/import", json!({"path":path}));
    assert_eq!(code, 200, "{started}");
    assert_eq!(started["kind"], "human_conclusions");
    let imported = poll("/api/imports", false);
    assert_eq!(imported["status"], "done", "{imported}");
    assert_eq!(imported["result"]["committed"], true);
    assert!(imported["result"]["undo"].is_null());
    let id = imported["result"]["report_id"].as_str().unwrap();
    assert_eq!(get(&format!("/api/human-conclusions/report?id={id}"))["report"]["report_id"], id);
    let bad = root.join("bad-human.jsonl");
    fs::write(&bad, vec![b' '; 4 * 1024 * 1024 + 1]).unwrap();
    assert_eq!(post("/api/human-conclusions/import", json!({"path":bad})).0, 200);
    let failed = poll("/api/imports", false);
    assert_eq!(failed["code"], 413, "{failed}");
    assert_eq!(failed["committed"], false);
    fs::write(&bad, b"{}\n").unwrap();
    assert_eq!(post("/api/human-conclusions/import", json!({"path":bad})).0, 200);
    let failed = poll("/api/imports", false);
    assert_eq!(failed["code"], 400, "{failed}");
    assert_eq!(failed["committed"], false);
    let deadline = crate::common::wait::deadline();
    let automatic = loop {
        let value = get("/api/human-conclusions")["automatic"].clone();
        if value["snapshots"].as_array().is_some_and(|s| !s.is_empty()) {
            break value;
        }
        assert!(std::time::Instant::now() < deadline, "{value}");
        thread::sleep(Duration::from_millis(20));
    };
    let backup = automatic["snapshots"][0]["backup_id"].as_str().unwrap();
    assert_eq!(post("/api/human-conclusions/restore", json!({"backup_id":backup})).0, 200);
    let restored = poll("/api/imports", false);
    assert_eq!(restored["status"], "done", "{restored}");
    assert_eq!(post("/api/human-conclusions/restore", json!({"backup_id":"../outside"})).0, 400);
}
