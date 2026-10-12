//! What the desktop shell's welcome page needs before any library is open (#107): the recent libraries,
//! whether a picked folder is a library, and creating one. The same registry, factory and reference rule as
//! the settings page; nothing here opens, upgrades or writes an existing library.
use crate::app_create::NewSource;
use crate::{Error, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// Recent libraries that can still be opened, most recent first: `[{path, name}]`.
pub fn recent() -> Vec<Value> {
    crate::app_libraries::recent_entries()
}

/// The library opened at startup without `--library`: the most recent one that can still be opened.
pub fn startup_library() -> Option<PathBuf> {
    crate::app_libraries::startup().ok()
}

/// A folder the user picked is a library package only if it carries library.json.
pub fn is_library(path: &Path) -> bool {
    path.join("library.json").is_file()
}

/// Shown on the welcome page: where new libraries go, the origins a folder can have, and whether the
/// Photos library (macOS only) can be a source.
pub fn describe() -> Value {
    json!({
        "home": crate::app_libraries::home().ok().and_then(|p| p.to_str().map(str::to_owned)),
        "origins": crate::origin::descriptions(),
        "photos": illustash_platform::capabilities::photos(),
    })
}

/// One source picked on the welcome page.
pub enum Pick {
    Folder { path: PathBuf, preset: String },
    Photos,
}

/// Create a library under the libraries home (a numbered name when taken), list it among the recent ones and
/// return its path. `root` is the resource root, for the reference library.
pub fn create(root: &Path, name: &str, picks: Vec<Pick>) -> Result<PathBuf> {
    let name = crate::compat_text::trim(name);
    let name = if name.is_empty() { "新图库" } else { name };
    let mut sources = Vec::new();
    for pick in picks {
        sources.push(match pick {
            Pick::Folder { path, preset } => NewSource::Folder { path, preset },
            Pick::Photos => {
                if !illustash_platform::capabilities::photos() {
                    return Err(Error::Invalid("Unsupported：照片图库只在 macOS 上可用".into()));
                }
                NewSource::Photos { path: crate::app_libraries::photos_library()? }
            }
        });
    }
    let dest = crate::app_libraries::unique_path(name)?;
    crate::app_service::create(root, &dest, name, sources)
}

/// How long the welcome page waits for one folder to be counted; past it the count is a lower bound.
const INSPECT_FOR: std::time::Duration = std::time::Duration::from_secs(3);

/// What each folder picked on the welcome page holds, read-only (names and sizes, never file contents) and bounded
/// in time: `{path, status, images, partial, unreadable, inside}` per path, in order. `status` is `ok`, `missing`,
/// `denied` (the folder itself cannot be listed), `file` or `empty` (no picture the sync would read); `inside` is the
/// earlier-listed path that already holds this one (the same folder, or a folder above it), so reading it is redundant.
/// Only the folders in `count` are walked for pictures; the others get `status` and `inside` (`counted: false`).
pub fn inspect(paths: &[PathBuf], count: &[PathBuf]) -> Vec<Value> {
    let canonical: Vec<Option<PathBuf>> = paths.iter().map(|p| crate::app_paths::canonical(p).ok()).collect();
    paths
        .iter()
        .enumerate()
        .map(|(i, path)| {
            let inside = canonical[i].as_ref().and_then(|me| {
                (0..paths.len()).filter(|&j| j != i).find_map(|j| {
                    let other = canonical[j].as_ref()?;
                    // The same rule as creating the library. An earlier path wins a tie, so of two copies of one
                    // folder only the later is redundant.
                    let (mine, theirs) = (me.components().count(), other.components().count());
                    let holds = crate::app_paths::overlaps(me, other) && (mine > theirs || (mine == theirs && j < i));
                    holds.then(|| paths[j].to_string_lossy().into_owned())
                })
            });
            let counted = count.contains(path);
            let mut out = json!({"path": path.to_string_lossy(), "images": 0, "partial": false, "unreadable": 0,
                "inside": inside, "counted": counted});
            out["status"] = match std::fs::metadata(path) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => "missing".into(),
                Err(_) => "denied".into(),
                Ok(m) if !m.is_dir() => "file".into(),
                Ok(_) if std::fs::read_dir(path).is_err() => "denied".into(),
                Ok(_) if !counted => "ok".into(),
                Ok(_) => {
                    let until = std::time::Instant::now() + INSPECT_FOR;
                    let (mut files, mut unreadable) = (Vec::new(), Vec::new());
                    let check = || {
                        if std::time::Instant::now() < until {
                            Ok(())
                        } else {
                            Err(Error::Invalid("time".into()))
                        }
                    };
                    let partial =
                        crate::platform_folder::walk(path, path, &mut files, &mut unreadable, &check).is_err();
                    out["images"] = files.len().into();
                    out["partial"] = partial.into();
                    out["unreadable"] = unreadable.len().into();
                    if files.is_empty() && !partial { "empty" } else { "ok" }.into()
                }
            };
            out
        })
        .collect()
}

