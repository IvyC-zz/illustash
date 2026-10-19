//! Full installers opt in through compute/manifest.json. Configure only this process,
//! before Tauri or worker threads start; the base installer has no compute directory,
//! and a compute directory without a matching manifest is an error on every platform.
//! The recognition models are either all in the package (the full package, for development and testing) or none
//! (the release package, #168): then the model root is the shared data's `models/<pack>`, downloaded on first use.
use std::{ffi::OsString, path::Path};

use crate::i18n::{tr, tr_with};

/// 四类计算共用一个 worker 程序，宿主启动时显式传入 kind（#37）。
const WORKER_KEYS: [&str; 4] = [
    "ILLUSTASH_RESEARCH_WORKER",
    "ILLUSTASH_INDEX_WORKER",
    "ILLUSTASH_PROVENANCE_WORKER",
    "ILLUSTASH_RECOGNITION_WORKER",
];

/// Windows 完整包 compute/ 必需组件（与 packaging/build.py 的 desktop_compute 产物一致）。模型不在其中，见 [`models`]。
#[cfg(any(windows, test))]
const WINDOWS_FILES: [&str; 10] = [
    "resources/web/static/index.html",
    "resources/tags/tags_v3.csv",
    "resources/onnxruntime/onnxruntime.dll",
    "resources/licenses/onnxruntime/LICENSE",
    "resources/licenses/onnxruntime/ThirdPartyNotices.txt",
    "resources/openblas/openblas.dll",
    "resources/licenses/openblas/LICENSE.txt",
    "bin/illustash-terms.exe",
    "bin/illustash-iqdb.exe",
    "bin/illustash-worker.exe",
];

/// macOS 完整包 compute/ 必需组件（与 packaging/build.py 的 macos_desktop_compute 产物一致）。模型不在其中，见 [`models`]。
#[cfg(any(target_os = "macos", test))]
const MACOS_FILES: [&str; 10] = [
    "resources/web/static/index.html",
    "resources/tags/tags_v3.csv",
    "resources/onnxruntime/libonnxruntime.dylib",
    "resources/licenses/onnxruntime/LICENSE",
    "resources/licenses/onnxruntime/ThirdPartyNotices.txt",
    "resources/licenses/pocketfft/LICENSE.md",
    "resources/licenses/LCMS.txt",
    "bin/illustash-terms",
    "bin/illustash-iqdb",
    "bin/illustash-worker",
];

/// Where the recognition models come from.
#[cfg(any(windows, target_os = "macos", test))]
#[derive(Debug, PartialEq)]
enum Models {
    /// All in `compute/resources/models` (the full package).
    Bundled,
    /// None in the package: downloaded on first use into the shared data (#168).
    OnDemand,
}

/// 平台无关的完整包契约：没有 compute 目录是基础安装（`Ok(None)`）；
/// 有 compute 目录就必须有可解析的 manifest，声明 format=1、variant=research、
/// 本平台 platform，且每个必需组件既存在又登记在 manifest.files 中。
/// 模型（app_models 清单里的文件）要么全部登记并存在（完整包），要么一个都不登记（发布用的小包）。
/// 平台差异只有 platform 标识、组件文件名和给用户的重装提示。
#[cfg(any(windows, target_os = "macos", test))]
fn full_bundle(
    compute: &Path,
    platform: &str,
    required: &[&str],
    reinstall: &str,
) -> Result<Option<Models>, String> {
    match std::fs::symlink_metadata(compute) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(tr_with("完整安装包目录不可用：{error}；{reinstall}", &[("error", &error), ("reinstall", &reinstall)])),
        Ok(_) => {}
    }
    let manifest = std::fs::read(compute.join("manifest.json"))
        .map_err(|e| tr_with("完整安装包 manifest 不可用：{error}；{reinstall}", &[("error", &e), ("reinstall", &reinstall)]))?;
    let manifest: serde_json::Value = serde_json::from_slice(&manifest)
        .map_err(|e| tr_with("完整安装包 manifest 损坏：{error}；{reinstall}", &[("error", &e), ("reinstall", &reinstall)]))?;
    if manifest["format"] != 1
        || manifest["variant"] != "research"
        || manifest["platform"] != platform
    {
        return Err(tr_with("完整安装包 manifest 类型不匹配；{reinstall}", &[("reinstall", &reinstall)]));
    }
    let listed = |relative: &str| compute.join(relative).is_file() && manifest["files"][relative].is_string();
    for relative in required {
        if !listed(relative) {
            return Err(tr_with("完整安装包组件缺失：{path}；{reinstall}", &[("path", relative), ("reinstall", &reinstall)]));
        }
    }
    let models = models();
    if models.iter().all(|relative| !manifest["files"][relative.as_str()].is_string()) {
        return Ok(Some(Models::OnDemand));
    }
    for relative in &models {
        if !listed(relative) {
            return Err(tr_with("完整安装包组件缺失：{path}；{reinstall}", &[("path", relative), ("reinstall", &reinstall)]));
        }
    }
    Ok(Some(Models::Bundled))
}

