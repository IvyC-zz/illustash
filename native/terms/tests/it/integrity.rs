//! 经原生宿主端到端地测试打开前后的完整性检查（#212）：正常退出和崩溃；崩溃后在打开前发现损坏、正常打开后在后台
//! 发现、由每周的图库检查发现、由「立即检查」发现；发现之后不再写入损坏的图库包。只用故意覆写坏页的合成图库包。
#![allow(clippy::unwrap_used, reason = "集成测试里 unwrap 就是断言")]
use illustash_terms::{app_create::NewSource, app_service, store};
use rusqlite::{auto_extension, ffi, functions::FunctionFlags, Connection, ErrorCode};
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

use crate::common::entry;

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
        Self::launch_with_gate(root, package, false)
    }
    fn launch_with_gate(root: &Path, package: &Path, gated: bool) -> Result<Self, (Vec<String>, String)> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let mut command = Command::new(env!("CARGO_BIN_EXE_illustash-terms"));
        command
            .args(["http", "--no-sync", "--no-names", "--no-index-check", "--no-provenance", "--library"])
            .arg(package)
            .arg("--root")
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
            .args(["--port", &port.to_string()]);
        if gated {
            let args: Vec<_> = command.get_args().map(|arg| arg.to_str().unwrap()).collect();
            let args = serde_json::to_string(&args).unwrap();
            command = Command::new(std::env::current_exe().unwrap());
            command.env_clear().env("ILLUSTASH_TEST_HOST_ARGS", args);
            command.args(["--ignored", "--exact", "integrity::writer_host", "--nocapture"]);
        } else {
            command.env_clear();
        }
        command
            .env("HOME", root.join("home"))
            .env("USERPROFILE", root.join("home"))
            .env("ILLUSTASH_APP_SUPPORT", root.join("support"))
            .env("ILLUSTASH_NO_KEYCHAIN", "1")
            .env("ILLUSTASH_SHARED", root.join("shared"))
            .env("ILLUSTASH_DESKTOP_STDIN", "1");
        #[cfg(windows)]
        command.env("SystemRoot", std::env::var_os("SystemRoot").expect("SystemRoot"));
        let mut child = command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
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
            assert!(start.elapsed() < crate::common::wait::limit(), "host startup timeout: {lines:?}");
        }
        Ok(Self { child, stdin: Some(stdin), origin: format!("http://127.0.0.1:{port}"), lines })
    }
    /// 失败时 panic 信息里带上状态码和回答正文，或连接错误本身。
    fn get(&self, url: &str) -> Value {
        match ureq::get(&format!("{}{url}", self.origin)).set("Cookie", &entry::cookie(&self.origin)).call() {
            Ok(response) => response.into_json().unwrap_or_else(|e| panic!("GET {url}: unreadable reply: {e}")),
            Err(ureq::Error::Status(code, response)) => {
                panic!("GET {url}: HTTP {code}: {}", response.into_string().unwrap_or_default())
            }
            Err(e) => panic!("GET {url}: {e}"),
        }
    }
    /// 同步点必须在检查期间到达。若检查提前结束，直接给出完整性状态。
    fn wait_for_read(&self, marker: &Path) -> Value {
        let deadline = crate::common::wait::deadline();
        loop {
            let at_marker = marker.is_file();
            let status = self.get("/api/integrity");
            if at_marker {
                return status;
            }
            assert!(status["running"].is_object() && Instant::now() < deadline, "未到达 {marker:?}：{status}");
            thread::sleep(Duration::from_millis(20));
        }
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
    /// 等后台检查结束。进度变化时重新计算停滞时长，总时长另设上限，避免反复让路时一直等下去。
    fn checked(&self) -> Value {
        let start = Instant::now();
        let (mut seen, mut moved) = (Value::Null, Instant::now());
        loop {
            let status = self.get("/api/integrity");
            if status["running"].is_null() {
                return status;
            }
            if status["running"] != seen {
                (seen, moved) = (status["running"].clone(), Instant::now());
            }
            assert!(moved.elapsed() < crate::common::wait::limit(), "background check stalled: {status}");
            assert!(start.elapsed() < crate::common::wait::limit() * 10, "background check did not finish: {status}");
            thread::sleep(Duration::from_millis(100));
        }
    }
    /// 外壳的正常结束：管道关闭，宿主停止会话。
    fn quit(mut self) {
        drop(self.stdin.take());
        let start = Instant::now();
        while self.child.try_wait().unwrap().is_none() {
            assert!(start.elapsed() < crate::common::wait::limit(), "host did not exit");
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
    let _env = crate::exclusive();
    let root =
        std::env::temp_dir().join(format!("illustash-integrity-{}-{}", std::process::id(), store::new_key().unwrap()));
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

    // quick_check 在只读事务中验证虚拟列的 NOT NULL 时会求值。首轮读到 0，重查才能读到已提交的 1。
    let busy = create("busy");
    let writer = Connection::open(busy.join("source.sqlite")).unwrap();
    writer.create_scalar_function("writer_gate", 1, GATE_FLAGS, |_| Ok(true)).unwrap();
    // 标记行后面还有普通行，解除同步点后 SQLite 会再次调用产品的进度处理函数，检查写入日志。
    writer
        .execute_batch(
            "CREATE TABLE check_gate(phase INTEGER, checked AS
                 (CASE WHEN phase >= 0 THEN writer_gate(phase) ELSE 1 END) VIRTUAL NOT NULL);
             INSERT INTO check_gate VALUES (0);
             WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<1000)
             INSERT INTO check_gate SELECT -i FROM n;",
        )
        .unwrap();
    let host = Host::launch_with_gate(&root, &busy, true).unwrap();
    let gates = root.join("support");
    let status = host.wait_for_read(&gates.join("reading-0"));
    assert_eq!(status["running"]["database"], "source.sqlite", "{status}");
    assert!(status["running"]["done"].as_f64().unwrap() > 0.05, "{status}");

    // 保持首轮读锁时，回滚日志已落盘，写事务仍未提交。用同一个连接和应用的 250 ms 忙等重试 COMMIT。
    writer.busy_timeout(Duration::from_millis(250)).unwrap();
    writer.execute_batch("BEGIN IMMEDIATE; UPDATE check_gate SET phase=1 WHERE phase=0;").unwrap();
    let blocked = writer.execute_batch("COMMIT").unwrap_err();
    assert_eq!(blocked.sqlite_error_code(), Some(ErrorCode::DatabaseBusy), "{blocked}");
    fs::write(gates.join("continue-0"), b"").unwrap();
    writer.execute_batch("COMMIT").expect("完整性检查应给写入方让路");
    drop(writer);

    // 首轮快照看不到 1。只有检查让路并重新打开数据库，才会进入这个同步点。取样后才允许重查完成。
    let status = host.wait_for_read(&gates.join("reading-1"));
    assert_eq!(status["running"]["database"], "source.sqlite", "{status}");
    assert!(status["checked"]["source.sqlite"].is_null(), "{status}");
    fs::write(gates.join("continue-1"), b"").unwrap();
    let status = host.checked();
    assert!(status["damaged"].is_null(), "{status}");
    assert!(status["error"].is_null(), "{status}");
    assert!(status["checked"]["source.sqlite"].as_i64().unwrap() >= now() - 120, "{status}");
    host.quit();
}

