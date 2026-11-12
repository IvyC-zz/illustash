//! 经原生宿主端到端地测试打开前后的完整性检查（#212）：正常退出和崩溃；崩溃后在打开前发现损坏、正常打开后在后台
//! 发现、由每周的图库检查发现、由「立即检查」发现；发现之后不再写入损坏的图库包。只用故意覆写坏页的合成图库包。
#![allow(clippy::unwrap_used, reason = "集成测试里 unwrap 就是断言")]
use illustash_terms::{app_create::NewSource, app_service, store};
use rusqlite::Connection;
use serde_json::{json, Value};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

const RECORD: &str = ".illustash-checks.json";

#[path = "common/entry.rs"]
mod entry;

struct Sandbox(PathBuf);
impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// 按桌面外壳的方式运行的宿主：stdin 上有一道启动闸门，stdin 结束即正常退出。
struct Host {
    child: Child,
    stdin: Option<ChildStdin>,
    origin: String,
    /// stdout 的每一行，用来看启动进度。
    lines: Vec<String>,
}
impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Host {
    /// 在 `package` 上启动；宿主在就绪前退出时返回 Err，带上它的 stdout 和 stderr。
    fn launch(root: &Path, package: &Path) -> Result<Self, (Vec<String>, String)> {
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
            .env("ILLUSTASH_SHARED", root.join("shared"))
            .env("ILLUSTASH_DESKTOP_STDIN", "1");
        #[cfg(windows)]
        command.env("SystemRoot", std::env::var_os("SystemRoot").expect("SystemRoot"));
        let mut child = command
            .args(["http", "--no-sync", "--no-names", "--no-index-check", "--no-provenance", "--library"])
            .arg(package)
            .arg("--root")
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
            .args(["--port", &port.to_string()])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(b"1").unwrap();
        stdin.flush().unwrap();
        let (tx, rx) = mpsc::channel();
        let stdout = child.stdout.take().unwrap();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if tx.send(line.unwrap()).is_err() {
                    break;
                }
            }
        });
        let mut stderr = child.stderr.take().unwrap();
        let errors = thread::spawn(move || {
            let mut text = String::new();
            let _ = std::io::Read::read_to_string(&mut stderr, &mut text);
            text
        });
        let mut lines = Vec::new();
        let start = Instant::now();
        loop {
            match rx.recv_timeout(Duration::from_millis(50)) {
                Ok(line) => {
                    let ready = entry::address(&line).is_some();
                    if ready {
                        entry::enter(&line);
                    }
                    lines.push(line);
                    if ready {
                        break;
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    child.wait().unwrap();
                    return Err((lines, errors.join().unwrap()));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            assert!(start.elapsed() < Duration::from_secs(60), "host startup timeout: {lines:?}");
        }
        Ok(Self { child, stdin: Some(stdin), origin: format!("http://127.0.0.1:{port}"), lines })
    }
    fn get(&self, url: &str) -> Value {
        ureq::get(&format!("{}{url}", self.origin))
            .set("Cookie", &entry::cookie(&self.origin))
            .call()
            .unwrap()
            .into_json()
            .unwrap()
    }
    fn post(&self, url: &str, body: Value) -> (u16, Value) {
        let token = self.get("/api/bootstrap")["token"].as_str().unwrap().to_owned();
        let response = match ureq::post(&format!("{}{url}", self.origin))
            .set("Origin", &self.origin)
            .set("Cookie", &entry::cookie(&self.origin))
            .set("X-CSRF-Token", &token)
            .send_json(body)
        {
            Ok(r) | Err(ureq::Error::Status(_, r)) => r,
            Err(e) => panic!("HTTP {url}: {e}"),
        };
        (response.status(), response.into_json().unwrap())
    }
    /// 等后台检查结束（它在宿主就绪几秒后开始）。
    fn checked(&self) -> Value {
        let start = Instant::now();
        loop {
            let status = self.get("/api/integrity");
            if status["running"].is_null() {
                return status;
            }
            assert!(start.elapsed() < Duration::from_secs(60), "background check did not finish: {status}");
            thread::sleep(Duration::from_millis(100));
        }
    }
    /// 外壳的正常结束：管道关闭，宿主停止会话。
    fn quit(mut self) {
        drop(self.stdin.take());
        let start = Instant::now();
        while self.child.try_wait().unwrap().is_none() {
            assert!(start.elapsed() < Duration::from_secs(20), "host did not exit");
            thread::sleep(Duration::from_millis(20));
        }
    }
    /// 崩溃：没有机会关闭会话。
    fn crash(mut self) {
        self.child.kill().unwrap();
        self.child.wait().unwrap();
    }
    fn checking(&self) -> bool {
        self.lines.iter().any(|line| line.contains("检查图库数据"))
    }
}

fn record(package: &Path) -> Value {
    serde_json::from_slice(&fs::read(package.join(RECORD)).unwrap()).unwrap()
}
fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64
}

