//! Full installers opt in through compute/manifest.json. Configure only this process,
//! before Tauri or worker threads start; the base installer has no compute directory,
//! and a compute directory without a matching manifest is an error on every platform.
use std::{ffi::OsString, path::Path};

const KINDS: [&str; 4] = ["research", "index", "provenance", "recognition"];

/// Windows 完整包 compute/ 必需组件（与 packaging/build.py 的 desktop_compute 产物一致）。
#[cfg(any(windows, test))]
const WINDOWS_FILES: [&str; 17] = [
    "resources/web/static/index.html",
    "resources/tags/tags_v3.csv",
    "resources/onnxruntime/onnxruntime.dll",
    "resources/licenses/onnxruntime/LICENSE",
    "resources/licenses/onnxruntime/ThirdPartyNotices.txt",
    "resources/openblas/openblas.dll",
    "resources/licenses/openblas/LICENSE.txt",
    "bin/illustash-terms.exe",
    "bin/illustash-iqdb.exe",
    "resources/models/wd_swinv2_v3_emb.onnx",
    "resources/models/ccip/model_feat.onnx",
    "resources/models/ccip/model_metrics.onnx",
    "resources/models/ccip/metrics.json",
    "bin/illustash-research.exe",
    "bin/illustash-index.exe",
    "bin/illustash-provenance.exe",
    "bin/illustash-recognition.exe",
];

/// macOS 完整包 compute/ 必需组件（与 packaging/build.py 的 macos_desktop_compute 产物一致）。
#[cfg(any(target_os = "macos", test))]
const MACOS_FILES: [&str; 17] = [
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
    "bin/illustash-research",
    "bin/illustash-index",
    "bin/illustash-provenance",
    "bin/illustash-recognition",
];

/// 平台无关的完整包契约：没有 compute 目录是基础安装（`Ok(false)`）；
/// 有 compute 目录就必须有可解析的 manifest，声明 format=1、variant=research、
/// 本平台 platform，且每个必需组件既存在又登记在 manifest.files 中。
/// 平台差异只有 platform 标识、组件文件名和给用户的重装提示。
#[cfg(any(windows, target_os = "macos", test))]
fn full_bundle(
    compute: &Path,
    platform: &str,
    required: &[&str],
    reinstall: &str,
) -> Result<bool, String> {
    match std::fs::symlink_metadata(compute) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(format!("完整安装包目录不可用：{error}；{reinstall}")),
        Ok(_) => {}
    }
    let manifest = std::fs::read(compute.join("manifest.json"))
        .map_err(|e| format!("完整安装包 manifest 不可用：{e}；{reinstall}"))?;
    let manifest: serde_json::Value = serde_json::from_slice(&manifest)
        .map_err(|e| format!("完整安装包 manifest 损坏：{e}；{reinstall}"))?;
    if manifest["format"] != 1
        || manifest["variant"] != "research"
        || manifest["platform"] != platform
    {
        return Err(format!("完整安装包 manifest 类型不匹配；{reinstall}"));
    }
    for relative in required {
        if !compute.join(relative).is_file() || !manifest["files"][relative].is_string() {
            return Err(format!("完整安装包组件缺失：{relative}；{reinstall}"));
        }
    }
    Ok(true)
}

#[cfg(any(windows, test))]
fn defaults(
    compute: &Path,
    local: Option<OsString>,
) -> Result<Vec<(&'static str, OsString)>, String> {
    if !full_bundle(
        compute,
        "win32",
        &WINDOWS_FILES,
        "请重新安装 Windows 完整版；如要改用基础版，请先卸载完整版再安装基础版",
    )? {
        return Ok(Vec::new());
    }
    let mut values = vec![
        ("ILLUSTASH_ROOT", compute.join("resources").into_os_string()),
        (
            "ILLUSTASH_MODELS",
            compute.join("resources/models").into_os_string(),
        ),
    ];
    values.push((
        "ILLUSTASH_RESEARCH_OPENBLAS_DLL",
        compute
            .join("resources/openblas/openblas.dll")
            .into_os_string(),
    ));
    for (kind, key) in KINDS.into_iter().zip([
        "ILLUSTASH_RESEARCH_WORKER",
        "ILLUSTASH_INDEX_WORKER",
        "ILLUSTASH_PROVENANCE_WORKER",
        "ILLUSTASH_RECOGNITION_WORKER",
    ]) {
        values.push((
            key,
            compute
                .join(format!("bin/illustash-{kind}.exe"))
                .into_os_string(),
        ));
    }
    if let Some(local) = local.filter(|v| !v.is_empty()) {
        values.push((
            "ILLUSTASH_SHARED",
            Path::new(&local).join("Illustash/Shared").into_os_string(),
        ));
    } else {
        return Err("无法定位 LOCALAPPDATA；请检查 Windows 用户环境后重启".into());
    }
    Ok(values)
}

