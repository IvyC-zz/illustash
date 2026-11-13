//! 启动页的命令（#107）：没有能打开的图库时的欢迎页，加载一个图库时改选另一个，以及失败后的重试 / 选择 / 新建。
//! 只有主窗口的本地启动页可以调用它们（capabilities/main.json）；127.0.0.1 上的图库页不能。
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

/// 欢迎页用的原生文件夹选择框（一个来源可以一次选多个文件夹）。
#[tauri::command(async)]
pub fn welcome_pick(app: AppHandle, kind: String, multiple: bool) -> Result<Value, String> {
    picker::choose(&app, &kind, multiple)
}

/// 打开已有的图库：先显示新的启动页，再进入图库页。
#[tauri::command(async)]
pub fn welcome_open(app: AppHandle, path: String) -> Result<(), String> {
    let path = PathBuf::from(path);
    if !illustash_terms::desktop_shell::is_library(&path) {
        return Err(tr("这个文件夹不是图库（找不到 library.json）"));
    }
    server::restart(&app, Some(path));
    Ok(())
}

/// 页面给每次新建和导入起一个请求编号。Tauri 的自定义协议请求失败时会经 postMessage 再发一次调用，
/// 而这两个命令都在回应之前让页面跳转，可能使那个请求失败（#115）：同一编号第二次送达时不再执行，
/// 而是等第一次执行完，拿它的结果。
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
            let mut seen = self.seen.lock().expect("lock poisoned by an earlier panic");
            if seen.iter().any(|(s, _)| s == id) {
                eprintln!("[welcome] 同一请求再次送达，不重复执行");
                loop {
                    if let Some((_, Some(answer))) = seen.iter().find(|(s, _)| s == id) {
                        return answer.clone();
                    }
                    seen = self.done.wait(seen).expect("lock poisoned by an earlier panic");
                }
            }
            // 同一时间只运行一个新建或导入；保留最近几个已回应的。
            while seen.len() >= 8 {
                let Some(i) = seen.iter().position(|(_, answer)| answer.is_some()) else {
                    break;
                };
                seen.remove(i);
            }
            seen.push_back((id.to_owned(), None));
        }
        let answer = work();
        if let Some(entry) =
            self.seen.lock().expect("lock poisoned by an earlier panic").iter_mut().find(|(s, _)| s == id)
        {
            entry.1 = Some(answer.clone());
        }
        self.done.notify_all();
        answer
    }
}

/// 用按出处选的文件夹（macOS 上还有照片）新建图库，然后打开它。如果只是登记进最近图库失败了，
/// 就改为回应 `{created, error}`：页面提供打开已建好的图库的选项。
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

/// 每个选中的文件夹里有什么（能否读取、有多少张图、是否已被另一个覆盖），只读；只有 `count` 里的文件夹会重新计数。
#[tauri::command(async)]
pub fn welcome_inspect(paths: Vec<String>, count: Vec<String>) -> Vec<Value> {
    let paths: Vec<PathBuf> = paths.into_iter().map(PathBuf::from).collect();
    let count: Vec<PathBuf> = count.into_iter().map(PathBuf::from).collect();
    illustash_terms::desktop_shell::inspect(&paths, &count)
}

/// 欢迎页的导出包导入：一次一个，在新图库发布之前都可以取消。
#[derive(Default)]
pub struct Import {
    cancel: Mutex<Option<Arc<AtomicBool>>>,
    state: Arc<Mutex<Value>>,
}

/// 把导出的图库（.zip）导入到存放图库的目录，然后打开它。页面轮询 `welcome_import_status`。
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
        let mut slot = import.cancel.lock().expect("lock poisoned by an earlier panic");
        if slot.is_some() {
            return Err(tr("已经在导入另一个图库"));
        }
        slot.insert(Arc::new(AtomicBool::new(false))).clone()
    };
    *import.state.lock().expect("lock poisoned by an earlier panic") = json!({"stage": "reading"});
    let state = import.state.clone();
    let report = move |stage: &'static str, count: Option<(u64, u64)>| {
        let mut state = state.lock().expect("lock poisoned by an earlier panic");
        match count {
            Some((done, total)) => {
                state["done"] = done.into();
                state["total"] = total.into();
            }
            None => *state = json!({"stage": stage}),
        }
    };
    let result = illustash_terms::desktop_shell::import(&PathBuf::from(path), &cancel, &report);
    import.cancel.lock().expect("lock poisoned by an earlier panic").take();
    match result {
        Ok(library) => {
            server::restart(app, Some(library));
            Ok(())
        }
        Err(_) if cancel.load(Ordering::SeqCst) => Err(tr("已取消导入")),
        Err(error) => Err(error.to_string()),
    }
}

/// 正在进行的导入的阶段和字节数（没有导入在运行时为 `{}`）。
#[tauri::command]
pub fn welcome_import_status(import: State<'_, Import>) -> Value {
    if import.cancel.lock().expect("lock poisoned by an earlier panic").is_some() {
        import.state.lock().expect("lock poisoned by an earlier panic").clone()
    } else {
        json!({})
    }
}

/// 在图库发布之前停止正在进行的导入；已解压的内容会被删除。
#[tauri::command]
pub fn welcome_import_cancel(import: State<'_, Import>) {
    if let Some(cancel) = import.cancel.lock().expect("lock poisoned by an earlier panic").as_ref() {
        cancel.store(true, Ordering::SeqCst);
    }
}

/// 停止正在打开的图库（或打开失败的那个），显示欢迎页。
#[tauri::command(async)]
pub fn startup_choose(app: AppHandle, create: bool) {
    if let Some(main) = app.get_webview_window("main") {
        server::welcome(&app, &main, create);
    }
}

/// 失败后再次启动同一个图库。
#[tauri::command(async)]
pub fn startup_retry(app: AppHandle) {
    server::restart(&app, None);
}

/// 用户在启动页上同意后，升级打开失败的图库（#193）。
#[tauri::command(async)]
pub fn startup_upgrade(app: AppHandle) {
    server::upgrade(&app);
}

/// 用户在启动页上选了「抢救重建」后，抢救图库里损坏的 source.sqlite（#228）。
#[tauri::command(async)]
pub fn startup_rescue(app: AppHandle) {
    server::rescue(&app);
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
        // 第二次送达时第一次还在运行：它等待，而不是再建一个图库。
        let second = {
            let (requests, runs) = (requests.clone(), runs.clone());
            std::thread::spawn(move || requests.once("r1", || Ok(json!({"made": runs.fetch_add(1, Ordering::SeqCst)}))))
        };
        std::thread::sleep(std::time::Duration::from_millis(50));
        release.store(true, Ordering::SeqCst);
        assert_eq!(first.join().unwrap(), Ok(json!({"made": 1})));
        assert_eq!(second.join().unwrap(), Ok(json!({"made": 1})));
        // 在第一次已回应之后送达（包括失败的回应），同样不再执行。
        assert_eq!(requests.once("r1", || unreachable!()), Ok(json!({"made": 1})));
        assert_eq!(requests.once("r2", || Err("坏包".into())), Err("坏包".to_string()));
        assert_eq!(requests.once("r2", || unreachable!()), Err("坏包".to_string()));
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        assert!(requests.once("", || unreachable!()).is_err());
        // 新的一次点击是新的请求。
        assert_eq!(requests.once("r3", || Ok(Value::Null)), Ok(Value::Null));
        for i in 0..20 {
            requests.once(&format!("x{i}"), || Ok(Value::Null)).unwrap();
        }
        assert!(requests.seen.lock().expect("lock poisoned by an earlier panic").len() <= 8);
    }
}
