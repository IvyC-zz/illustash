//! Commands of the start page (#107): the welcome page when no library can be opened, choosing another library
//! while one is loading, and retry / choose / create after a failure. Only the main window's local start page
//! may call them (capabilities/main.json); the library page on 127.0.0.1 cannot.
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde::Deserialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Manager, State};

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

/// What each picked folder holds (readable, how many pictures, already covered by another), read-only; only the
/// folders in `count` are counted again.
#[tauri::command(async)]
pub fn welcome_inspect(paths: Vec<String>, count: Vec<String>) -> Vec<Value> {
    let paths: Vec<PathBuf> = paths.into_iter().map(PathBuf::from).collect();
    let count: Vec<PathBuf> = count.into_iter().map(PathBuf::from).collect();
    illustash_terms::desktop_shell::inspect(&paths, &count)
}

/// The welcome page's archive import: one at a time, cancellable until the new library is published.
#[derive(Default)]
pub struct Import {
    cancel: Mutex<Option<Arc<AtomicBool>>>,
    state: Arc<Mutex<Value>>,
}

/// Import an exported library (.zip) into the libraries home, then open it. The page polls `welcome_import_status`.
#[tauri::command(async)]
pub fn welcome_import(app: AppHandle, import: State<'_, Import>, path: String) -> Result<(), String> {
    let cancel = {
        let mut slot = import.cancel.lock().unwrap();
        if slot.is_some() {
            return Err("已经在导入另一个图库".into());
        }
        slot.insert(Arc::new(AtomicBool::new(false))).clone()
    };
    *import.state.lock().unwrap() = json!({"stage": "reading"});
    let state = import.state.clone();
    let report = move |stage: &'static str, count: Option<(u64, u64)>| {
        let mut state = state.lock().unwrap();
        match count {
            Some((done, total)) => {
                state["done"] = done.into();
                state["total"] = total.into();
            }
            None => *state = json!({"stage": stage}),
        }
    };
    let result = illustash_terms::desktop_shell::import(&PathBuf::from(path), &cancel, &report);
    import.cancel.lock().unwrap().take();
    match result {
        Ok(library) => {
            server::restart(&app, Some(library));
            Ok(())
        }
        Err(_) if cancel.load(Ordering::SeqCst) => Err("已取消导入".into()),
        Err(error) => Err(error.to_string()),
    }
}

/// The running import's stage and byte counts (`{}` when none runs).
#[tauri::command]
pub fn welcome_import_status(import: State<'_, Import>) -> Value {
    if import.cancel.lock().unwrap().is_some() { import.state.lock().unwrap().clone() } else { json!({}) }
}

/// Stop the running import before its library is published; what it unpacked is removed.
#[tauri::command]
pub fn welcome_import_cancel(import: State<'_, Import>) {
    if let Some(cancel) = import.cancel.lock().unwrap().as_ref() {
        cancel.store(true, Ordering::SeqCst);
    }
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
