//! Commands of the start page (#107): the welcome page when no library can be opened, choosing another library
//! while one is loading, and retry / choose / create after a failure. Only the main window's local start page
//! may call them (capabilities/main.json); the library page on 127.0.0.1 cannot.
use std::path::PathBuf;

use serde::Deserialize;
use serde_json::Value;
use tauri::{AppHandle, Manager};

use crate::{picker, server};

#[derive(Deserialize)]
pub struct Source {
    kind: String,
    path: Option<String>,
    preset: Option<String>,
}

/// Native folder picker for the welcome page (several folders at once for a source).
#[tauri::command(async)]
pub fn welcome_pick(app: AppHandle, kind: String, multiple: bool) -> Result<Value, String> {
    picker::choose(&app, &kind, multiple)
}

/// Open an existing library: a fresh start page, then the library page.
#[tauri::command(async)]
pub fn welcome_open(app: AppHandle, path: String) -> Result<(), String> {
    let path = PathBuf::from(path);
    if !illustash_terms::desktop_shell::is_library(&path) {
        return Err("这个文件夹不是图库（找不到 library.json）".into());
    }
    server::restart(&app, Some(path));
    Ok(())
}

/// Create a library from the folders picked per origin (and Photos on macOS), then open it.
#[tauri::command(async)]
pub fn welcome_create(app: AppHandle, name: String, sources: Vec<Source>) -> Result<(), String> {
    use illustash_terms::desktop_shell::Pick;
    let picks = sources
        .into_iter()
        .map(|s| match s.kind.as_str() {
            "folder" => Ok(Pick::Folder {
                path: PathBuf::from(s.path.unwrap_or_default()),
                preset: s.preset.unwrap_or_default(),
            }),
            "photos" => Ok(Pick::Photos),
            _ => Err("请选择来源".to_string()),
        })
        .collect::<Result<Vec<_>, _>>()?;
    let root = server::resource_root(&app).ok_or("应用资源不可用；请重新启动应用")?;
    let library = illustash_terms::desktop_shell::create(&root, &name, picks).map_err(|e| e.to_string())?;
    server::restart(&app, Some(library));
    Ok(())
}

/// Stop the library being opened (or the one that failed) and show the welcome page.
#[tauri::command(async)]
pub fn startup_choose(app: AppHandle, create: bool) {
    if let Some(main) = app.get_webview_window("main") {
        server::welcome(&app, &main, create);
    }
}

/// Start the same library again after a failure.
#[tauri::command(async)]
pub fn startup_retry(app: AppHandle) {
    server::restart(&app, None);
}
