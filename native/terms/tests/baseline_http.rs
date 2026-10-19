//! The comparison baseline of the first public test release (#201) for the native HTTP host: the four contract
//! scenarios of `contracts.rs`, recorded from main on core version 7 (`ILLUSTASH_RECORD_BASELINE=1` there).
//!
//! Each scenario materializes its synthetic package, starts the host with no inherited environment (as the
//! contracts do), and replays the requests. A library at an older core version than this build is opened with
//! `--upgrade`, the way the user's libraries are (#193): one pre-upgrade backup, holding the library as it was.
//! Replies are compared as bytes after `common/baseline.rs` masking (and the random session token); every
//! database of the package is compared after start-up and after each request, by the rules there.
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

#[path = "common/baseline.rs"]
mod baseline;

const DATABASES: [&str; 3] = ["source.sqlite", "library.sqlite", "provenance.sqlite"];

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

fn text_masks(root: &Path) -> impl Fn(&str) -> String + '_ {
    move |text: &str| {
        let real = root.to_str().unwrap();
        // As written, and escaped inside JSON text (Windows paths).
        let escaped = json!(real).to_string();
        text.replace(&escaped[1..escaped.len() - 1], "@ROOT@").replace(real, "@ROOT@")
    }
}

fn states(package: &Path, root: &Path) -> BTreeMap<String, baseline::State> {
    let roots = text_masks(root);
    let masks = baseline::Masks { roots: &roots, version: illustash_terms::SCHEMA_VERSION };
    DATABASES
        .iter()
        .filter(|name| package.join(name).exists())
        .map(|name| {
            let c =
                Connection::open_with_flags(package.join(name), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
            (name.to_string(), baseline::state(&c, &masks))
        })
        .collect()
}

fn apply(
    before: &BTreeMap<String, baseline::State>,
    change: Option<&Value>,
) -> Result<BTreeMap<String, baseline::State>, String> {
    let mut out = before.clone();
    for (name, delta) in change.and_then(Value::as_object).into_iter().flatten() {
        let state = baseline::apply(out.get(name).unwrap_or(&baseline::State::new()), delta)?;
        out.insert(name.clone(), state);
    }
    Ok(out)
}

fn compare(want: &BTreeMap<String, baseline::State>, got: &BTreeMap<String, baseline::State>, at: &str) {
    let mut problems = Vec::new();
    for name in DATABASES {
        let (w, g) = (want.get(name), got.get(name));
        if w != g {
            let empty = baseline::State::new();
            problems.extend(
                baseline::differences(w.unwrap_or(&empty), g.unwrap_or(&empty), 5)
                    .into_iter()
                    .map(|d| format!("{name}: {d}")),
            );
        }
    }
    assert!(problems.is_empty(), "{at}: databases differ from the baseline:\n{}", problems.join("\n"));
}

fn session_token(text: &str) -> String {
    regex::Regex::new(r#"("token":\s*)"[0-9a-f]{32,}""#)
        .unwrap()
        .replace_all(text, r#"$1"<session-token>""#)
        .into_owned()
}

fn request(origin: &str, url: &str, body: Option<&Value>, token: &str) -> (u16, String, Map<String, Value>) {
    let req = if body.is_some() { ureq::post(&format!("{origin}{url}")) } else { ureq::get(&format!("{origin}{url}")) }
        .set("Accept-Encoding", "identity")
        .set("Origin", origin)
        .set("X-CSRF-Token", token);
    let result = if let Some(body) = body { req.send_json(body) } else { req.call() };
    let response = match result {
        Ok(r) | Err(ureq::Error::Status(_, r)) => r,
        Err(e) => panic!("HTTP {url}: {e}"),
    };
    let status = response.status();
    let mut headers = Map::new();
    for k in ["Content-Type", "Cache-Control", "X-Content-Type-Options", "Referrer-Policy"] {
        if let Some(v) = response.header(k) {
            headers.insert(k.into(), json!(v));
        }
    }
    (status, response.into_string().unwrap(), headers)
}

fn materialize(fixture: &Value, category: &str) -> (Sandbox, PathBuf, PathBuf) {
    let mut nonce = [0u8; 8];
    getrandom::fill(&mut nonce).unwrap();
    let scratch = Sandbox(std::env::temp_dir().join(format!(
        "illustash-baseline-http-{}-{category}-{:x}",
        std::process::id(),
        u64::from_ne_bytes(nonce)
    )));
    fs::create_dir_all(&scratch.0).unwrap();
    let root = scratch.0.canonicalize().unwrap();
    // Windows canonical paths carry a verbatim `\\?\` prefix the host never produces.
    #[cfg(windows)]
    let root = PathBuf::from(root.to_str().unwrap().strip_prefix(r"\\?\").unwrap());
    let package = root.join("fixture.pilib");
    for dir in ["fixture.pilib", "fixture.pilib/thumbnails", "folder", "bin", "home", "support", "shared"] {
        fs::create_dir_all(root.join(dir)).unwrap();
    }
    for (name, input) in fixture["input"].as_object().unwrap() {
        let path = package.join(name);
        if let Some(sql) = input["sql"].as_str() {
            let c = Connection::open(path).unwrap();
            c.pragma_update(None, "foreign_keys", false).unwrap();
            c.execute_batch(&sql.replace("@ROOT@", root.to_str().unwrap())).unwrap();
        } else {
            // Text inputs are JSON documents: the root goes in as a JSON string body.
            let encoded = json!(root.to_str().unwrap()).to_string();
            fs::write(path, input["text"].as_str().unwrap().replace("@ROOT@", &encoded[1..encoded.len() - 1])).unwrap();
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for name in ["python", "python3", "python3.12"] {
            let path = root.join("bin").join(name);
            fs::write(&path, "#!/bin/sh\n/bin/echo forbidden > \"$ILLUSTASH_PYTHON_SENTINEL\"\nexit 97\n").unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    (scratch, root, package)
}

fn launch(root: &Path, package: &Path, category: &str, upgrade: bool) -> (Host, String, String) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let origin = format!("http://127.0.0.1:{port}");
    let mut command = Command::new(env!("CARGO_BIN_EXE_illustash-terms"));
    command
        .env_clear()
        .env("PATH", root.join("bin"))
        .env("HOME", root.join("home"))
        .env("USERPROFILE", root.join("home"))
        .env("ILLUSTASH_APP_SUPPORT", root.join("support"))
        .env("ILLUSTASH_NO_KEYCHAIN", "1")
        .env("ILLUSTASH_SHARED", root.join("shared"))
        .env("ILLUSTASH_PYTHON_SENTINEL", root.join("python-launched"))
        .env("ILLUSTASH_TEST_VERIFY_INCREMENTAL", root.join("verified"))
        .env("ILLUSTASH_TEST_KEY_COUNTER", root.join("key-counter"));
    #[cfg(windows)]
    command.env("SystemRoot", std::env::var_os("SystemRoot").expect("SystemRoot"));
    command
        .args(["http", "--no-auto-sync", "--no-names", "--no-index-check", "--no-provenance", "--library"])
        .arg(package)
        .arg("--root")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .args(["--port", &port.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    if category != "task_recovery" {
        command.arg("--no-sync");
    }
    if upgrade {
        command.arg("--upgrade");
    }
    let mut host = Host(command.spawn().unwrap());
    let start = Instant::now();
    let token = loop {
        assert!(host.0.try_wait().unwrap().is_none(), "host exited at startup");
        if let Ok(r) = ureq::get(&format!("{origin}/api/bootstrap")).call() {
            let v: Value = r.into_json().unwrap();
            break v["token"].as_str().unwrap().to_string();
        }
        assert!(start.elapsed() < Duration::from_secs(15), "host startup timeout");
        thread::sleep(Duration::from_millis(20));
    };
    (host, origin, token)
}

fn replay(category: &str) {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("tests/fixtures/baseline/contracts/{category}.json.gz"));
    let mut text = String::new();
    flate2::read::GzDecoder::new(fs::File::open(&path).unwrap()).read_to_string(&mut text).unwrap();
    let fixture: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(fixture["format"], baseline::FORMAT);
    let (_scratch, root, package) = materialize(&fixture, category);
    let before = states(&package, &root);
    let version = {
        let c = Connection::open_with_flags(package.join("library.sqlite"), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
        baseline::terms_version(&c)
    };
    let upgrade = version < illustash_terms::SCHEMA_VERSION;
    let (_host, origin, token) = launch(&root, &package, category, upgrade);
    let mut want = apply(&before, fixture.get("startup")).unwrap();
    compare(&want, &states(&package, &root), &format!("{category}/startup"));
    if upgrade {
        let backups: Vec<PathBuf> = fs::read_dir(package.join("library-backups"))
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|e| e == "sqlite"))
            .collect();
        assert_eq!(backups.len(), 1, "one pre-upgrade backup");
        let check = root.join("backup-check");
        fs::create_dir(&check).unwrap();
        fs::copy(&backups[0], check.join("library.sqlite")).unwrap();
        let backup = states(&check, &root);
        compare(
            &BTreeMap::from([("library.sqlite".to_owned(), before["library.sqlite"].clone())]),
            &backup,
            &format!("{category}/pre-upgrade backup"),
        );
    }
    let roots = text_masks(&root);
    let masks = baseline::Masks { roots: &roots, version: illustash_terms::SCHEMA_VERSION };
    for (i, step) in fixture["steps"].as_array().unwrap().iter().enumerate() {
        let at = format!("{category}/{i} {}", step["url"]);
        let url = step["url"].as_str().unwrap();
        let body = (!step["body"].is_null()).then_some(&step["body"]);
        let previous_runs: i64 = if category == "task_recovery" && body.is_some() {
            Connection::open(package.join("library.sqlite"))
                .unwrap()
                .query_row("SELECT count(*) FROM sync_runs", [], |r| r.get(0))
                .unwrap()
        } else {
            0
        };
        let (status, raw, headers) =
            request(&origin, url, body, if step["bad_token"] == true { "bad" } else { &token });
        assert_eq!(json!(status), step["status"], "{at}: status");
        if category == "task_recovery" && body.is_some() {
            let start = Instant::now();
            loop {
                let (_, progress, _) = request(&origin, "/api/sync", None, &token);
                let progress: Value = serde_json::from_str(&progress).unwrap();
                if progress["history"].as_array().unwrap().len() as i64 > previous_runs
                    && progress["history"][0]["status"].as_str().is_some_and(|s| s != "running")
                {
                    break;
                }
                assert!(start.elapsed() < Duration::from_secs(10), "recovery timeout");
                thread::sleep(Duration::from_millis(10));
            }
        }
        let reply = match step["projection"].as_str() {
            Some(key) => serde_json::from_str::<Value>(&raw).unwrap()[key].to_string(),
            None => raw,
        };
        assert_eq!(Value::Object(headers), step["headers"], "{at}: headers");
        assert_eq!(json!(session_token(&masks.output(&reply))), step["reply"], "{at}: reply");
        want = apply(&want, step.get("change")).unwrap_or_else(|e| panic!("{at}: {e}"));
        compare(&want, &states(&package, &root), &at);
    }
    assert!(!root.join("python-launched").exists());
}

#[test]
fn http_api_contract() {
    replay("http_api");
}
#[test]
fn genuine_v6_upgrade_contract() {
    replay("upgrade_v6");
}
#[test]
fn undo_and_history_contract() {
    replay("undo_history");
}
#[test]
fn interrupted_task_recovery_contract() {
    replay("task_recovery");
}
