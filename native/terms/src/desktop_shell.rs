//! 桌面外壳的欢迎页在还没打开任何图库时需要的东西（#107）：最近图库、选中的文件夹是不是图库，以及新建图库。
//! 图库登记、建库流程和参考库规则都与设置页相同；这里不打开、不升级、也不写入任何已有图库。
use crate::app_create::NewSource;
use crate::{Error, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// 仍能打开的最近图库，最近的在前：`[{path, name}]`。
pub fn recent() -> Vec<Value> {
    crate::app_libraries::recent_entries()
}

/// 启动时没给 `--library` 就打开的图库：仍能打开的最近一个。
pub fn startup_library() -> Option<PathBuf> {
    crate::app_libraries::startup().ok()
}

/// 本地服务打开 `path` 失败之后：如果显式升级后就能打开，返回图库的名字（#193）。
/// 只读；外壳随后先征得同意，再带 `--upgrade` 重新启动服务。
pub fn upgrade_needed(path: &Path) -> Option<String> {
    crate::app_upgrade::needed(path).map(|prepared| prepared.name().to_owned())
}

/// 外壳在升级按钮上方显示的文字，与图库页显示的相同（#193）。
pub fn upgrade_note() -> &'static str {
    crate::app_upgrade::NEEDED
}

/// 本地服务打开 `path` 失败之后：如果检查发现它的某个数据库损坏，返回该怎么办（#212）。
/// 只读；这条记录一直保留，直到打开前的检查发现数据库重新完好。
pub fn damage_note(path: &Path) -> Option<&'static str> {
    crate::app_integrity::recorded_damage(path).map(|damage| damage.guidance())
}

/// 损坏是否在 source.sqlite 上；是的话启动页提供抢救（#228，`--rescue-source`）。
pub fn damage_rescuable(path: &Path) -> bool {
    crate::app_integrity::recorded_damage(path)
        .is_some_and(|damage| damage.database == crate::app_integrity::Database::Source)
}

/// 用户选中的文件夹只有带着 library.json 才是图库包。
pub fn is_library(path: &Path) -> bool {
    path.join("library.json").is_file()
}

/// 显示在欢迎页上：新图库放在哪里、文件夹可以选的出处，以及照片图库（仅 macOS）能否作为来源。
pub fn describe() -> Value {
    json!({
        "home": crate::app_libraries::home().ok().and_then(|p| p.to_str().map(str::to_owned)),
        "origins": crate::origin::descriptions(),
        "photos": illustash_platform::capabilities::photos(),
    })
}

/// 在欢迎页上选的一个来源。
pub enum Pick {
    Folder { path: PathBuf, preset: String },
    Photos,
}

/// 在欢迎页上新建的图库。`unlisted` 说明它为什么不在最近图库里：图库包已经发布，只是登记写不进去（#97）。
/// 再新建一次会多出第二个带编号的包；打开这个包就会把它登记进去。
pub struct Created {
    pub path: PathBuf,
    pub unlisted: Option<String>,
}

/// 在图库主目录下新建图库（名字被占用时加编号），并登记到最近图库。
/// `root` 是资源根目录，用来找参考库。返回错误表示什么都没有发布。
pub fn create(root: &Path, name: &str, picks: Vec<Pick>) -> Result<Created> {
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
    let built = crate::app_service::build_sources(root, &dest, name, sources)?;
    let path = PathBuf::from(built["path"].as_str().ok_or_else(|| Error::Schema("建库结果缺少路径".into()))?);
    let unlisted =
        crate::app_libraries::remember_created(&path, name).err().map(|e| crate::app_libraries::unlisted_reason(&e));
    Ok(Created { path, unlisted })
}

/// 欢迎页为数一个文件夹最多等多久；超时后的数量只是下限。
const INSPECT_FOR: std::time::Duration = std::time::Duration::from_secs(3);

/// 欢迎页上选中的每个文件夹里有什么，只读（只看文件名和大小，从不读文件内容）且有时间上限：每个路径按顺序给出
/// `{path, status, images, partial, unreadable, inside}`。`status` 为 `ok`、`missing`、`denied`（文件夹本身列不出
/// 内容）、`file` 或 `empty`（没有同步会读的图片）；`inside` 是排在前面、已经包含这个路径的路径（同一个文件夹，
/// 或它的上级文件夹），因此再读它是多余的。只有 `count` 里的文件夹会遍历图片；其余只给出 `status` 和
/// `inside`，并标 `counted: false`。
pub fn inspect(paths: &[PathBuf], count: &[PathBuf]) -> Vec<Value> {
    let canonical: Vec<Option<PathBuf>> = paths.iter().map(|p| crate::app_paths::canonical(p).ok()).collect();
    paths
        .iter()
        .enumerate()
        .map(|(i, path)| {
            let inside = canonical[i].as_ref().and_then(|me| {
                (0..paths.len()).filter(|&j| j != i).find_map(|j| {
                    let other = canonical[j].as_ref()?;
                    // 与建库用同一条规则。层级相同时排在前面的路径优先，所以同一文件夹的两份里只有后一份是多余的。
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

/// 导入的阶段和计数，与归档导入器报告的一致（`reading`、带字节数的 `unpacking`、`cleaning`、`checking`、
/// `publishing`）。
struct Report<'a>(&'a (dyn Fn(&'static str, Option<(u64, u64)>) + Sync));
impl crate::app_imports::Observer for Report<'_> {
    fn stage(&self, stage: &'static str) {
        (self.0)(stage, None)
    }
    fn count(&self, done: u64, total: u64) {
        (self.0)("", Some((done, total)))
    }
}

/// 从欢迎页把分享的图库归档（.zip）导入到图库主目录，返回新的图库包。
/// 与「设置 › 图库」用同一个导入器：发布前 `cancel` 会删掉已解压的内容。这里不做登记，打开这个包时才登记到最近图库。
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
        // 以 root 运行时照样能列出内容；这时它只是空的。
        assert!(["denied", "empty"].contains(&found[0]["status"].as_str().unwrap()));
    }
}
