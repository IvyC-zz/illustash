//! 平台路径边界：用户目录、应用数据目录和普通路径解析。
//! SQLite Windows 长路径/VFS 由 sqlite 模块独立封装。
use crate::{Error, Result};
use std::{
    collections::HashSet,
    ffi::OsString,
    fs,
    path::{Component, Path, PathBuf},
};

pub fn user_home() -> Result<PathBuf> {
    user_home_with(|key| std::env::var_os(key))
}
fn user_home_with(env: impl Fn(&str) -> Option<OsString>) -> Result<PathBuf> {
    #[cfg(windows)]
    let home = env("USERPROFILE");
    #[cfg(not(windows))]
    let home = env("HOME");
    home.filter(|v| !v.is_empty()).map(PathBuf::from).ok_or_else(|| Error::Invalid("无法定位用户主目录".into()))
}
/// 显式 ILLUSTASH_APP_SUPPORT（包括空值）优先；Windows 用非空 APPDATA，
/// 否则回退到 home/AppData/Roaming。非 Windows 保留 Library/Application Support，
/// 包括 Linux；改变该目录需要单独迁移已有数据。
pub fn app_support(home: &Path) -> PathBuf {
    app_support_with(home, |key| std::env::var_os(key))
}
fn app_support_with(home: &Path, env: impl Fn(&str) -> Option<OsString>) -> PathBuf {
    if let Some(configured) = env("ILLUSTASH_APP_SUPPORT") {
        return PathBuf::from(configured);
    }
    #[cfg(windows)]
    {
        env("APPDATA")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("AppData").join("Roaming"))
            .join("Illustash")
    }
    #[cfg(not(windows))]
    {
        home.join("Library").join("Application Support").join("Illustash")
    }
}

/// 与 Path.resolve(strict=False) 相同：即使经过不存在的组件也消去点号段，
/// 处理后续的上级组件之前先跟随已存在的符号链接。
/// 展开用户主目录和校验结果目录由调用方负责。
pub fn resolve(path: &Path) -> Result<PathBuf> {
    let base = if path.is_absolute() { PathBuf::new() } else { std::env::current_dir()? };
    resolve_from(path, &base, &mut HashSet::new())
}
fn resolve_from(path: &Path, base: &Path, links: &mut HashSet<PathBuf>) -> Result<PathBuf> {
    let mut out = if path.is_absolute() { PathBuf::new() } else { base.to_owned() };
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => out.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(name) => {
                let candidate = out.join(name);
                if fs::symlink_metadata(&candidate).is_ok_and(|m| m.file_type().is_symlink()) {
                    if !links.insert(candidate.clone()) {
                        return Err(Error::Io(std::io::Error::from_raw_os_error(libc::ELOOP)));
                    }
                    let target = fs::read_link(&candidate)?;
                    out = resolve_from(&target, &out, links)?;
                    links.remove(&candidate);
                } else {
                    // 不存在或不是目录的组件先保留，后面的 '..' 可能把它消去。
                    out = candidate;
                }
            }
        }
    }
    Ok(out)
}

/// 两个规范化后的文件夹相同或一个包含另一个时算作重叠，按组件逐个比较。
/// macOS 和 Windows 的文件系统默认不区分大小写，所以在这两个平台上比较名称时忽略大小写。
pub fn overlaps(a: &Path, b: &Path) -> bool {
    let key = |p: &Path| -> Vec<String> {
        p.components()
            .filter(|c| !matches!(c, Component::CurDir))
            .map(|c| {
                let text = c.as_os_str().to_string_lossy();
                if cfg!(any(target_os = "macos", windows)) {
                    text.to_lowercase()
                } else {
                    text.into_owned()
                }
            })
            .collect()
    };
    let (a, b) = (key(a), key(b));
    let n = a.len().min(b.len());
    a[..n] == b[..n]
}

