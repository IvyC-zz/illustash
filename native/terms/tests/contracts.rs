//! Fixed HTTP contracts of the native host, as the comparison baseline of the first public test release (#201): the
//! four scenarios of the former Python-reference contracts, recorded from main on core version 7 (PR #239), replayed
//! against this build. This executable and its host child need no Python (`tools/test_native_contracts.sh` runs it
//! with an empty PATH).
//!
//! Each scenario materializes its synthetic package, starts the host with no inherited environment (as the
//! contracts do), and replays the requests. A library at an older core version than this build is opened with
//! `--upgrade`, the way the user's libraries are (#193): one pre-upgrade backup, holding the library as it was.
//! Replies are compared as bytes after `common/baseline.rs` masking (and the random session token); every
//! database of the package is compared after start-up and after each request, by the rules there.
#![allow(clippy::unwrap_used, reason = "集成测试里 unwrap 就是断言")]
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
#[path = "common/entry.rs"]
mod entry;

const DATABASES: [&str; 3] = ["source.sqlite", "library.sqlite", "provenance.sqlite"];

struct Sandbox(PathBuf);
impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
struct Host {
    child: Child,
    reaped: bool,
}
impl Drop for Host {
    fn drop(&mut self) {
        if !self.reaped {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
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
    states_at(package, root, illustash_terms::SCHEMA_VERSION)
}

/// The compared content, `version` being the current core version (the recorded inputs: the recorded one).
fn states_at(package: &Path, root: &Path, version: i64) -> BTreeMap<String, baseline::State> {
    let roots = text_masks(root);
    let masks = baseline::Masks { roots: &roots, version };
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
        .set("Cookie", &entry::cookie(origin))
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
        "illustash-contract-{}-{category}-{:x}",
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

fn launch(root: &Path, package: &Path, category: &str, upgrade: bool, binary: &Path) -> (Host, String, String) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let origin = format!("http://127.0.0.1:{port}");
    let mut command = Command::new(binary);
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
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    if category != "task_recovery" {
        command.arg("--no-sync");
    }
    if upgrade {
        command.arg("--upgrade");
    }
    let mut host = Host { child: command.spawn().unwrap(), reaped: false };
    let (ready, lines) = std::sync::mpsc::channel();
    let stdout = host.child.stdout.take().unwrap();
    thread::spawn(move || {
        for line in std::io::BufRead::lines(std::io::BufReader::new(stdout)).map_while(Result::ok) {
            if entry::address(&line).is_some() {
                let _ = ready.send(line);
            }
        }
    });
    let start = Instant::now();
    let line = loop {
        assert!(host.child.try_wait().unwrap().is_none(), "host exited at startup");
        if let Ok(line) = lines.recv_timeout(Duration::from_millis(20)) {
            break line;
        }
        assert!(start.elapsed() < Duration::from_secs(15), "host startup timeout");
    };
    entry::enter(&line);
    let (status, bootstrap, _) = request(&origin, "/api/bootstrap", None, "");
    assert_eq!(status, 200);
    let bootstrap: Value = serde_json::from_str(&bootstrap).unwrap();
    (host, origin, bootstrap["token"].as_str().unwrap().to_string())
}

fn load(category: &str) -> Value {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("tests/fixtures/baseline/contracts/{category}.json.gz"));
    let mut text = String::new();
    flate2::read::GzDecoder::new(fs::File::open(&path).unwrap()).read_to_string(&mut text).unwrap();
    let fixture: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(fixture["format"], baseline::FORMAT);
    fixture
}

/// One pre-upgrade backup, holding the library as it was.
fn check_backup(
    package: &Path,
    root: &Path,
    before: &BTreeMap<String, baseline::State>,
    recorded: i64,
    category: &str,
) {
    let backups: Vec<PathBuf> = fs::read_dir(package.join("library-backups"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "sqlite"))
        .collect();
    assert_eq!(backups.len(), 1, "one pre-upgrade backup");
    let check = root.join("backup-check");
    fs::create_dir(&check).unwrap();
    fs::copy(&backups[0], check.join("library.sqlite")).unwrap();
    compare(
        &BTreeMap::from([("library.sqlite".to_owned(), before["library.sqlite"].clone())]),
        &states_at(&check, root, recorded),
        &format!("{category}/pre-upgrade backup"),
    );
    fs::remove_dir_all(&check).unwrap();
}

fn replay(category: &str) {
    let fixture = load(category);
    let (_scratch, root, package) = materialize(&fixture, category);
    let recorded = fixture["recorded"]["terms_version"].as_i64().unwrap();
    let before = states_at(&package, &root, recorded);
    let version = {
        let c = Connection::open_with_flags(package.join("library.sqlite"), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
        baseline::terms_version(&c)
    };
    // Recorded with `--upgrade` (upgrade_v6): the host upgrades it, as recorded. The others were at the core version
    // of the recording; on a later core they are upgraded first with the core's own command, as `baseline.rs` does
    // (the host's `--upgrade` would also bring these synthetic packages' older source.sqlite layout up to date, which
    // was not part of the recording), and the pre-upgrade backup is checked and removed.
    let upgrade = version < recorded;
    if version == recorded && recorded < illustash_terms::SCHEMA_VERSION {
        let done = Command::new(env!("CARGO_BIN_EXE_illustash-terms"))
            .args(["migrate", "--db"])
            .arg(package.join("library.sqlite"))
            .output()
            .unwrap();
        assert!(done.status.success(), "{category}: upgrade: {}", String::from_utf8_lossy(&done.stderr));
        check_backup(&package, &root, &before, recorded, category);
        fs::remove_dir_all(package.join("library-backups")).unwrap();
    }
    let (_host, origin, token) =
        launch(&root, &package, category, upgrade, Path::new(env!("CARGO_BIN_EXE_illustash-terms")));
    let mut want = apply(&before, fixture.get("startup")).unwrap();
    compare(&want, &states(&package, &root), &format!("{category}/startup"));
    if upgrade {
        check_backup(&package, &root, &before, recorded, category);
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

/// A reply as JSON (the benchmark reads values from replies).
#[cfg(unix)]
fn request_json(origin: &str, url: &str, body: Option<&Value>, token: &str) -> (u16, Value, Map<String, Value>) {
    let (status, raw, headers) = request(origin, url, body, token);
    (status, serde_json::from_str(&raw).unwrap_or(Value::Null), headers)
}

// One independent trial per executable invocation. The shell runner takes exactly
// three trials for each explicit release host and reports their medians.
#[cfg(unix)]
#[test]
#[ignore = "explicit release performance runner only"]
fn benchmark_release_host() {
    if cfg!(debug_assertions) {
        panic!("benchmark must be compiled --release");
    }
    let binary = std::env::var_os("ILLUSTASH_BENCH_BINARY").expect("explicit release binary");
    let (_scratch, root, package) = materialize(&load("http_api"), "benchmark");
    // Fixed 4096 synthetic application/source assets, two reference characters.
    // Three original assets keep their claims; others are unclassified, no files decoded.
    for name in ["library.sqlite", "source.sqlite"] {
        let mut c = Connection::open(package.join(name)).unwrap();
        let tx = c.transaction().unwrap();
        for i in 3..4096 {
            let id = format!("synthetic-{i:05}");
            let filename = format!("{id}.png");
            if name == "library.sqlite" {
                tx.execute("INSERT INTO assets(uuid,filename,src_path,width,height,added_at,src_kind) VALUES (?,?,?,120,150,'2026-09-22','orig')",
                    rusqlite::params![id, filename, root.join("folder").join(&filename).to_str()]).unwrap();
            } else {
                tx.execute("INSERT INTO assets(uuid,filename,src_path,width,height,added_at,src_kind,in_scope,trashed) VALUES (?,?,?,120,150,'2026-09-22','orig',1,0)",
                    rusqlite::params![id, filename, root.join("folder").join(&filename).to_str()]).unwrap();
            }
        }
        tx.commit().unwrap();
    }
    // A physically different package has the same library ID; every switch must open that instance.
    fn copy_tree(from: &Path, to: &Path) {
        fs::create_dir(to).unwrap();
        for entry in fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            let dest = to.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy_tree(&entry.path(), &dest);
            } else {
                fs::copy(entry.path(), dest).unwrap();
            }
        }
    }
    let replacement = root.join("replacement.pilib");
    copy_tree(&package, &replacement);
    // Measure complete process launch -> package validation/recovery -> bootstrap readiness.
    let mut opens = Vec::new();
    for _ in 0..7 {
        let start = Instant::now();
        let (mut opened, _, _) = launch(&root, &package, "benchmark", false, Path::new(&binary));
        opens.push(start.elapsed().as_secs_f64() * 1000.0);
        // SAFETY: kill 只接收整数；子进程尚未回收，PID 不会被复用。
        unsafe {
            libc::kill(opened.child.id() as i32, libc::SIGTERM);
        }
        assert!(opened.child.wait().unwrap().success());
        opened.reaped = true;
    }
    opens.sort_by(f64::total_cmp);
    let (mut host, origin, mut token) = launch(&root, &package, "benchmark", false, Path::new(&binary));
    assert_eq!(request_json(&origin, "/api/assets/count", None, &token).1["total"], 4096);
    let paths = [
        ("assets", "/api/assets?limit=100&offset=2000"),
        ("count", "/api/assets/count"),
        ("detail", "/api/detail?id=a"),
    ];
    let mut samples = Map::new();
    samples.insert("open".into(), json!(opens[3]));
    for (name, url) in paths {
        for _ in 0..5 {
            assert_eq!(request_json(&origin, url, None, &token).0, 200);
        }
        let start = Instant::now();
        for _ in 0..50 {
            assert_eq!(request_json(&origin, url, None, &token).0, 200);
        }
        samples.insert(name.into(), json!(start.elapsed().as_secs_f64() * 1000.0 / 50.0));
    }
    let mut tag_ms = 0.0;
    let mut undo_ms = 0.0;
    for _ in 0..20 {
        let start = Instant::now();
        let (status, result, _) =
            request_json(&origin, "/api/tag", Some(&json!({"ids":["a","b"],"tag":"benchmark synthetic"})), &token);
        tag_ms += start.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(status, 200);
        let start = Instant::now();
        assert_eq!(request_json(&origin, "/api/undo", Some(&json!({"id":result["undo"]["id"]})), &token).0, 200);
        undo_ms += start.elapsed().as_secs_f64() * 1000.0;
    }
    samples.insert("tag".into(), json!(tag_ms / 20.0));
    samples.insert("undo".into(), json!(undo_ms / 20.0));
    let mut switches = Vec::new();
    for i in 0..21 {
        let path = if i % 2 == 0 { &replacement } else { &package };
        let start = Instant::now();
        assert_eq!(request_json(&origin, "/api/libraries/open", Some(&json!({"path":path})), &token).0, 200);
        let (status, bootstrap, _) = request_json(&origin, "/api/bootstrap", None, &token);
        assert_eq!(status, 200);
        token = bootstrap["token"].as_str().unwrap().to_owned();
        if i > 0 {
            switches.push(start.elapsed().as_secs_f64() * 1000.0);
        }
    }
    switches.sort_by(f64::total_cmp);
    samples.insert("switch".into(), json!((switches[9] + switches[10]) / 2.0));
    host.child.kill().unwrap();
    // SAFETY: libc::rusage 只含整数和 timeval，全零是合法值。
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    let mut status = 0;
    // SAFETY: PID 属于尚未回收的子进程，`status`、`usage` 是本地出参。
    assert_eq!(unsafe { libc::wait4(host.child.id() as i32, &mut status, 0, &mut usage) }, host.child.id() as i32);
    // wait4 already owns reaping; Drop must never signal a potentially reused PID.
    host.reaped = true;
    let bytes = if cfg!(target_os = "macos") { usage.ru_maxrss as f64 } else { usage.ru_maxrss as f64 * 1024.0 };
    samples.insert("rss_mib".into(), json!(bytes / 1048576.0));
    for (key, value) in samples {
        println!("BENCH {key} {}", value.as_f64().unwrap());
    }
    assert!(!root.join("python-launched").exists());
}
