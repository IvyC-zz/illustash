use std::{path::Path, process::Command};
fn main() {
    println!("cargo:rerun-if-changed=src/research_compute/relation_fft.cpp");
    println!("cargo:rerun-if-changed=vendor/pocketfft/pocketfft_hdronly.h");
    cc::Build::new()
        .cpp(true)
        .std("c++11")
        .opt_level(3)
        // NumPy 2.5.3's top-level meson.build requires strict Clang FP behavior
        // for both C and C++; contraction changes PocketFFT's float32 bits.
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
    let build = run("git", &["rev-parse", "--short", "HEAD"]);
    let dirty = !run("git", &["status", "--porcelain", "--untracked-files=no"]).is_empty();
    println!("cargo:rustc-env=ILLUSTASH_BUILD={build}{}", if dirty { "-dirty" } else { "" });
    println!("cargo:rustc-env=ILLUSTASH_RUSTC={}", run("rustc", &["--version"]));
    let source = std::fs::read_to_string("application.json").expect("application metadata");
    let metadata: serde_json::Value = serde_json::from_str(&source).expect("application metadata JSON");
    for (key, env) in [("name", "ILLUSTASH_NAME"), ("version", "ILLUSTASH_VERSION"), ("channel", "ILLUSTASH_CHANNEL")] {
        let value = metadata[key].as_str().expect("metadata string");
        println!("cargo:rustc-env={env}={value}");
    }
}
