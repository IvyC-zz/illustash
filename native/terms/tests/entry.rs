//! The native host's entry: every request needs the session cookie that only the ready line's one-time address
//! hands out, so another local process (which can set any Host and leave Origin out) reads and writes nothing; writes
//! still need Origin and the page token, and a new model address drops the stored key. Synthetic package only.
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

#[path = "common/entry.rs"]
mod entry;

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

/// Status, headers asked for and body of a request with exactly these headers (ureq adds Host itself).
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
    let root =
        std::env::temp_dir().join(format!("illustash-entry-{}-{}", std::process::id(), store::new_key().unwrap()));
    fs::create_dir_all(root.join("source")).unwrap();
    let _sandbox = Sandbox(root.clone());
    let root = illustash_platform::app_paths::canonical(&root).unwrap();
    for dir in ["home", "support", "shared"] {
        fs::create_dir_all(root.join(dir)).unwrap();
    }
    // This test binary has one test: nothing else reads the process environment meanwhile.
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
    let line = rx.recv_timeout(Duration::from_secs(60)).expect("ready line");
    let origin = format!("http://127.0.0.1:{port}");
    let address = entry::address(&line).unwrap().to_owned();
    let code = address.strip_prefix(&format!("{origin}/?k=")).expect("the address carries a code");
    assert!(code.len() == 64 && code.bytes().all(|b| b.is_ascii_hexdigit()), "{address}");

    // Before entering: whatever another local process sends — the right Host, no Origin or the right one, a guessed
    // cookie, a wrong code — nothing is read, no page or picture either.
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

    // Entering as the desktop shell does (it adds `launched`): the cookie is set and the page goes on, without
    // the code, by a refresh of its own.
    let (status, cookies, body) = call("GET", &format!("{address}&launched=1"), &[], None);
    assert_eq!(status, 200, "{body}");
    let [set] = &cookies[..] else { panic!("{cookies:?}") };
    let (pair, attributes) = set.split_once("; ").unwrap();
    assert!(pair.starts_with(&format!("illustash-{port}=")), "{set}");
    assert_eq!(attributes, "HttpOnly; SameSite=Strict; Path=/");
    assert!(body.contains(r#"<meta http-equiv="refresh" content="0;url=/?launched=1">"#), "{body}");
    assert!(!body.contains(code));
    // The code works once.
    for url in [address.clone(), format!("{address}&launched=1")] {
        let (status, cookies, _) = call("GET", &url, &[], None);
        assert_eq!((status, cookies.len()), (403, 0), "{url}");
    }

    // With the cookie everything reads as before; writes still need Origin and the page token.
    let with = [("Cookie", pair)];
    let (status, _, body) = call("GET", &format!("{origin}/api/bootstrap"), &with, None);
    assert_eq!(status, 200, "{body}");
    let token = serde_json::from_str::<Value>(&body).unwrap()["token"].as_str().unwrap().to_owned();
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
    // The browser checks hold with the cookie as well: another site's page, or a name that resolves here.
    assert_eq!(
        call("GET", &format!("{origin}/api/bootstrap"), &[("Cookie", pair), ("Origin", "http://evil.test")], None).0,
        403
    );
    assert_eq!(
        call("GET", &format!("{origin}/api/bootstrap"), &[("Cookie", pair), ("Host", "evil.test")], None).0,
        403
    );

    // The model key goes only to the address it was entered for.
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