// 函数的 SQL 结果恒为 true；同步文件只在独立宿主的合成目录中使用。
const GATE_FLAGS: FunctionFlags =
    FunctionFlags::SQLITE_UTF8.union(FunctionFlags::SQLITE_INNOCUOUS).union(FunctionFlags::SQLITE_DETERMINISTIC);

fn wait_for(path: &Path) {
    let deadline = crate::common::wait::deadline();
    while !path.exists() {
        assert!(Instant::now() < deadline, "未到达完整性检查同步点 {path:?}");
        thread::sleep(Duration::from_millis(5));
    }
}

// SQLite 给每个宿主连接注册虚拟列用的函数。只有合成表的标记行会调用它。
unsafe extern "C" fn install_gate(
    db: *mut ffi::sqlite3,
    error: *mut *mut std::ffi::c_char,
    _: *const ffi::sqlite3_api_routines,
) -> std::ffi::c_int {
    // SAFETY: SQLite 传入本次初始化的有效指针。回调只注册函数，不开关连接，也不修改扩展列表。
    unsafe {
        auto_extension::init_auto_extension(db, error, |conn| {
            let gates = PathBuf::from(std::env::var_os("ILLUSTASH_APP_SUPPORT").unwrap());
            conn.create_scalar_function("writer_gate", 1, GATE_FLAGS, move |ctx| {
                let phase: i64 = ctx.get(0)?;
                fs::write(gates.join(format!("reading-{phase}")), b"").unwrap();
                wait_for(&gates.join(format!("continue-{phase}")));
                Ok(true)
            })
        })
    }
}

#[test]
#[ignore = "由并发写入用例作为独立的原生 HTTP 宿主启动"]
fn writer_host() {
    let _env = crate::exclusive();
    let args: Vec<String> = serde_json::from_str(&std::env::var("ILLUSTASH_TEST_HOST_ARGS").unwrap()).unwrap();
    // SAFETY: install_gate 只在这个测试子进程中注册，满足 SQLite 自动扩展的初始化约束。
    unsafe { auto_extension::register_auto_extension(install_gate) }.unwrap();
    assert_eq!(illustash_terms::cli::run(&args), 0);
}
