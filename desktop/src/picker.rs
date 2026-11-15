//! 平台原生的选择框。在 bridge 的 worker 线程上调用，从不在事件循环上调用。
use serde_json::{json, Value};
use tauri::{AppHandle, Manager};
use tauri_plugin_dialog::DialogExt;

use crate::i18n::tr;

pub fn choose(app: &AppHandle, kind: &str, multiple: bool) -> Result<Value, String> {
    let title = match kind {
        "folder" => tr("选择图片文件夹"),
        "library" => tr("选择图库（.pilib）"),
        "zip" => tr("选择导出的图库（.zip）"),
        "archive" => tr("选择 X 数据归档（.zip）"),
        "models" => tr("选择下载好的模型文件（.onnx、.json 或 .zip）"),
        "human-conclusions" => tr("选择人工结论文件（.illustash-human.jsonl）"),
        _ => return Err(tr("未知的选择类型")),
    };
    if multiple && !matches!(kind, "folder" | "models") {
        return Err(tr("此类型不支持多选"));
    }
    let mut dialog = app.dialog().file().set_title(title);
    if let Some(main) = app.get_webview_window("main") {
        dialog = dialog.set_parent(&main);
    }
    let selected = match kind {
        "folder" if multiple => dialog.blocking_pick_folders(),
        "folder" | "library" => dialog.blocking_pick_folder().map(|p| vec![p]),
        "models" => dialog.add_filter(tr("模型文件"), &["onnx", "json", "zip"]).blocking_pick_files(),
        "human-conclusions" => dialog.add_filter(tr("人工结论"), &["jsonl"]).blocking_pick_file().map(|p| vec![p]),
        _ => dialog.add_filter("ZIP", &["zip"]).blocking_pick_file().map(|p| vec![p]),
    };
    let paths: Vec<String> = selected
        .unwrap_or_default()
        .into_iter()
        .map(|p| {
            p.into_path()
                .map_err(|_| tr("选择结果不是本地路径"))?
                .into_os_string()
                .into_string()
                .map_err(|_| tr("选择路径不是 UTF-8"))
        })
        .collect::<Result<_, _>>()?;
    // 不猜测、不读文件系统，也不记录选中的私人路径。
    Ok(json!({"paths": paths}))
}