#[cfg(any(target_os = "macos", test))]
fn macos_defaults(
    compute: &Path,
    home: Option<OsString>,
    shared: Option<OsString>,
) -> Result<Vec<(&'static str, OsString)>, String> {
    if !full_bundle(compute, "darwin", &MACOS_FILES, "请重新安装 macOS 完整版")? {
        return Ok(Vec::new());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for relative in [
            "bin/illustash-terms",
            "bin/illustash-iqdb",
            "bin/illustash-research",
            "bin/illustash-index",
            "bin/illustash-provenance",
            "bin/illustash-recognition",
        ] {
            let mode = std::fs::metadata(compute.join(relative))
                .map_err(|e| e.to_string())?
                .permissions()
                .mode();
            if mode & 0o111 == 0 {
                return Err(format!(
                    "完整安装包组件不可执行：{relative}；请重新安装 macOS 完整版"
                ));
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
                .ok_or("无法定位 HOME；请检查 macOS 用户环境后重启")?;
            home.join("Library/Application Support/Illustash/Shared")
                .into_os_string()
        }
    };
    let mut values = vec![
        ("ILLUSTASH_ROOT", compute.join("resources").into_os_string()),
        (
            "ILLUSTASH_MODELS",
            compute.join("resources/models").into_os_string(),
        ),
        ("ILLUSTASH_SHARED", shared),
    ];
    for (kind, key) in KINDS.into_iter().zip([
        "ILLUSTASH_RESEARCH_WORKER",
        "ILLUSTASH_INDEX_WORKER",
        "ILLUSTASH_PROVENANCE_WORKER",
        "ILLUSTASH_RECOGNITION_WORKER",
    ]) {
        values.push((
            key,
            compute
                .join(format!("bin/illustash-{kind}"))
                .into_os_string(),
        ));
    }
    Ok(values)
}

#[cfg(target_os = "macos")]
pub fn configure() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let contents = exe
        .parent()
        .and_then(Path::parent)
        .ok_or("无法定位应用安装目录")?;
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
    let compute = exe.parent().ok_or("无法定位安装目录")?.join("compute");
    for (key, value) in defaults(&compute, std::env::var_os("LOCALAPPDATA"))? {
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
            "bin/illustash-research",
            "bin/illustash-index",
            "bin/illustash-provenance",
            "bin/illustash-recognition",
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
            let helper = root.join("bin/illustash-provenance");
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
        assert!(defaults(&compute, None).unwrap().is_empty());
        assert!(defaults(&compute, Some("user data".into()))
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
        let error = defaults(&root, local()).unwrap_err();
        assert!(error.contains("manifest 不可用"), "{error}");
        assert!(error.contains("重新安装 Windows 完整版"), "{error}");
        for name in WINDOWS_FILES {
            let path = root.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, b"fixture").unwrap();
        }
        // 组件齐全也不能掩盖 manifest 缺失或损坏。
        assert!(defaults(&root, local())
            .unwrap_err()
            .contains("manifest 不可用"));
        for broken in ["invalid", "", "{\"format\":1,"] {
            fs::write(root.join("manifest.json"), broken).unwrap();
            assert!(defaults(&root, local())
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
            assert!(defaults(&root, local())
                .unwrap_err()
                .contains("manifest 类型不匹配"));
        }
        // 声明正确但 files 未登记必需组件。
        fs::write(
            root.join("manifest.json"),
            r#"{"format":1,"variant":"research","platform":"win32","files":{}}"#,
        )
        .unwrap();
        assert!(defaults(&root, local())
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
            let windows = defaults(&root, Some("user data".into()));
            let macos = macos_defaults(&root, None, Some("synthetic shared".into()));
            if windows_ok {
                assert_eq!(windows.unwrap().len(), 8);
                assert!(macos.unwrap_err().contains("macOS 完整版"));
            } else {
                assert!(windows.unwrap_err().contains("manifest 类型不匹配"));
                assert_eq!(macos.unwrap().len(), 7);
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
        assert!(defaults(&root, Some("user data".into()))
            .unwrap_err()
            .contains("resources/web/static/index.html"));
        for name in WINDOWS_FILES {
            let path = root.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, b"fixture").unwrap();
        }
        let settings = defaults(&root, Some("user data".into())).unwrap();
        let keys: Vec<_> = settings.iter().map(|(key, _)| *key).collect();
        assert_eq!(
            keys,
            [
                "ILLUSTASH_ROOT",
                "ILLUSTASH_MODELS",
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
            root.join("resources/models").into_os_string()
        );
        assert_eq!(
            settings[2].1,
            root.join("resources/openblas/openblas.dll")
                .into_os_string()
        );
        assert_eq!(
            settings[6].1,
            root.join("bin/illustash-recognition.exe").into_os_string()
        );
        assert_eq!(
            settings[7].1,
            Path::new("user data")
                .join("Illustash/Shared")
                .into_os_string()
        );
        assert!(settings
            .iter()
            .all(|(key, _)| !key.contains("NETWORK") && !key.contains("PYTHON")));
        assert!(defaults(&root, None).unwrap_err().contains("LOCALAPPDATA"));
        fs::remove_file(root.join("bin/illustash-recognition.exe")).unwrap();
        assert!(defaults(&root, Some("user data".into()))
            .unwrap_err()
            .contains("illustash-recognition.exe"));
        fs::remove_dir_all(scratch).unwrap();
    }
}
