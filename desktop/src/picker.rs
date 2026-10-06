//! Native platform picker. Called on bridge worker threads, never on the event loop.
use serde_json::{json, Value};
use tauri::{AppHandle, Manager};
use tauri_plugin_dialog::DialogExt;

pub fn choose(app: &AppHandle, kind: &str, multiple: bool) -> Result<Value, String> {
    let title = match kind {
        "folder" => "选择图片文件夹",
        "library" => "选择图库（.pilib）",
        "zip" => "选择导出的图库（.zip）",
        "archive" => "选择 X 数据归档（.zip）",
        _ => return Err("未知的选择类型".into()),
    };
    if multiple && kind != "folder" {
        return Err("此类型不支持多选".into());
    }
    let mut dialog = app.dialog().file().set_title(title);
    if let Some(main) = app.get_webview_window("main") {
        dialog = dialog.set_parent(&main);
    }
    let selected = match kind {
        "folder" if multiple => dialog.blocking_pick_folders(),
        "folder" | "library" => dialog.blocking_pick_folder().map(|p| vec![p]),
        _ => dialog.add_filter("ZIP", &["zip"]).blocking_pick_file().map(|p| vec![p]),
    };
    let paths: Vec<String> = selected.unwrap_or_default().into_iter().map(|p| {
        p.into_path().map_err(|_| "选择结果不是本地路径".to_string())?
            .into_os_string().into_string().map_err(|_| "选择路径不是 UTF-8".to_string())
    }).collect::<Result<_, _>>()?;
    // No guessing, filesystem reads, or logs of selected private paths.
    Ok(json!({"paths": paths}))
}
