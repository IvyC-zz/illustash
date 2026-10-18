//! Commands of the start page (#107): the welcome page when no library can be opened, choosing another library
//! while one is loading, and retry / choose / create after a failure. Only the main window's local start page
//! may call them (capabilities/main.json); the library page on 127.0.0.1 cannot.
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use serde::Deserialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Manager, State};

use crate::i18n::tr;
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
        return Err(tr("这个文件夹不是图库（找不到 library.json）"));
    }
    server::restart(&app, Some(path));
    Ok(())
}

/// The page names each create and import with a request id. Tauri sends a call again over postMessage when its
/// custom-protocol request fails, and both commands navigate the page before they answer, which can fail that
/// request (#115): a second delivery of the same id does not run again, it waits for the first and gets its answer.
type Answer = Result<Value, String>;

#[derive(Default)]
pub struct Requests {
    seen: Mutex<VecDeque<(String, Option<Answer>)>>,
    done: Condvar,
}

impl Requests {
    fn once(&self, id: &str, work: impl FnOnce() -> Answer) -> Answer {
        if id.is_empty() {
            return Err(tr("缺少请求编号；请重新启动应用"));
        }
        {
            let mut seen = self.seen.lock().unwrap();
            if seen.iter().any(|(s, _)| s == id) {
                eprintln!("[welcome] 同一请求再次送达，不重复执行");
                loop {
                    if let Some((_, Some(answer))) = seen.iter().find(|(s, _)| s == id) {
                        return answer.clone();
                    }
                    seen = self.done.wait(seen).unwrap();
                }
            }
            // Only one create or import runs at a time; keep the last few answered ones.
            while seen.len() >= 8 {
                let Some(i) = seen.iter().position(|(_, answer)| answer.is_some()) else {
                    break;
                };
                seen.remove(i);
            }
            seen.push_back((id.to_owned(), None));
        }
        let answer = work();
        if let Some(entry) = self.seen.lock().unwrap().iter_mut().find(|(s, _)| s == id) {
            entry.1 = Some(answer.clone());
        }
        self.done.notify_all();
        answer
    }
}

/// Create a library from the folders picked per origin (and Photos on macOS), then open it. When only listing it
/// among the recent libraries failed, answer `{created, error}` instead: the page offers to open what was made.
#[tauri::command(async)]
pub fn welcome_create(
    app: AppHandle,
    requests: State<'_, Requests>,
    request: String,
    name: String,
    sources: Vec<Source>,
) -> Result<Value, String> {
    requests.once(&request, || create(&app, name, sources))
}

fn create(app: &AppHandle, name: String, sources: Vec<Source>) -> Result<Value, String> {
    use illustash_terms::desktop_shell::Pick;
    let picks = sources
        .into_iter()
        .map(|s| match s.kind.as_str() {
            "folder" => Ok(Pick::Folder {
                path: PathBuf::from(s.path.unwrap_or_default()),
                preset: s.preset.unwrap_or_default(),
            }),
            "photos" => Ok(Pick::Photos),
            _ => Err(tr("请选择来源")),
        })
        .collect::<Result<Vec<_>, _>>()?;
    let root = server::resource_root(app).ok_or_else(|| tr("应用资源不可用；请重新启动应用"))?;
    let created = illustash_terms::desktop_shell::create(&root, &name, picks).map_err(|e| e.to_string())?;
    if let Some(error) = created.unlisted {
        return Ok(json!({"created": created.path, "error": error}));
    }
    server::restart(app, Some(created.path));
    Ok(json!({}))
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
pub fn welcome_import(
    app: AppHandle,
    import: State<'_, Import>,
    requests: State<'_, Requests>,
    request: String,
    path: String,
) -> Result<(), String> {
    requests.once(&request, || import_archive(&app, &import, path).map(|()| Value::Null)).map(|_| ())
}

fn import_archive(app: &AppHandle, import: &Import, path: String) -> Result<(), String> {
    let cancel = {
        let mut slot = import.cancel.lock().unwrap();
        if slot.is_some() {
            return Err(tr("已经在导入另一个图库"));
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
            server::restart(app, Some(library));
            Ok(())
        }
        Err(_) if cancel.load(Ordering::SeqCst) => Err(tr("已取消导入")),
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn a_request_delivered_twice_runs_once_and_both_get_its_answer() {
        let requests = Arc::new(Requests::default());
        let runs = Arc::new(AtomicUsize::new(0));
        let (started, release) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
        let first = {
            let (requests, runs, started, release) = (requests.clone(), runs.clone(), started.clone(), release.clone());
            std::thread::spawn(move || {
                requests.once("r1", || {
                    runs.fetch_add(1, Ordering::SeqCst);
                    started.store(true, Ordering::SeqCst);
                    while !release.load(Ordering::SeqCst) {
                        std::thread::yield_now();
                    }
                    Ok(json!({"made": 1}))
                })
            })
        };
        while !started.load(Ordering::SeqCst) {
            std::thread::yield_now();
        }
        // The second delivery arrives while the first still runs: it waits instead of making a second library.
        let second = {
            let (requests, runs) = (requests.clone(), runs.clone());
            std::thread::spawn(move || requests.once("r1", || Ok(json!({"made": runs.fetch_add(1, Ordering::SeqCst)}))))
        };
        std::thread::sleep(std::time::Duration::from_millis(50));
        release.store(true, Ordering::SeqCst);
        assert_eq!(first.join().unwrap(), Ok(json!({"made": 1})));
        assert_eq!(second.join().unwrap(), Ok(json!({"made": 1})));
        // Arriving after the first answered, failures included, it is not run either.
        assert_eq!(requests.once("r1", || unreachable!()), Ok(json!({"made": 1})));
        assert_eq!(requests.once("r2", || Err("坏包".into())), Err("坏包".to_string()));
        assert_eq!(requests.once("r2", || unreachable!()), Err("坏包".to_string()));
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        assert!(requests.once("", || unreachable!()).is_err());
        // A new click is a new request.
        assert_eq!(requests.once("r3", || Ok(Value::Null)), Ok(Value::Null));
        for i in 0..20 {
            requests.once(&format!("x{i}"), || Ok(Value::Null)).unwrap();
        }
        assert!(requests.seen.lock().unwrap().len() <= 8);
    }
}