/// 在单独的表里放几页合成数据行，损坏这些页不影响打开时读取版本和表结构：只有完整检查才能发现。
fn bulk(db: &Path) {
    let c = Connection::open(db).unwrap();
    c.execute_batch(
        "CREATE TABLE synthetic_bulk(id INTEGER PRIMARY KEY, body TEXT); CREATE INDEX synthetic_bulk_body ON synthetic_bulk(body);
         WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<4000)
         INSERT INTO synthetic_bulk SELECT i, printf('%0100d', i) FROM n;",
    )
    .unwrap();
}
/// 先把 WAL 并回文件，再用垃圾数据覆写最后几页中的一页（属于 synthetic_bulk）。
fn corrupt(db: &Path) {
    let (size, count) = {
        let c = Connection::open(db).unwrap();
        c.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(())).unwrap();
        let size: u64 = c.query_row("PRAGMA page_size", [], |r| r.get(0)).unwrap();
        let count: u64 = c.query_row("PRAGMA page_count", [], |r| r.get(0)).unwrap();
        (size, count)
    };
    let mut bytes = fs::read(db).unwrap();
    let page = (count - 3) as usize;
    let start = page * size as usize;
    for (i, byte) in bytes[start..start + size as usize].iter_mut().enumerate() {
        *byte = (i as u8).wrapping_mul(37) ^ 0xA5;
    }
    fs::write(db, bytes).unwrap();
}
/// 两个数据库及其 WAL 文件，用来证明什么都没写。
fn files(package: &Path) -> Vec<(String, Option<Vec<u8>>)> {
    ["library.sqlite", "library.sqlite-wal", "source.sqlite", "source.sqlite-wal"]
        .into_iter()
        .map(|name| (name.to_owned(), fs::read(package.join(name)).ok()))
        .collect()
}