/// fs::canonicalize，但在 Windows 上能用普通形式表示时去掉 `\\?\` 前缀：
/// 这些路径会写进 library.json、最近图库记录和资产路径，也会显示给用户，
/// 需要和用户输入、其他程序看到的形式一致。超长或含 Win32 不接受的组件时保留扩展形式。
pub fn canonical(path: &Path) -> std::io::Result<PathBuf> {
    let path = fs::canonicalize(path)?;
    #[cfg(windows)]
    {
        Ok(simplify(path))
    }
    #[cfg(not(windows))]
    {
        Ok(path)
    }
}
#[cfg(windows)]
fn simplify(path: PathBuf) -> PathBuf {
    use std::path::Prefix;
    let Some(text) = path.to_str() else { return path };
    let plain = match path.components().next() {
        Some(Component::Prefix(p)) => match p.kind() {
            Prefix::VerbatimDisk(_) => text[4..].to_owned(),
            Prefix::VerbatimUNC(..) => format!("\\\\{}", &text[8..]),
            _ => return path,
        },
        _ => return path,
    };
    // MAX_PATH 含结尾 NUL；保留名、结尾点或空格在普通形式下会被 Win32 改写。
    const RESERVED: [&str; 22] = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9", "LPT1",
        "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    let safe = plain.encode_utf16().count() < 260
        && Path::new(&plain).components().all(|c| match c {
            Component::Normal(name) => name.to_str().is_some_and(|n| {
                let stem = n.split('.').next().unwrap_or("").trim_end();
                !n.ends_with('.') && !n.ends_with(' ') && !RESERVED.iter().any(|r| r.eq_ignore_ascii_case(stem))
            }),
            _ => true,
        });
    if safe {
        PathBuf::from(plain)
    } else {
        path
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn overlapping_folders_are_same_or_nested() {
        assert!(super::overlaps(std::path::Path::new("/a/b"), std::path::Path::new("/a/b")));
        assert!(super::overlaps(std::path::Path::new("/a/b"), std::path::Path::new("/a/b/c")));
        assert!(super::overlaps(std::path::Path::new("/a/b/c"), std::path::Path::new("/a/b")));
        assert!(!super::overlaps(std::path::Path::new("/a/b"), std::path::Path::new("/a/bc")));
        assert!(!super::overlaps(std::path::Path::new("/a/b"), std::path::Path::new("/a/c")));
        assert_eq!(
            super::overlaps(std::path::Path::new("/a/B"), std::path::Path::new("/a/b/c")),
            cfg!(any(target_os = "macos", windows))
        );
    }
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    // 注入合成环境，避免修改进程环境或读取真实用户目录。
    fn environment<'a>(values: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<OsString> + 'a {
        |key| values.iter().find(|(name, _)| *name == key).map(|(_, value)| OsString::from(value))
    }
    #[test]
    fn home_uses_platform_variable_and_rejects_empty_or_missing_values() {
        #[cfg(windows)]
        let key = "USERPROFILE";
        #[cfg(not(windows))]
        let key = "HOME";
        assert_eq!(user_home_with(environment(&[(key, "synthetic-home")])).unwrap(), PathBuf::from("synthetic-home"));
        for values in [vec![], vec![(key, "")]] {
            assert!(
                matches!(user_home_with(environment(&values)), Err(Error::Invalid(text)) if text == "无法定位用户主目录")
            );
        }
        #[cfg(windows)]
        assert!(user_home_with(environment(&[("HOME", "ignored")])).is_err());
        #[cfg(not(windows))]
        assert!(user_home_with(environment(&[("USERPROFILE", "ignored")])).is_err());
    }
    #[test]
    fn app_support_override_is_exact_including_empty_and_relative_values() {
        for value in ["synthetic-support", "relative/支持目录", ""] {
            let result = app_support_with(Path::new("synthetic-home"), |key| {
                assert_eq!(key, "ILLUSTASH_APP_SUPPORT", "覆盖后不应再取平台默认目录");
                Some(OsString::from(value))
            });
            assert_eq!(result, PathBuf::from(value));
        }
    }
    #[cfg(windows)]
    #[test]
    fn windows_appdata_uses_nonempty_value_or_home_roaming_fallback() {
        let home = Path::new(r"C:\synthetic-home");
        assert_eq!(
            app_support_with(home, environment(&[("APPDATA", r"D:\synthetic-roaming")])),
            Path::new(r"D:\synthetic-roaming").join("Illustash")
        );
        for values in [vec![], vec![("APPDATA", "")]] {
            assert_eq!(
                app_support_with(home, environment(&values)),
                home.join("AppData").join("Roaming").join("Illustash")
            );
        }
    }
    #[cfg(not(windows))]
    #[test]
    fn non_windows_preserves_application_support_even_on_linux() {
        let home = Path::new("/synthetic-home");
        assert_eq!(
            app_support_with(home, environment(&[("APPDATA", "ignored"), ("XDG_DATA_HOME", "ignored")])),
            home.join("Library").join("Application Support").join("Illustash")
        );
    }

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "illustash-paths-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(fs::canonicalize(path).unwrap())
        }
        fn resolve(&self, path: &Path) -> PathBuf {
            resolve_from(path, &self.0, &mut HashSet::new()).unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }
    #[cfg(windows)]
    #[test]
    fn canonical_drops_verbatim_prefix_only_when_win32_can_represent_it() {
        let f = Fixture::new();
        let plain = canonical(&f.0).unwrap();
        assert!(!plain.to_string_lossy().starts_with(r"\\?\"), "{plain:?}");
        assert!(plain.is_dir());
        for kept in [r"\\?\C:\x\CON", r"\\?\C:\x\trailing.", r"\\?\C:\x\space "] {
            assert_eq!(simplify(PathBuf::from(kept)), PathBuf::from(kept));
        }
        assert_eq!(simplify(PathBuf::from(r"\\?\C:\x\y.txt")), PathBuf::from(r"C:\x\y.txt"));
        assert_eq!(simplify(PathBuf::from(r"\\?\UNC\server\share\a")), PathBuf::from(r"\\server\share\a"));
        let long = format!(r"\\?\C:\{}", "a".repeat(300));
        assert_eq!(simplify(PathBuf::from(&long)), PathBuf::from(&long));
    }
    #[test]
    fn missing_then_parent_resolves_existing_directory_without_creating_anything() {
        let f = Fixture::new();
        let existing = f.0.join("existing");
        fs::create_dir(&existing).unwrap();
        let input = existing.join("missing/..");
        // Windows 在打开前按字面折叠 `..`，这个前提只在 unix 上成立；解析结果的断言两边都适用。
        #[cfg(unix)]
        assert!(!input.is_dir());
        let resolved = resolve(&input).unwrap();
        assert_eq!(resolved, existing);
        assert!(resolved.is_dir());
        assert!(!existing.join("missing").exists());
    }
    #[test]
    fn dot_and_parent_segments_normalize_relative_and_missing_paths() {
        let f = Fixture::new();
        assert_eq!(f.resolve(Path::new(".")), f.0);
        assert_eq!(f.resolve(Path::new("missing/.././folder/./child/..")), f.0.join("folder"));
        let missing = f.resolve(Path::new("missing/child/../tail"));
        assert_eq!(missing, f.0.join("missing/tail"));
        assert!(!missing.is_dir());
        assert_eq!(fs::read_dir(&f.0).unwrap().count(), 0);
    }
    #[test]
    fn non_directory_component_can_be_cancelled_before_validation() {
        let f = Fixture::new();
        fs::write(f.0.join("file"), b"keep").unwrap();
        assert_eq!(f.resolve(Path::new("file/missing/../../")), f.0);
        assert_eq!(fs::read(f.0.join("file")).unwrap(), b"keep");
    }
    #[cfg(unix)]
    #[test]
    fn existing_relative_and_absolute_symlinks_resolve_before_parent_components() {
        let f = Fixture::new();
        fs::create_dir_all(f.0.join("target/deep")).unwrap();
        std::os::unix::fs::symlink("target/deep", f.0.join("relative")).unwrap();
        std::os::unix::fs::symlink(f.0.join("target/deep"), f.0.join("absolute")).unwrap();
        for alias in ["relative", "absolute"] {
            assert_eq!(f.resolve(&PathBuf::from(format!("{alias}/.."))), f.0.join("target"));
            assert_eq!(f.resolve(&PathBuf::from(format!("{alias}/missing/.."))), f.0.join("target/deep"));
        }
    }
    #[cfg(unix)]
    #[test]
    fn dangling_target_dot_segments_resolve_without_creating_target() {
        let f = Fixture::new();
        std::os::unix::fs::symlink("missing/../target", f.0.join("alias")).unwrap();
        assert_eq!(f.resolve(Path::new("alias/child/..")), f.0.join("target"));
        assert_eq!(f.resolve(Path::new("alias/..")), f.0);
        assert!(!f.0.join("missing").exists());
        assert!(!f.0.join("target").exists());
    }
    #[cfg(unix)]
    #[test]
    fn repeated_link_use_is_allowed_and_cycles_fail_without_recursing_forever() {
        let f = Fixture::new();
        fs::create_dir(f.0.join("target")).unwrap();
        std::os::unix::fs::symlink("target", f.0.join("alias")).unwrap();
        assert_eq!(f.resolve(Path::new("alias/../alias")), f.0.join("target"));
        std::os::unix::fs::symlink("loop", f.0.join("loop")).unwrap();
        assert!(matches!(resolve_from(Path::new("loop"), &f.0, &mut HashSet::new()), Err(Error::Io(_))));
    }
}