/// The model files a full package carries, as compute/ paths (native/terms/models.json).
#[cfg(any(windows, target_os = "macos", test))]
fn models() -> Vec<String> {
    illustash_terms::app_models::manifest().files.iter().map(|entry| format!("resources/models/{}", entry.path)).collect()
}

/// The model root and whether recognition waits for a download: the package's own models, or the shared data's
/// `models/<pack>` (the same folder for every later version using this pack, so an update downloads nothing).
#[cfg(any(windows, target_os = "macos", test))]
fn model_settings(compute: &Path, models: Models, shared: &Path) -> Vec<(&'static str, OsString)> {
    match models {
        Models::Bundled => vec![("ILLUSTASH_MODELS", compute.join("resources/models").into_os_string())],
        Models::OnDemand => vec![
            (
                "ILLUSTASH_MODELS",
                shared.join("models").join(&illustash_terms::app_models::manifest().pack).into_os_string(),
            ),
            ("ILLUSTASH_MODELS_ON_DEMAND", "1".into()),
        ],
    }
}

#[cfg(any(windows, test))]
fn defaults(
    compute: &Path,
    local: Option<OsString>,
    shared: Option<OsString>,
) -> Result<Vec<(&'static str, OsString)>, String> {
    let Some(models) = full_bundle(
        compute,
        "win32",
        &WINDOWS_FILES,
        &tr("请重新安装 Windows 完整版；如要改用基础版，请先卸载完整版再安装基础版"),
    )?
    else {
        return Ok(Vec::new());
    };
    let Some(local) = local.filter(|v| !v.is_empty()) else {
        return Err(tr("无法定位 LOCALAPPDATA；请检查 Windows 用户环境后重启"));
    };
    let default_shared = Path::new(&local).join("Illustash/Shared").into_os_string();
    // An explicit ILLUSTASH_SHARED stays (configure never overrides it); the models follow it.
    let models = model_settings(compute, models, Path::new(shared.as_ref().unwrap_or(&default_shared)));
    let mut values = vec![("ILLUSTASH_ROOT", compute.join("resources").into_os_string())];
    values.extend(models);
    values.push((
        "ILLUSTASH_RESEARCH_OPENBLAS_DLL",
        compute
            .join("resources/openblas/openblas.dll")
            .into_os_string(),
    ));
    let worker = compute.join("bin/illustash-worker.exe").into_os_string();
    for key in WORKER_KEYS {
        values.push((key, worker.clone()));
    }
    values.push(("ILLUSTASH_SHARED", default_shared));
    Ok(values)
}

#[cfg(any(target_os = "macos", test))]
fn macos_defaults(
    compute: &Path,
    home: Option<OsString>,
    shared: Option<OsString>,
) -> Result<Vec<(&'static str, OsString)>, String> {
    let Some(models) = full_bundle(compute, "darwin", &MACOS_FILES, &tr("请重新安装 macOS 完整版"))? else {
        return Ok(Vec::new());
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for relative in [
            "bin/illustash-terms",
            "bin/illustash-iqdb",
            "bin/illustash-worker",
        ] {
            let mode = std::fs::metadata(compute.join(relative))
                .map_err(|e| e.to_string())?
                .permissions()
                .mode();
            if mode & 0o111 == 0 {
                return Err(tr_with("完整安装包组件不可执行：{path}；请重新安装 macOS 完整版", &[("path", &relative)]));
            }
        }
    }
    let shared = match shared {
        Some(path) => path,
        None => {
            let home = home
                .filter(|p| !p.is_empty())
                .map(std::path::PathBuf::from)
                .filter(|p| p.is_absolute())
                .ok_or_else(|| tr("无法定位 HOME；请检查 macOS 用户环境后重启"))?;
            home.join("Library/Application Support/Illustash/Shared")
                .into_os_string()
        }
    };
    let mut values = vec![("ILLUSTASH_ROOT", compute.join("resources").into_os_string())];
    values.extend(model_settings(compute, models, Path::new(&shared)));
    values.push(("ILLUSTASH_SHARED", shared));
    let worker = compute.join("bin/illustash-worker").into_os_string();
    for key in WORKER_KEYS {
        values.push((key, worker.clone()));
    }
    Ok(values)
}

