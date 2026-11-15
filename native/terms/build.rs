use std::{path::Path, process::Command};
fn main() {
    println!("cargo:rerun-if-changed=src/research_compute/relation_fft.cpp");
    println!("cargo:rerun-if-changed=vendor/pocketfft/pocketfft_hdronly.h");
    println!("cargo:rerun-if-changed=application.json");
    println!("cargo:rerun-if-changed=Cargo.lock");
    println!("cargo:rerun-if-changed=src");
    // 仓库根目录用绝对路径：git 返回的相对路径要拼到它上面再交给 cargo，
    // cargo 会把 rerun-if-changed 里的相对路径按包目录（native/terms）解析（#327）。
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR for build scripts");
    let root = Path::new(&manifest).ancestors().nth(2).expect("native/terms is two levels below the repository root");
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
    // 提交变了构建号要跟着变，所以监视 HEAD 和它指向的分支 ref；不变时不能重跑。cargo 把不存在的路径
    // 当成每次都过期，所以只输出存在的路径（#327）。普通检出里 `--git-path` 返回相对路径，worktree 里
    // 返回绝对路径，HEAD 在 worktree 自己的 git 目录，分支 ref 在共用的 git 目录。
    let git_path = |key: &str| {
        let path = run("git", &["rev-parse", "--git-path", key]);
        (!path.is_empty()).then(|| root.join(path))
    };
    let mut watched = vec![git_path("HEAD")];
    // detached HEAD 时 symbolic-ref 是空串，`--git-path ""` 会给出整个 git 目录，任何 git 操作都会让它变。
    let branch = run("git", &["symbolic-ref", "-q", "HEAD"]);
    if !branch.is_empty() {
        // 分支 ref 被 pack 进 packed-refs 后没有单独的文件，下一次提交才会重新写出来：这时监视离它
        // 最近的、存在的上级目录（不超出 refs/），新文件出现时目录会变。packed-refs 本身不监视：
        // 更新分支总是写单独的文件，packed-refs 只在打包、删分支时改写，不改变当前分支指向的提交。
        if let Some(path) = git_path(&branch) {
            let refs = git_path("refs");
            watched.push(path.ancestors().find(|p| p.exists() || refs.as_deref() == Some(*p)).map(Path::to_path_buf));
        }
    }
    for path in watched.into_iter().flatten().filter(|p| p.exists()) {
        println!("cargo:rerun-if-changed={}", path.display());
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
