//! 经原生宿主端到端地抢救损坏的 source.sqlite（#228）：图库同步并整理过，图片行和文件夹行所在的页被覆写破坏，
//! 然后分别从图库页面、桌面外壳的 `--rescue-source` 和图库切换处抢救。每张图片都保留原来的 ID，
//! 整理结果也就跟着它。只用合成图片和合成图库包。
#![allow(clippy::unwrap_used, reason = "集成测试里 unwrap 就是断言")]
use illustash_terms::{app_create::NewSource, app_service, store};
use rusqlite::Connection;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
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
    lines: Vec<String>,
}
impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Host {
    fn launch(root: &Path, package: &Path, extra: &[&str]) -> Result<Self, (Vec<String>, String)> {
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
            .args(["http", "--no-names", "--no-index-check", "--no-provenance", "--library"])
            .arg(package)
            .arg("--root")
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
            .args(["--port", &port.to_string()])
            .args(extra)
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
            assert!(start.elapsed() < Duration::from_secs(120), "host startup timeout: {lines:?}");
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
    /// 一直轮询 `GET /api/integrity`，直到 `done` 判定满足。
    fn integrity_until(&self, what: &str, done: impl Fn(&Value) -> bool) -> Value {
        let start = Instant::now();
        loop {
            let status = self.get("/api/integrity");
            if done(&status) {
                return status;
            }
            assert!(start.elapsed() < Duration::from_secs(120), "{what}: {status}");
            thread::sleep(Duration::from_millis(100));
        }
    }
    fn quit(mut self) {
        drop(self.stdin.take());
        let start = Instant::now();
        while self.child.try_wait().unwrap().is_none() {
            assert!(start.elapsed() < Duration::from_secs(20), "host did not exit");
            thread::sleep(Duration::from_millis(20));
        }
    }
}

fn record(package: &Path) -> Value {
    serde_json::from_slice(&fs::read(package.join(RECORD)).unwrap()).unwrap()
}
fn picture(path: &Path, shade: u8) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    image::RgbImage::from_pixel(5, 4, image::Rgb([shade, 40, 90])).save(path).unwrap();
}
/// 图库包 source.sqlite 里的相对路径 → ID。
fn ids(package: &Path) -> BTreeMap<String, String> {
    let c = Connection::open(package.join("source.sqlite")).unwrap();
    let ids = c
        .prepare("SELECT path,asset FROM files")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    ids
}
fn count(db: &Path, sql: &str) -> i64 {
    Connection::open(db).unwrap().query_row(sql, [], |r| r.get(0)).unwrap()
}
/// 覆写一个小 source.sqlite 中存放图片行和文件夹行的页。
fn destroy_rows(source: &Path) {
    let (size, pages): (u64, Vec<u64>) = {
        let c = Connection::open(source).unwrap();
        let size = c.query_row("PRAGMA page_size", [], |r| r.get(0)).unwrap();
        let pages = c
            .prepare("SELECT rootpage FROM sqlite_master WHERE name IN ('assets','files')")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        (size, pages)
    };
    let mut bytes = fs::read(source).unwrap();
    for page in pages {
        let start = ((page - 1) * size) as usize;
        for (i, byte) in bytes[start..start + size as usize].iter_mut().enumerate() {
            *byte = (i as u8).wrapping_mul(37) ^ 0xA5;
        }
    }
    fs::write(source, bytes).unwrap();
}

/// 建在自己文件夹上、已同步并整理过的图库：记下各图片的 ID 和带标签的那张图片。
struct Organized {
    package: PathBuf,
    folder: PathBuf,
    ids: BTreeMap<String, String>,
    tagged: String,
}
fn organized(root: &Path, name: &str) -> Organized {
    let folder = root.join(format!("{name}-pictures"));
    for (i, rel) in ["a.png", "b.png", "sub/c.png", "sub/d.png"].iter().enumerate() {
        picture(&folder.join(rel), 30 * i as u8 + 10);
    }
    let package = app_service::create(
        root,
        &root.join(format!("{name}.pilib")),
        name,
        vec![NewSource::Folder { path: folder.clone(), preset: "other".into() }],
    )
    .unwrap();
    // 打开时会同步：图片得到 ID，library.sqlite 记下它们。
    let host = Host::launch(root, &package, &[]).unwrap();
    let start = Instant::now();
    while count(&package.join("library.sqlite"), "SELECT count(*) FROM assets") < 4 {
        assert!(start.elapsed() < Duration::from_secs(60), "the first sync did not finish");
        thread::sleep(Duration::from_millis(100));
    }
    let ids = ids(&package);
    let tagged = ids["sub/c.png"].clone();
    let (code, reply) = host.post("/api/tag", json!({"ids": [tagged], "tag": "合成标签"}));
    assert_eq!(code, 200, "{reply}");
    host.quit();
    // 造成损坏：source.sqlite 的图片行和文件夹行都读不出来。与此同时 c 移到了另一个文件夹。
    destroy_rows(&package.join("source.sqlite"));
    fs::create_dir_all(folder.join("moved")).unwrap();
    fs::rename(folder.join("sub/c.png"), folder.join("moved/c.png")).unwrap();
    Organized { package, folder, ids, tagged }
}
/// 抢救之后：每张图片都是原来的 ID（c 按内容在新路径上找到），标签仍指向图库里的图片，
/// 损坏的文件保留了下来，检查记录已清空。
fn rescued(o: &Organized) {
    let mut expected = o.ids.clone();
    let c = expected.remove("sub/c.png").unwrap();
    expected.insert("moved/c.png".into(), c);
    assert_eq!(ids(&o.package), expected);
    let source = o.package.join("source.sqlite");
    assert_eq!(count(&source, "SELECT count(*) FROM assets WHERE coalesce(trashed,0)=0"), 4);
    let tag = count(&o.package.join("library.sqlite"), "SELECT count(*) FROM asset_tags WHERE tag='合成标签'");
    assert_eq!(tag, 1);
    let tagged = format!("SELECT count(*) FROM assets WHERE uuid='{}' AND src_path LIKE '%moved%'", o.tagged);
    assert_eq!(count(&source, &tagged), 1);
    let kept = fs::read_dir(o.package.join("backups"))
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with("source-damaged-"))
        .count();
    assert_eq!(kept, 1);
    assert!(record(&o.package)["damaged"].is_null());
    assert!(o.folder.join("moved/c.png").is_file());
}