#[cfg(target_os = "macos")]
pub fn configure() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let contents = exe
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| tr("无法定位应用安装目录"))?;
    let compute = contents.join("Resources/compute");
    for (key, value) in macos_defaults(
        &compute,
        std::env::var_os("HOME"),
        std::env::var_os("ILLUSTASH_SHARED"),
    )? {
        if std::env::var_os(key).is_none() {
            std::env::set_var(key, value);
        }
    }
    Ok(())
}

#[cfg(windows)]
pub fn configure() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let compute = exe.parent().ok_or_else(|| tr("无法定位安装目录"))?.join("compute");
    for (key, value) in defaults(&compute, std::env::var_os("LOCALAPPDATA"), std::env::var_os("ILLUSTASH_SHARED"))? {
        if std::env::var_os(key).is_none() {
            std::env::set_var(key, value);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn macos_full_bundle_relocation_and_dependency_errors() {
        let scratch = std::env::temp_dir().join(format!(
            "illustash-mac-compute-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&scratch).unwrap();
        let root = scratch.join("original.app/Contents/Resources/compute");
        assert!(macos_defaults(&root, None, None).unwrap().is_empty());
        fs::create_dir_all(&root).unwrap();
        assert!(macos_defaults(&root, None, None)
            .unwrap_err()
            .contains("manifest"));
        fs::write(root.join("manifest.json"), "invalid").unwrap();
        assert!(macos_defaults(&root, None, None)
            .unwrap_err()
            .contains("manifest"));
        let mut files = serde_json::Map::new();
        for name in [
            "resources/web/static/index.html",
            "resources/tags/tags_v3.csv",
            "resources/models/wd_swinv2_v3_emb.onnx",
            "resources/models/ccip/model_feat.onnx",
            "resources/models/ccip/model_metrics.onnx",
            "resources/models/ccip/metrics.json",
            "resources/onnxruntime/libonnxruntime.dylib",
            "resources/licenses/onnxruntime/LICENSE",
            "resources/licenses/onnxruntime/ThirdPartyNotices.txt",
            "resources/licenses/pocketfft/LICENSE.md",
            "resources/licenses/LCMS.txt",
            "bin/illustash-terms",
            "bin/illustash-iqdb",
            "bin/illustash-worker",
        ] {
            let path = root.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, "synthetic").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
            }
            files.insert(name.into(), "synthetic-sha".into());
        }
        let manifest =
            serde_json::json!({"format":1,"variant":"research","platform":"darwin","files":files});
        fs::write(root.join("manifest.json"), manifest.to_string()).unwrap();
        let moved = scratch.join("moved app with spaces");
        fs::rename(scratch.join("original.app"), &moved).unwrap();
        let root = moved.join("Contents/Resources/compute");
        let home = scratch.join("synthetic home");
        let settings = macos_defaults(&root, Some(home.clone().into_os_string()), None).unwrap();
        assert_eq!(settings.len(), 7);
        assert_eq!(settings[0].1, root.join("resources").into_os_string());
        assert_eq!(
            settings[1].1,
            root.join("resources/models").into_os_string()
        );
        assert_eq!(
            settings[2].1,
            home.join("Library/Application Support/Illustash/Shared")
                .into_os_string()
        );
        assert!(settings
            .iter()
            .all(|(key, _)| !key.contains("NETWORK") && !key.contains("PYTHON")));
        // 四个 worker 变量都指向同一个程序，kind 由宿主启动时传入。
        assert_eq!(
            settings[3..]
                .iter()
                .map(|(key, value)| (*key, value.clone()))
                .collect::<Vec<_>>(),
            WORKER_KEYS
                .map(|key| (key, root.join("bin/illustash-worker").into_os_string()))
                .to_vec()
        );
        assert!(!home.exists());
        assert_eq!(
            macos_defaults(&root, None, Some("/explicit shared".into())).unwrap()[2].1,
            OsString::from("/explicit shared")
        );
        assert!(macos_defaults(&root, None, None)
            .unwrap_err()
            .contains("HOME"));
        assert!(macos_defaults(&root, Some("relative".into()), None)
            .unwrap_err()
            .contains("HOME"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let helper = root.join("bin/illustash-worker");
            fs::set_permissions(&helper, fs::Permissions::from_mode(0o644)).unwrap();
            assert!(
                macos_defaults(&root, Some(home.clone().into_os_string()), None)
                    .unwrap_err()
                    .contains("不可执行")
            );
            fs::set_permissions(&helper, fs::Permissions::from_mode(0o755)).unwrap();
        }
        fs::remove_file(root.join("resources/onnxruntime/libonnxruntime.dylib")).unwrap();
        assert!(macos_defaults(&root, Some(home.into_os_string()), None)
            .unwrap_err()
            .contains("libonnxruntime"));
        fs::remove_dir_all(scratch).unwrap();
    }

    fn scratch(prefix: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "{prefix}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn windows_manifest(platform: &str, variant: &str, format: i64) -> String {
        let files: serde_json::Map<_, _> = WINDOWS_FILES
            .iter()
            .map(|name| ((*name).into(), "synthetic-sha".into()))
            .collect();
        serde_json::json!({"format":format,"variant":variant,"platform":platform,"files":files})
            .to_string()
    }

    #[test]
    fn windows_base_install_has_no_compute_directory() {
        let scratch = scratch("illustash-win-base");
        let compute = scratch.join("compute");
        assert!(defaults(&compute, None, None).unwrap().is_empty());
        assert!(defaults(&compute, Some("user data".into()), None)
            .unwrap()
            .is_empty());
        fs::remove_dir_all(scratch).unwrap();
    }

    #[test]
    fn windows_compute_requires_valid_matching_manifest() {
        let scratch = scratch("illustash-win-manifest");
        let root = scratch.join("compute");
        fs::create_dir_all(&root).unwrap();
        let local = || Some(OsString::from("user data"));
        // compute 存在但 manifest 缺失：不再当作基础安装。
        let error = defaults(&root, local(), None).unwrap_err();
        assert!(error.contains("manifest 不可用"), "{error}");
        assert!(error.contains("重新安装 Windows 完整版"), "{error}");
        for name in WINDOWS_FILES {
            let path = root.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, b"fixture").unwrap();
        }
        // 组件齐全也不能掩盖 manifest 缺失或损坏。
        assert!(defaults(&root, local(), None)
            .unwrap_err()
            .contains("manifest 不可用"));
        for broken in ["invalid", "", "{\"format\":1,"] {
            fs::write(root.join("manifest.json"), broken).unwrap();
            assert!(defaults(&root, local(), None)
                .unwrap_err()
                .contains("manifest 损坏"));
        }
        for mismatched in [
            "{}".to_string(),
            "[]".to_string(),
            windows_manifest("darwin", "research", 1),
            windows_manifest("linux", "research", 1),
            windows_manifest("win32", "base", 1),
            windows_manifest("win32", "research", 2),
        ] {
            fs::write(root.join("manifest.json"), mismatched).unwrap();
            assert!(defaults(&root, local(), None)
                .unwrap_err()
                .contains("manifest 类型不匹配"));
        }
        // 声明正确但 files 未登记必需组件。
        fs::write(
            root.join("manifest.json"),
            r#"{"format":1,"variant":"research","platform":"win32","files":{}}"#,
        )
        .unwrap();
        assert!(defaults(&root, local(), None)
            .unwrap_err()
            .contains("resources/web/static/index.html"));
        fs::remove_dir_all(scratch).unwrap();
    }

    #[test]
    fn platforms_share_contract_and_reject_each_others_bundle() {
        let scratch = scratch("illustash-cross-platform");
        let root = scratch.join("compute");
        fs::create_dir_all(&root).unwrap();
        for name in WINDOWS_FILES.iter().chain(MACOS_FILES.iter()) {
            let path = root.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, b"fixture").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        let files: serde_json::Map<_, _> = WINDOWS_FILES
            .iter()
            .chain(MACOS_FILES.iter())
            .map(|name| ((*name).into(), "synthetic-sha".into()))
            .collect();
        for (platform, windows_ok) in [("win32", true), ("darwin", false)] {
            let manifest = serde_json::json!(
                {"format":1,"variant":"research","platform":platform,"files":files.clone()});
            fs::write(root.join("manifest.json"), manifest.to_string()).unwrap();
            let windows = defaults(&root, Some("user data".into()), None);
            let macos = macos_defaults(&root, None, Some("synthetic shared".into()));
            if windows_ok {
                // No models in the package: the on-demand flag joins the usual settings.
                assert_eq!(windows.unwrap().len(), 9);
                assert!(macos.unwrap_err().contains("macOS 完整版"));
            } else {
                assert!(windows.unwrap_err().contains("manifest 类型不匹配"));
                assert_eq!(macos.unwrap().len(), 8);
            }
        }
        fs::remove_dir_all(scratch).unwrap();
    }

    #[test]
    fn windows_complete_bundle_and_missing_component() {
        let scratch = scratch("illustash-bundle");
        let root = scratch.join("relocated with spaces/compute");
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("manifest.json"),
            windows_manifest("win32", "research", 1),
        )
        .unwrap();
        assert!(defaults(&root, Some("user data".into()), None)
            .unwrap_err()
            .contains("resources/web/static/index.html"));
        for name in WINDOWS_FILES {
            let path = root.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, b"fixture").unwrap();
        }
        // The release package: no models inside, so they are downloaded into the shared data on first use.
        let pack = &illustash_terms::app_models::manifest().pack;
        let settings = defaults(&root, Some("user data".into()), None).unwrap();
        let keys: Vec<_> = settings.iter().map(|(key, _)| *key).collect();
        assert_eq!(
            keys,
            [
                "ILLUSTASH_ROOT",
                "ILLUSTASH_MODELS",
                "ILLUSTASH_MODELS_ON_DEMAND",
                "ILLUSTASH_RESEARCH_OPENBLAS_DLL",
                "ILLUSTASH_RESEARCH_WORKER",
                "ILLUSTASH_INDEX_WORKER",
                "ILLUSTASH_PROVENANCE_WORKER",
                "ILLUSTASH_RECOGNITION_WORKER",
                "ILLUSTASH_SHARED",
            ]
        );
        assert_eq!(settings[0].1, root.join("resources").into_os_string());
        assert_eq!(
            settings[1].1,
            Path::new("user data").join("Illustash/Shared").join("models").join(pack).into_os_string()
        );
        assert_eq!(settings[2].1, OsString::from("1"));
        assert_eq!(
            settings[3].1,
            root.join("resources/openblas/openblas.dll")
                .into_os_string()
        );
        for (_, value) in &settings[4..8] {
            assert_eq!(
                *value,
                root.join("bin/illustash-worker.exe").into_os_string()
            );
        }
        assert_eq!(
            settings[8].1,
            Path::new("user data")
                .join("Illustash/Shared")
                .into_os_string()
        );
        assert!(settings
            .iter()
            .all(|(key, _)| !key.contains("NETWORK") && !key.contains("PYTHON")));
        // An explicit shared folder is kept, and the downloaded models live under it.
        let explicit = defaults(&root, Some("user data".into()), Some("explicit shared".into())).unwrap();
        assert_eq!(explicit[1].1, Path::new("explicit shared").join("models").join(pack).into_os_string());
        // The full package: every model listed and present, the package's own folder, no download.
        let mut manifest: serde_json::Value = serde_json::from_str(&windows_manifest("win32", "research", 1)).unwrap();
        for name in models() {
            manifest["files"][&name] = "synthetic-sha".into();
        }
        fs::write(root.join("manifest.json"), manifest.to_string()).unwrap();
        let error = defaults(&root, Some("user data".into()), None).unwrap_err();
        assert!(error.contains("resources/models/wd_swinv2_v3_emb.onnx"), "{error}");
        for name in models() {
            let path = root.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, b"fixture").unwrap();
        }
        let settings = defaults(&root, Some("user data".into()), None).unwrap();
        assert_eq!(settings[1], ("ILLUSTASH_MODELS", root.join("resources/models").into_os_string()));
        assert!(settings.iter().all(|(key, _)| *key != "ILLUSTASH_MODELS_ON_DEMAND"));
        assert_eq!(settings.len(), 8);
        // Only some models listed: neither package, so reinstall.
        manifest["files"].as_object_mut().unwrap().remove("resources/models/ccip/metrics.json");
        fs::write(root.join("manifest.json"), manifest.to_string()).unwrap();
        let error = defaults(&root, Some("user data".into()), None).unwrap_err();
        assert!(error.contains("resources/models/ccip/metrics.json") && error.contains("重新安装"), "{error}");
        manifest["files"]["resources/models/ccip/metrics.json"] = "synthetic-sha".into();
        fs::write(root.join("manifest.json"), manifest.to_string()).unwrap();
        assert!(defaults(&root, None, None).unwrap_err().contains("LOCALAPPDATA"));
        fs::remove_file(root.join("bin/illustash-worker.exe")).unwrap();
        assert!(defaults(&root, Some("user data".into()), None)
            .unwrap_err()
            .contains("illustash-worker.exe"));
        // #37 之前的布局（每类一份 illustash-<kind>.exe）不再满足契约：提示重装，不按旧文件名猜。
        for kind in ["research", "index", "provenance", "recognition"] {
            fs::write(root.join(format!("bin/illustash-{kind}.exe")), b"fixture").unwrap();
        }
        let error = defaults(&root, Some("user data".into()), None).unwrap_err();
        assert!(error.contains("illustash-worker.exe"), "{error}");
        assert!(error.contains("重新安装 Windows 完整版"), "{error}");
        fs::remove_dir_all(scratch).unwrap();
    }
}
