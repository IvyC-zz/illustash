//! 只放资源的安装根目录；图库状态仍归 app_libraries 管。
use crate::{Error, Result};
use std::path::{Path, PathBuf};
pub fn installed_root(executable: &Path) -> Option<PathBuf> {
    let bin = executable.parent()?;
    let candidates = [bin.join("../resources"), bin.join("../Resources/resources"), bin.join("resources")];
    candidates
        .into_iter()
        .find(|root| root.join("web/static/index.html").is_file() && root.join("tags/tags_v3.csv").is_file())
}
pub fn default_root() -> PathBuf {
    std::env::var_os("ILLUSTASH_ROOT")
        .map(PathBuf::from)
        .or_else(|| std::env::current_exe().ok().and_then(|exe| installed_root(&exe)))
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
}
/// 识别和索引 worker 加载的 ONNX Runtime：`ILLUSTASH_ORT_DYLIB`，否则用完整安装包放在资源根目录下的那一份。
pub fn ort_runtime(resources: &Path) -> PathBuf {
    let library = if cfg!(windows) {
        "onnxruntime.dll"
    } else if cfg!(target_os = "macos") {
        "libonnxruntime.dylib"
    } else {
        "libonnxruntime.so"
    };
    std::env::var_os("ILLUSTASH_ORT_DYLIB")
        .map(PathBuf::from)
        .unwrap_or_else(|| resources.join("onnxruntime").join(library))
}
/// 共享的运行时资源。相对路径的覆盖值以资源根目录为基准，绝不以 worker 的 CWD 为基准。
pub fn shared_path(root: &Path) -> Result<PathBuf> {
    shared_from(root, &std::env::current_dir()?, std::env::var_os("ILLUSTASH_SHARED").as_deref().map(Path::new))
}
fn shared_from(root: &Path, cwd: &Path, configured: Option<&Path>) -> Result<PathBuf> {
    let root = crate::app_paths::resolve(&if root.is_absolute() { root.to_owned() } else { cwd.join(root) })?;
    let Some(configured) = configured else { return Ok(root.join("review")) };
    if configured.is_absolute() {
        return crate::app_paths::resolve(configured);
    }
    let desired = crate::app_paths::resolve(&root.join(configured))?;
    let legacy = crate::app_paths::resolve(&cwd.join(configured))?;
    // 以前的索引和旧实现的消费者（#9 已删除）按自己的 CWD 解释相对路径的覆盖值。拒绝这种有歧义的切换，不去检查私有数据，
    // 也不新建空索引。绝对路径的覆盖值是明确的。
    if desired != legacy {
        for name in
            ["reference.sqlite", "char-index", "game-chars", "char-anchors.npy", "char-anchors.txt", "char-patch"]
        {
            match std::fs::symlink_metadata(legacy.join(name)) {
                Ok(_) => {
                    return Err(Error::Invalid(
                        "ILLUSTASH_SHARED 相对路径在旧工作目录下已有共享资源；请指定原目录的绝对路径".into(),
                    ));
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
    Ok(desired)
}
/// 新图库从中复制角色索引的参考图库；可能不存在。
pub(crate) fn reference_path(root: &Path) -> Result<PathBuf> {
    Ok(shared_path(root)?.join("reference.sqlite"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shared_paths_anchor_to_root_and_refuse_legacy_resources() {
        let temp = std::env::temp_dir().join(format!("illustash-shared-test-{}", std::process::id()));
        std::fs::create_dir_all(&temp).unwrap();
        let temp = temp.canonicalize().unwrap();
        let cwd = temp.join("cwd");
        let root = cwd.join("resources");
        std::fs::create_dir_all(&root).unwrap();
        assert_eq!(shared_from(Path::new("resources"), &cwd, None).unwrap(), root.join("review"));
        assert_eq!(shared_from(&root, &cwd, Some(Path::new("shared"))).unwrap(), root.join("shared"));
        let old = cwd.join("shared");
        std::fs::create_dir_all(&old).unwrap();
        assert!(shared_from(&root, &cwd, Some(Path::new("shared"))).is_ok());
        std::fs::write(old.join("reference.sqlite"), b"synthetic").unwrap();
        assert!(shared_from(&root, &cwd, Some(Path::new("shared"))).is_err());
        assert!(!root.join("shared").exists());
        assert_eq!(shared_from(&root, &cwd, Some(&old)).unwrap(), old);
        assert_eq!(shared_from(&cwd, &cwd, Some(Path::new("shared"))).unwrap(), old);
        #[cfg(unix)]
        {
            let alias = root.join("shared");
            std::os::unix::fs::symlink(&old, &alias).unwrap();
            assert_eq!(shared_from(&root, &cwd, Some(Path::new("shared"))).unwrap(), old);
            std::fs::remove_file(&alias).unwrap();
            std::fs::remove_file(old.join("reference.sqlite")).unwrap();
            std::os::unix::fs::symlink(old.join("missing"), old.join("char-index")).unwrap();
            assert!(shared_from(&root, &cwd, Some(Path::new("shared"))).is_err());
        }
        std::fs::remove_dir_all(temp).unwrap();
    }
    #[test]
    fn portable_and_app_bundle_require_complete_resources() {
        let root = std::env::temp_dir().join(format!("illustash-resource-test-{}", std::process::id()));
        for (bin, resources) in
            [("portable/bin", "portable/resources"), ("App.app/Contents/MacOS", "App.app/Contents/Resources/resources")]
        {
            let executable = root.join(bin).join("illustash");
            std::fs::create_dir_all(executable.parent().unwrap()).unwrap();
            let resources = root.join(resources);
            std::fs::create_dir_all(resources.join("web/static")).unwrap();
            std::fs::write(resources.join("web/static/index.html"), "synthetic").unwrap();
            assert!(installed_root(&executable).is_none());
            std::fs::create_dir_all(resources.join("tags")).unwrap();
            std::fs::write(resources.join("tags/tags_v3.csv"), "synthetic").unwrap();
            assert_eq!(installed_root(&executable).unwrap().canonicalize().unwrap(), resources.canonicalize().unwrap());
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
