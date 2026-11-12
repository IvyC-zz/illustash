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