#[test]
fn a_damaged_source_is_rescued_and_every_picture_keeps_its_id() {
    let _env = crate::exclusive();
    let root =
        std::env::temp_dir().join(format!("illustash-rescue-{}-{}", std::process::id(), store::new_key().unwrap()));
    fs::create_dir_all(&root).unwrap();
    let _sandbox = Sandbox(root.clone());
    let root = illustash_platform::app_paths::canonical(&root).unwrap();
    for dir in ["home", "support", "shared"] {
        fs::create_dir_all(root.join(dir)).unwrap();
    }
    // 持有 `crate::exclusive()`：此期间没有别的测试在跑，也就没有别的代码读进程环境。
    std::env::set_var("ILLUSTASH_APP_SUPPORT", root.join("support"));
    std::env::set_var("ILLUSTASH_SHARED", root.join("shared"));
    std::env::set_var("ILLUSTASH_NO_KEYCHAIN", "1");

    // 从图库页面：后台检查发现损坏，「抢救重建」在会话保持停止的状态下建出新文件，再次打开图库时让它就位。
    let page = organized(&root, "page");
    let host = Host::launch(&root, &page.package, &["--no-sync"]).unwrap();
    let status = host.integrity_until("background check", |s| !s["damaged"].is_null());
    assert_eq!(status["damaged"]["database"], "source.sqlite");
    assert!(status["damaged"]["message"].as_str().unwrap().contains("抢救重建"));
    let (code, started) = host.post("/api/integrity/rescue", json!({}));
    assert_eq!(code, 200, "{started}");
    host.integrity_until("rescue", |s| s["rescue"]["ready"] == true || !s["rescue"]["error"].is_null());
    assert!(host.get("/api/integrity")["rescue"]["error"].is_null());
    // 新文件就位之前仍是停止状态。
    let (code, _) = host.post("/api/startup", json!({"sync": false}));
    assert_eq!(code, 409);
    let (code, reopened) = host.post("/api/libraries/open", json!({"path": page.package}));
    assert_eq!(code, 200, "{reopened}");
    assert_eq!(reopened["rescued"]["kept"], 4, "{reopened}");
    assert_eq!(host.get("/api/libraries")["rescued"]["kept"], 4);
    assert!(host.get("/api/integrity")["damaged"].is_null());
    let (code, reply) = host.post("/api/startup", json!({"sync": false}));
    assert_eq!(code, 200, "{reply}");
    host.quit();
    rescued(&page);

    // 从桌面外壳：检查发现损坏后，用 --rescue-source 重新启动服务，它像检查一样报告各个阶段，然后打开图库。
    let shell = organized(&root, "shell");
    let host = Host::launch(&root, &shell.package, &["--no-sync"]).unwrap();
    host.integrity_until("background check", |s| !s["damaged"].is_null());
    host.quit();
    assert!(Host::launch(&root, &shell.package, &["--no-sync"]).is_err(), "damage on record refuses the package");
    assert!(illustash_terms::desktop_shell::damage_rescuable(&shell.package));
    let host = Host::launch(&root, &shell.package, &["--no-sync", "--rescue-source"]).unwrap();
    assert!(host.lines.iter().any(|l| l.contains("正在抢救识别结果数据库")), "{:?}", host.lines);
    assert_eq!(host.get("/api/libraries")["rescued"]["kept"], 4);
    host.quit();
    rescued(&shell);
    assert!(!illustash_terms::desktop_shell::damage_rescuable(&shell.package));

    // 从图库切换处：打开另一个损坏的图库时会提示损坏；同意后先抢救再打开。
    let switched = organized(&root, "switched");
    let host = Host::launch(&root, &switched.package, &["--no-sync"]).unwrap();
    host.integrity_until("background check", |s| !s["damaged"].is_null());
    host.quit();
    let host = Host::launch(&root, &page.package, &["--no-sync"]).unwrap();
    let (code, refused) = host.post("/api/libraries/open", json!({"path": switched.package}));
    assert_eq!(code, 409, "{refused}");
    assert!(refused["error"].as_str().unwrap().contains("抢救重建"));
    let (code, opened) = host.post("/api/libraries/open", json!({"path": switched.package, "rescue": true}));
    assert_eq!(code, 200, "{opened}");
    assert_eq!(opened["rescued"]["kept"], 4);
    host.quit();
    rescued(&switched);
}