/// Stages and counts of an import, as the archive importer reports them (`reading`, `unpacking` with bytes,
/// `cleaning`, `checking`, `publishing`).
struct Report<'a>(&'a (dyn Fn(&'static str, Option<(u64, u64)>) + Sync));
impl crate::app_imports::Observer for Report<'_> {
    fn stage(&self, stage: &'static str) {
        (self.0)(stage, None)
    }
    fn count(&self, done: u64, total: u64) {
        (self.0)("", Some((done, total)))
    }
}

/// Import a shared library archive (.zip) from the welcome page into the libraries home and return the new package.
/// Same importer as 设置 › 图库: `cancel` before publication removes what was unpacked. Nothing is registered here —
/// opening the package lists it among the recent libraries.
pub fn import(
    archive: &Path,
    cancel: &std::sync::Arc<std::sync::atomic::AtomicBool>,
    report: &(dyn Fn(&'static str, Option<(u64, u64)>) + Sync),
) -> Result<PathBuf> {
    crate::app_archives::import(archive, cancel, &Report(report)).map(|(path, _)| path)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Root(PathBuf);
    impl Drop for Root {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn inspect_reports_each_folder_and_which_are_already_covered() {
        let root = Root(std::env::temp_dir().join(format!("illustash-inspect-{}", std::process::id())));
        let pictures = root.0.join("pictures");
        let nested = pictures.join("x");
        let empty = root.0.join("empty");
        for dir in [&nested, &empty] {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::write(pictures.join("a.png"), b"png").unwrap();
        std::fs::write(nested.join("b.JPG"), b"jpg").unwrap();
        std::fs::write(empty.join("notes.txt"), b"text").unwrap();
        let file = pictures.join("a.png");
        let missing = root.0.join("missing");
        let all = [nested.clone(), pictures.clone(), empty.clone(), missing, file, pictures.clone()];
        let found = inspect(&all, &all);
        let get = |i: usize, k: &str| found[i][k].clone();
        assert_eq!((get(0, "status"), get(0, "images")), (json!("ok"), json!(1)));
        assert_eq!(get(0, "inside"), json!(pictures.to_string_lossy()), "a folder below another is read with it");
        assert_eq!((get(1, "status"), get(1, "images"), get(1, "inside")), (json!("ok"), json!(2), Value::Null));
        assert_eq!((get(2, "status"), get(2, "images")), (json!("empty"), json!(0)));
        assert_eq!(get(3, "status"), json!("missing"));
        assert_eq!(get(4, "status"), json!("file"));
        assert_eq!(get(5, "inside"), json!(pictures.to_string_lossy()), "the second copy of one folder");
        assert_eq!(get(1, "partial"), json!(false));
        let cheap = inspect(&all[..2], &[]);
        assert_eq!((cheap[1]["images"].clone(), cheap[1]["counted"].clone()), (json!(0), json!(false)));
        assert_eq!(cheap[0]["inside"], json!(pictures.to_string_lossy()), "overlaps are always reported");
    }

    #[cfg(unix)]
    #[test]
    fn a_folder_that_cannot_be_listed_is_denied() {
        use std::os::unix::fs::PermissionsExt;
        let root = Root(std::env::temp_dir().join(format!("illustash-inspect-denied-{}", std::process::id())));
        let locked = root.0.join("locked");
        std::fs::create_dir_all(&locked).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let found = inspect(std::slice::from_ref(&locked), std::slice::from_ref(&locked));
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        // Running as root lists it anyway; then it is simply empty.
        assert!(["denied", "empty"].contains(&found[0]["status"].as_str().unwrap()));
    }
}
