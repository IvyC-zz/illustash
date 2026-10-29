use std::{path::Path, process::Command};
fn main() {
    println!("cargo:rerun-if-changed=src/research_compute/relation_fft.cpp");
    println!("cargo:rerun-if-changed=vendor/pocketfft/pocketfft_hdronly.h");
    cc::Build::new()
        .cpp(true)
        .std("c++11")
        .opt_level(3)
        // NumPy 2.5.3 的顶层 meson.build 要求 C 和 C++ 都使用严格的 Clang 浮点行为；
        // 浮点收缩（contraction）会改变 PocketFFT 的 float32 结果位。
        .flag_if_supported("-ftrapping-math")
        .file("src/research_compute/relation_fft.cpp")
        .include("vendor/pocketfft")
        .compile("illustash_relation_fft");
    println!("cargo:rerun-if-changed=application.json");
    println!("cargo:rerun-if-changed=Cargo.lock");
    println!("cargo:rerun-if-changed=src");
    let root = Path::new("../..");
    let run = |program: &str, args: &[&str]| {
        Command::new(program)
            .args(args)
            .current_dir(root)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
            .unwrap_or_default()
    };
    for key in ["HEAD", &run("git", &["symbolic-ref", "-q", "HEAD"])] {
        let path = run("git", &["rev-parse", "--git-path", key]);
        if !path.is_empty() {
            println!("cargo:rerun-if-changed={path}");
        }
    }
    println!("cargo:rerun-if-changed=source-revision.txt");
    let mut build = run("git", &["rev-parse", "--short", "HEAD"]);
    let dirty = !run("git", &["status", "--porcelain", "--untracked-files=no"]).is_empty();
    if build.is_empty() {
        // `git archive` 导出的源码没有 .git；export-subst 把导出的版本写进了这个文件。
        // 在检出的仓库里它仍是 `$Format:%h$`，不是哈希，会被忽略。
        let exported = std::fs::read_to_string("source-revision.txt").unwrap_or_default();
        let exported = exported.trim();
        if (7..=40).contains(&exported.len()) && exported.bytes().all(|b| b.is_ascii_hexdigit()) {
            build = exported.to_owned();
        }
    }
    println!("cargo:rustc-env=ILLUSTASH_BUILD={build}{}", if dirty { "-dirty" } else { "" });
    println!("cargo:rustc-env=ILLUSTASH_RUSTC={}", run("rustc", &["--version"]));
    let source = std::fs::read_to_string("application.json").expect("application metadata");
    let metadata: serde_json::Value = serde_json::from_str(&source).expect("application metadata JSON");
    for (key, env) in [("name", "ILLUSTASH_NAME"), ("version", "ILLUSTASH_VERSION"), ("channel", "ILLUSTASH_CHANNEL")] {
        let value = metadata[key].as_str().expect("metadata string");
        println!("cargo:rustc-env={env}={value}");
    }
}
