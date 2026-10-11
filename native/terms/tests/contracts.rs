//! Fixed Python-reference contracts. This executable and its native HTTP child need no Python.
//! Only owned synthetic SQL inputs are materialized; expected output is never recorded here.
use rusqlite::{types::ValueRef, Connection};
use serde_json::{json, Map, Value};
use std::fs;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

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
fn clock(value: &str) -> Value {
    assert!(
        chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S").is_ok()
            || chrono::DateTime::parse_from_rfc3339(value).is_ok(),
        "invalid runtime clock: {value}"
    );
    json!("<runtime-time>")
}
fn normalize(value: Value, root: &Path, rules: &Value) -> Value {
    match value {
        Value::String(s) => json!(s.replace(root.to_str().unwrap(), "@ROOT@")),
        Value::Array(a) => a.into_iter().map(|v| normalize(v, root, rules)).collect(),
        Value::Object(o) => o
            .into_iter()
            .map(|(k, v)| {
                let v = if k == "token" {
                    let token = v.as_str().unwrap();
                    assert!(token.len() >= 32 && token.bytes().all(|b| b.is_ascii_hexdigit()), "invalid session token");
                    json!("<session-token>")
                } else if !v.is_null() && rules["clock_keys"].as_array().unwrap().contains(&json!(k)) {
                    clock(v.as_str().unwrap())
                } else {
                    normalize(v, root, rules)
                };
                (k, v)
            })
            .collect(),
        other => other,
    }
}
fn state(package: &Path, root: &Path, rules: &Value) -> Value {
    let mut databases = Map::new();
    for name in ["source.sqlite", "library.sqlite", "provenance.sqlite"] {
        let path = package.join(name);
        if !path.exists() {
            continue;
        }
        let c = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        assert_eq!(c.query_row("PRAGMA quick_check", [], |r| r.get::<_, String>(0)).unwrap(), "ok");
        let tables = c
            .prepare("SELECT name,sql FROM sqlite_schema WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let ids: Vec<String> = if name == "library.sqlite" {
            let rule = &rules["generated_term_key"];
            c.prepare("SELECT t.key FROM terms t JOIN term_names n ON n.term_id=t.id WHERE t.origin=?1 AND t.kind=?2 AND n.source=?3 AND n.value=?4").unwrap()
                .query_map(rusqlite::params![rule["origin"].as_str().unwrap(), rule["kind"].as_str().unwrap(), rule["source"].as_str().unwrap(), rule["name"].as_str().unwrap()], |r| r.get(0)).unwrap()
                .collect::<Result<Vec<_>, _>>().unwrap()
        } else {
            Vec::new()
        };
        assert!(ids.len() <= 1, "generated tag key must be unique");
        for id in &ids {
            assert!(
                id.len() == 36
                    && [8, 13, 18, 23].iter().all(|i| id.as_bytes()[*i] == b'-')
                    && id.bytes().filter(|b| *b != b'-').all(|b| b.is_ascii_hexdigit()),
                "invalid generated UUID"
            );
        }
        let mut data = Map::new();
        for (table, ddl) in tables {
            let selection = if ddl.to_uppercase().contains("WITHOUT ROWID") { "*" } else { "rowid,*" };
            let mut stmt = c.prepare(&format!("SELECT {selection} FROM \"{}\"", table.replace('"', "\"\""))).unwrap();
            let columns: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
            let rows = stmt
                .query_map([], |r| {
                    let mut row = Vec::new();
                    for (i, column) in columns.iter().enumerate() {
                        let v = match r.get_ref(i)? {
                            ValueRef::Null => Value::Null,
                            ValueRef::Integer(n) => json!(n),
                            ValueRef::Real(n) => json!(n),
                            ValueRef::Text(s) => {
                                let s = std::str::from_utf8(s).unwrap();
                                if rules["clock_columns"][&table].as_array().is_some_and(|a| a.contains(&json!(column)))
                                {
                                    clock(s)
                                } else {
                                    normalize(
                                        json!(if ids.iter().any(|id| id == s) {
                                            rules["generated_term_key"]["placeholder"].as_str().unwrap()
                                        } else {
                                            s
                                        }),
                                        root,
                                        rules,
                                    )
                                }
                            }
                            ValueRef::Blob(b) => {
                                json!({"blob": b.iter().map(|v| format!("{v:02x}")).collect::<String>()})
                            }
                        };
                        row.push(v);
                    }
                    Ok(Value::Array(row))
                })
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            let mut rows = rows;
            rows.sort_by_key(|r| serde_json::to_string(r).unwrap());
            data.insert(table, json!({"columns": columns, "rows": rows}));
        }
        databases.insert(name.into(), Value::Object(data));
    }
    Value::Object(databases)
}
// Report the first exact mismatch instead of spilling an entire synthetic database to the log.
fn equal(actual: &Value, expected: &Value, path: &str) {
    match (actual, expected) {
        (Value::Object(a), Value::Object(e)) => {
            assert_eq!(a.keys().collect::<Vec<_>>(), e.keys().collect::<Vec<_>>(), "keys at {path}");
            for (k, v) in e {
                equal(&a[k], v, &format!("{path}/{k}"));
            }
        }
        (Value::Array(a), Value::Array(e)) => {
            assert_eq!(a.len(), e.len(), "array length at {path}");
            for (i, (av, ev)) in a.iter().zip(e).enumerate() {
                equal(av, ev, &format!("{path}/{i}"));
            }
        }
        _ => assert_eq!(actual, expected, "contract at {path}"),
    }
}
fn request(origin: &str, url: &str, body: Option<&Value>, token: &str) -> (u16, Value, Map<String, Value>) {
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
    (status, response.into_json().unwrap(), headers)
}
fn prepare(category: &str) -> (Value, Sandbox, PathBuf, PathBuf) {
    let fixture: Value = serde_json::from_slice(
        &fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("tests/fixtures/contracts/{category}.json")))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(fixture["format"], 1);
    let mut nonce = [0u8; 8];
    getrandom::fill(&mut nonce).unwrap();
    let scratch = Sandbox(std::env::temp_dir().join(format!(
        "illustash-contract-{}-{category}-{:x}",
        std::process::id(),
        u64::from_ne_bytes(nonce)
    )));
    let root = scratch.0.canonicalize().unwrap_or_else(|_| {
        fs::create_dir_all(&scratch.0).unwrap();
        scratch.0.canonicalize().unwrap()
    });
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
    // No inherited PATH, user HOME, credentials or application support. Unix sentinels
    // additionally prove common Python names were not invoked. Absolute host path only.
    // Windows has no sentinel: std resolves bare names to `.exe` only, then also searches
    // the system directories, so a script in `bin` could not intercept a launch there.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for name in ["python", "python3", "python3.12"] {
            let path = root.join("bin").join(name);
            fs::write(&path, "#!/bin/sh\n/bin/echo forbidden > \"$ILLUSTASH_PYTHON_SENTINEL\"\nexit 97\n").unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    equal(&state(&package, &root, &fixture), &fixture["before"], "input");
    (fixture, scratch, root, package)
}
fn launch(root: &Path, package: &Path, category: &str, binary: &Path) -> (Host, String, String) {
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
        .env("ILLUSTASH_PYTHON_SENTINEL", root.join("python-launched"));
    // Winsock loads its providers through %SystemRoot%; nothing else is passed through.
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
    if category == "upgrade_v6" {
        command.arg("--upgrade");
    }
    let mut host = Host { child: command.spawn().unwrap(), reaped: false };
    let start = Instant::now();
    let token = loop {
        assert!(host.child.try_wait().unwrap().is_none(), "host exited at startup");
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
    let (fixture, _scratch, root, package) = prepare(category);
    let (_host, origin, token) = launch(&root, &package, category, Path::new(env!("CARGO_BIN_EXE_illustash-terms")));
    equal(&state(&package, &root, &fixture), &fixture["startup"], "startup");
    if category == "upgrade_v6" {
        let backups = fs::read_dir(package.join("library-backups"))
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|e| e == "sqlite"))
            .collect::<Vec<_>>();
        assert_eq!(backups.len(), 1, "one preupgrade backup");
        let backup_package = root.join("backup-check");
        fs::create_dir(&backup_package).unwrap();
        fs::copy(&backups[0], backup_package.join("library.sqlite")).unwrap();
        equal(
            &state(&backup_package, &root, &fixture)["library.sqlite"],
            &fixture["before"]["library.sqlite"],
            "preupgrade backup",
        );
    }
    for (i, step) in fixture["steps"].as_array().unwrap().iter().enumerate() {
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
        let (status, mut payload, headers) =
            request(&origin, url, body, if step["bad_token"] == true { "bad" } else { &token });
        assert_eq!(json!(status), step["status"], "status {category} step {i} {url}");
        if category == "task_recovery" && body.is_some() {
            let start = Instant::now();
            loop {
                let (_, progress, _) = request(&origin, "/api/sync", None, &token);
                if progress["history"].as_array().unwrap().len() as i64 > previous_runs
                    && progress["history"][0]["status"].as_str().is_some_and(|s| s != "running")
                {
                    break;
                }
                assert!(start.elapsed() < Duration::from_secs(10), "recovery timeout");
                thread::sleep(Duration::from_millis(10));
            }
        }
        if status < 400 {
            if let Some(key) = step["projection"].as_str() {
                payload = payload[key].clone();
            }
            equal(&normalize(payload, &root, &fixture), &step["response"], &format!("{category}/{i}/HTTP"));
            equal(&Value::Object(headers), &step["headers"], &format!("{category}/{i}/headers"));
        }
        // Errors compare only status, but must also preserve the recorded DB invariants.
        equal(&state(&package, &root, &fixture), &step["state"], &format!("{category}/{i}/database"));
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
    let (_, _scratch, root, package) = prepare("http_api");
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
        let (mut opened, _, _) = launch(&root, &package, "benchmark", Path::new(&binary));
        opens.push(start.elapsed().as_secs_f64() * 1000.0);
        unsafe {
            libc::kill(opened.child.id() as i32, libc::SIGTERM);
        }
        assert!(opened.child.wait().unwrap().success());
        opened.reaped = true;
    }
    opens.sort_by(f64::total_cmp);
    let (mut host, origin, mut token) = launch(&root, &package, "benchmark", Path::new(&binary));
    assert_eq!(request(&origin, "/api/assets/count", None, &token).1["total"], 4096);
    let paths = [
        ("assets", "/api/assets?limit=100&offset=2000"),
        ("count", "/api/assets/count"),
        ("detail", "/api/detail?id=a"),
    ];
    let mut samples = Map::new();
    samples.insert("open".into(), json!(opens[3]));
    for (name, url) in paths {
        for _ in 0..5 {
            assert_eq!(request(&origin, url, None, &token).0, 200);
        }
        let start = Instant::now();
        for _ in 0..50 {
            assert_eq!(request(&origin, url, None, &token).0, 200);
        }
        samples.insert(name.into(), json!(start.elapsed().as_secs_f64() * 1000.0 / 50.0));
    }
    let mut tag_ms = 0.0;
    let mut undo_ms = 0.0;
    for _ in 0..20 {
        let start = Instant::now();
        let (status, result, _) =
            request(&origin, "/api/tag", Some(&json!({"ids":["a","b"],"tag":"benchmark synthetic"})), &token);
        tag_ms += start.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(status, 200);
        let start = Instant::now();
        assert_eq!(request(&origin, "/api/undo", Some(&json!({"id":result["undo"]["id"]})), &token).0, 200);
        undo_ms += start.elapsed().as_secs_f64() * 1000.0;
    }
    samples.insert("tag".into(), json!(tag_ms / 20.0));
    samples.insert("undo".into(), json!(undo_ms / 20.0));
    let mut switches = Vec::new();
    for i in 0..21 {
        let path = if i % 2 == 0 { &replacement } else { &package };
        let start = Instant::now();
        assert_eq!(request(&origin, "/api/libraries/open", Some(&json!({"path":path})), &token).0, 200);
        let (status, bootstrap, _) = request(&origin, "/api/bootstrap", None, &token);
        assert_eq!(status, 200);
        token = bootstrap["token"].as_str().unwrap().to_owned();
        if i > 0 {
            switches.push(start.elapsed().as_secs_f64() * 1000.0);
        }
    }
    switches.sort_by(f64::total_cmp);
    samples.insert("switch".into(), json!((switches[9] + switches[10]) / 2.0));
    host.child.kill().unwrap();
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    let mut status = 0;
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
