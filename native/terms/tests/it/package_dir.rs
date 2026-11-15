//! 目录形式的证据包：夹具、文件核对、符号链接和导入。只用合成数据。
#![allow(clippy::unwrap_used, reason = "集成测试里 unwrap 就是断言")]

use illustash_terms::package::{Package, Status};
use illustash_terms::store::{import_package, register_asset, state, term_by_link};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/evidence")
}

/// 夹具副本目录；和其他测试的临时目录一样，离开作用域（含断言失败）即删除。
struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
impl std::ops::Deref for Scratch {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

fn scratch(name: &str) -> Scratch {
    let dir = Scratch(std::env::temp_dir().join(format!("illustash-terms-{}-{name}", std::process::id())));
    let _ = fs::remove_dir_all(&dir.0);
    fs::create_dir_all(&dir.0).unwrap();
    for f in ["manifest.json", "evidence.jsonl", "notes.txt"] {
        fs::copy(fixture().join(f), dir.join(f)).unwrap();
    }
    dir
}

#[test]
fn the_fixture_package_validates_and_imports() {
    let _env = crate::shared();
    let p = Package::open(&fixture()).unwrap();
    assert_eq!(p.rows.len(), 4);
    assert_eq!(p.manifest.assets[2].status, Status::Failed);
    let mut c = illustash_terms::open(Path::new(":memory:")).unwrap();
    illustash_terms::migrate(&mut c).unwrap();
    for id in ["synthetic-asset-1", "synthetic-asset-2", "synthetic-asset-3"] {
        register_asset(&c, "photos", id, None).unwrap();
    }
    let out = import_package(&mut c, &p, "synthetic-library").unwrap();
    assert_eq!((out.added, out.created_terms), (4, 4));
    let artist = term_by_link(&c, "danbooru", "synthetic_artist").unwrap().unwrap();
    assert_eq!(state(&c, 1, artist).unwrap().as_deref(), Some("machine"));
}

#[test]
fn file_checks() {
    let _env = crate::shared();
    let dir = scratch("tamper");
    fs::write(dir.join("notes.txt"), b"changed").unwrap();
    assert!(Package::open(&dir).unwrap_err().to_string().contains("notes.txt"));

    let dir = scratch("symlink");
    fs::remove_file(dir.join("notes.txt")).unwrap();
    #[cfg(unix)]
    let linked = {
        std::os::unix::fs::symlink(fixture().join("notes.txt"), dir.join("notes.txt")).unwrap();
        true
    };
    // Windows 创建符号链接要开发者模式或管理员权限；没有权限时只跳过这一项。
    #[cfg(windows)]
    let linked = std::os::windows::fs::symlink_file(fixture().join("notes.txt"), dir.join("notes.txt")).is_ok();
    if linked {
        assert!(Package::open(&dir).unwrap_err().to_string().contains("不是普通文件"));
    }

    let dir = scratch("missing");
    fs::remove_file(dir.join("notes.txt")).unwrap();
    assert!(Package::open(&dir).is_err());

    // 未声明的文件不读取。
    let dir = scratch("extra");
    fs::write(dir.join("unrelated.bin"), b"ignored").unwrap();
    assert!(Package::open(&dir).is_ok());
}

#[test]
fn cli_validates_with_exit_codes() {
    let _env = crate::shared();
    let bin = env!("CARGO_BIN_EXE_illustash-terms");
    let ok = Command::new(bin).args(["validate", fixture().to_str().unwrap()]).output().unwrap();
    assert!(ok.status.success());
    let summary: serde_json::Value = serde_json::from_slice(&ok.stdout).unwrap();
    assert_eq!((summary["evidence"].as_u64(), summary["completed"].as_u64()), (Some(4), Some(2)));
    let dir = scratch("cli");
    fs::write(dir.join("evidence.jsonl"), b"{}\n").unwrap();
    let bad = Command::new(bin).args(["validate", dir.to_str().unwrap()]).output().unwrap();
    assert_eq!(bad.status.code(), Some(2));
    assert!(bad.stdout.is_empty() && !bad.stderr.is_empty());
}

#[test]
fn cli_package_descriptions_do_not_export_secret_urls_or_claim_human_names() {
    use illustash_core::human_conclusions::{
        export,
        format::{Header, Policy},
    };
    use serde_json::{json, Value};
    use std::io::Write;
    use std::process::Stdio;
    let _env = crate::shared();
    let dir = scratch("human-export");
    let evidence = fs::read_to_string(dir.join("evidence.jsonl")).unwrap().replace("\"photos\"", "\"library\"");
    let descriptions = format!(
        "{}\n",
        json!({"term":{"kind":"artist","link":{"system":"danbooru","key":"synthetic_artist"}},
        "links":[{"system":"pixiv","key":"1234","url":"https://www.pixiv.net/users/1234?api_key=synthetic"}],
        "names":[{"lang":"en","value":"Package Claimed User","source":"user"}]})
    );
    let mut manifest: Value = serde_json::from_slice(&fs::read(dir.join("manifest.json")).unwrap()).unwrap();
    for asset in manifest["assets"].as_array_mut().unwrap() {
        asset["asset"]["namespace"] = json!("library");
    }
    for (name, data) in [("evidence.jsonl", evidence.as_bytes()), ("terms.jsonl", descriptions.as_bytes())] {
        fs::write(dir.join(name), data).unwrap();
        manifest["files"][name] = json!({"bytes":data.len(),"sha256":illustash_core::strict_json::sha256_hex(data)});
    }
    fs::write(dir.join("manifest.json"), serde_json::to_vec(&manifest).unwrap()).unwrap();
    let path = dir.join("library.sqlite");
    let mut c = illustash_terms::open(&path).unwrap();
    illustash_terms::migrate(&mut c).unwrap();
    c.execute_batch(include_str!("../../src/app_create_schema.sql")).unwrap();
    c.execute("INSERT INTO assets(uuid) VALUES ('synthetic-asset-1')", []).unwrap();
    drop(c);
    let bin = env!("CARGO_BIN_EXE_illustash-terms");
    let imported = Command::new(bin)
        .args(["import", "--db"])
        .arg(&path)
        .args(["--library-id", "synthetic-library", "--package"])
        .arg(&dir.0)
        .arg("--new-assets")
        .output()
        .unwrap();
    assert!(imported.status.success(), "{}", String::from_utf8_lossy(&imported.stderr));
    let mut c = illustash_terms::open(&path).unwrap();
    let term:String=c.query_row("SELECT t.key FROM terms t JOIN term_links l ON l.term_id=t.id WHERE l.system='danbooru' AND l.key='synthetic_artist'",[],|r|r.get(0)).unwrap();
    assert!(c
        .query_row("SELECT url FROM term_links WHERE system='pixiv'", [], |r| r.get::<_, String>(0))
        .unwrap()
        .contains("api_key=synthetic"));
    let mut judge = Command::new(bin)
        .args(["judge", "--db"])
        .arg(&path)
        .args(["--term", &term, "--verdict", "yes", "--label", "合成裁决"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    judge.stdin.take().unwrap().write_all(b"[\"synthetic-asset-1\"]").unwrap();
    let judged = judge.wait_with_output().unwrap();
    assert!(judged.status.success(), "{}", String::from_utf8_lossy(&judged.stderr));
    let tx = c.transaction().unwrap();
    let snapshot = export::capture(
        &tx,
        Header::new(
            "synthetic-snapshot".into(),
            "synthetic-library".into(),
            "2026-11-15T00:00:00Z".into(),
            Policy::Default,
        ),
    )
    .unwrap();
    let mut wire = Vec::new();
    snapshot.write(&mut wire).unwrap();
    let wire = String::from_utf8(wire).unwrap();
    assert!(!wire.contains("api_key"));
    assert!(!wire.contains("https://www.pixiv.net"));
    let records: Vec<Value> = wire.lines().map(|line| serde_json::from_str(line).unwrap()).collect();
    let entity = records.iter().find(|r| r["type"] == "entity" && r["record_key"] == term).unwrap();
    let name = entity["definition"]["names"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["value"] == "Package Claimed User")
        .unwrap();
    assert_eq!(name["provenance"], "legacy_unknown");
    assert_eq!(name["preferred"], false);
}