#[test]
fn checks_after_a_crash_in_the_background_and_on_request_and_stop_writing_on_damage() {
    let root =
        std::env::temp_dir().join(format!("illustash-integrity-{}-{}", std::process::id(), store::new_key().unwrap()));
    fs::create_dir_all(root.join("source")).unwrap();
    let _sandbox = Sandbox(root.clone());
    let root = illustash_platform::app_paths::canonical(&root).unwrap();
    for dir in ["home", "support", "shared"] {
        fs::create_dir_all(root.join(dir)).unwrap();
    }
    // 这个测试二进制只有一个测试：此期间没有别的代码读进程环境。
    std::env::set_var("ILLUSTASH_APP_SUPPORT", root.join("support"));
    std::env::set_var("ILLUSTASH_SHARED", root.join("shared"));
    std::env::set_var("ILLUSTASH_NO_KEYCHAIN", "1");
    let create = |name: &str| {
        let package = app_service::create(
            &root,
            &root.join(format!("{name}.pilib")),
            name,
            vec![NewSource::Folder { path: root.join("source"), preset: "other".into() }],
        )
        .unwrap();
        bulk(&package.join("source.sqlite"));
        bulk(&package.join("library.sqlite"));
        package
    };

    // 正常退出会把记录标为已关闭；崩溃则让它保持打开。这两次打开都不在就绪前检查。
    let crashed = create("crashed");
    let host = Host::launch(&root, &crashed).unwrap();
    assert!(!host.checking(), "{:?}", host.lines);
    assert_eq!(record(&crashed)["open"], true);
    host.quit();
    assert_eq!(record(&crashed)["open"], false);
    let host = Host::launch(&root, &crashed).unwrap();
    assert!(!host.checking(), "{:?}", host.lines);
    host.crash();
    assert_eq!(record(&crashed)["open"], true);
    // 崩溃损坏了 source.sqlite：下次打开时先检查两个数据库，拒绝打开图库包，也不改动它。
    corrupt(&crashed.join("source.sqlite"));
    let before = files(&crashed);
    let (lines, errors) = Host::launch(&root, &crashed).err().expect("a damaged package must not open");
    assert!(lines.iter().any(|l| l.contains("上次没有正常关闭，正在检查图库数据（library.sqlite）")), "{lines:?}");
    assert!(lines.iter().any(|l| l.contains("上次没有正常关闭，正在检查图库数据（source.sqlite）")), "{lines:?}");
    assert!(errors.contains("source.sqlite"), "{errors}");
    assert_eq!(record(&crashed)["damaged"]["database"], "source.sqlite");
    assert_eq!(files(&crashed), before);
    let note = illustash_terms::desktop_shell::damage_note(&crashed).expect("the shell explains the damage");
    assert!(note.contains("source.sqlite"));
    // 损坏一直留在记录里，所以在被替换之前，每次打开前都会重新检查这个图库包。
    assert!(Host::launch(&root, &crashed).is_err());
    assert_eq!(files(&crashed), before);

    // 正常打开时在后台检查 source.sqlite，发现损坏后不再接受写入。
    let damaged = create("damaged");
    corrupt(&damaged.join("source.sqlite"));
    let host = Host::launch(&root, &damaged).unwrap();
    assert!(!host.checking(), "{:?}", host.lines);
    let status = host.checked();
    assert_eq!(status["damaged"]["database"], "source.sqlite", "{status}");
    assert!(status["damaged"]["message"].as_str().unwrap().contains("source.sqlite"));
    let before = files(&damaged);
    for (url, body) in [
        ("/api/startup", json!({"sync": false})),
        ("/api/tag", json!({"ids": ["synthetic"], "tag": "synthetic", "on": true})),
        ("/api/integrity", json!({})),
    ] {
        let (code, reply) = host.post(url, body);
        assert_eq!(code, 409, "{url}: {reply}");
        assert_eq!(reply["error"], status["damaged"]["message"], "{url}");
    }
    assert_eq!(files(&damaged), before);
    host.quit();
    assert_eq!(record(&damaged)["damaged"]["database"], "source.sqlite");
    assert!(Host::launch(&root, &damaged).is_err());

    // library.sqlite 每周只在后台检查一次：它的损坏要等每周的检查，或者等「立即检查」才能发现。
    let weekly = create("weekly");
    corrupt(&weekly.join("library.sqlite"));
    let yesterday = now() - 86400;
    fs::write(weekly.join(RECORD), json!({"open": false, "checked": {"library.sqlite": yesterday}}).to_string())
        .unwrap();
    let host = Host::launch(&root, &weekly).unwrap();
    let status = host.checked();
    assert!(status["damaged"].is_null(), "{status}");
    assert_eq!(status["checked"]["library.sqlite"], yesterday);
    assert!(status["checked"]["source.sqlite"].as_i64().unwrap() >= now() - 60);
    let (code, started) = host.post("/api/integrity", json!({}));
    assert_eq!(code, 200, "{started}");
    let status = host.checked();
    assert_eq!(status["damaged"]["database"], "library.sqlite", "{status}");
    host.quit();
    let overdue = create("overdue");
    corrupt(&overdue.join("library.sqlite"));
    fs::write(
        overdue.join(RECORD),
        json!({"open": false, "checked": {"library.sqlite": now() - 8 * 86400}}).to_string(),
    )
    .unwrap();
    let host = Host::launch(&root, &overdue).unwrap();
    let status = host.checked();
    assert_eq!(status["damaged"]["database"], "library.sqlite", "{status}");
    let (code, reply) = host.post("/api/startup", json!({"sync": false}));
    assert_eq!(code, 409, "{reply}");
    host.crash();
    let (_, errors) = Host::launch(&root, &overdue).err().expect("damaged library.sqlite must not open");
    assert!(errors.contains("library.sqlite"), "{errors}");

    // source.sqlite 用回滚日志：后台检查运行期间，使用应用里最短忙等超时（250 ms）的写入方仍能提交；
    // 检查会让路，等写入方完成后重新开始。
    let busy = create("busy");
    {
        let c = Connection::open(busy.join("source.sqlite")).unwrap();
        c.execute_batch(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<400000)
             INSERT INTO synthetic_bulk(body) SELECT printf('%0100d-%d', i, i) || hex(randomblob(450)) FROM n;",
        )
        .unwrap();
    }
    let host = Host::launch(&root, &busy).unwrap();
    let start = Instant::now();
    loop {
        let status = host.get("/api/integrity");
        if status["running"]["database"] == "source.sqlite" && status["running"]["done"].as_f64().unwrap() > 0.05 {
            break;
        }
        assert!(status["running"].is_object(), "the check ended before a writer could meet it: {status}");
        assert!(start.elapsed() < Duration::from_secs(60), "{status}");
        thread::sleep(Duration::from_millis(20));
    }
    let writer = Connection::open(busy.join("source.sqlite")).unwrap();
    writer.busy_timeout(Duration::from_millis(250)).unwrap();
    writer
        .execute_batch("BEGIN IMMEDIATE; INSERT INTO synthetic_bulk(body) VALUES ('written during the check'); COMMIT;")
        .unwrap();
    drop(writer);
    let status = host.checked();
    assert!(status["damaged"].is_null(), "{status}");
    assert!(status["error"].is_null(), "{status}");
    assert!(status["checked"]["source.sqlite"].as_i64().unwrap() >= now() - 120, "{status}");
    host.quit();
}
