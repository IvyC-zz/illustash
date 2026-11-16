//! 记住主窗口的位置、大小和是否最大化（#451；macOS 不记最大化，见 `track`）。存在外壳自己的配置目录（`app_config_dir()/window.json`），不进图库；
//! Pixiv 登录窗口不记。移动、缩放停下来以后存一次，退出时再存一次，所以 App 崩溃也只丢最后一次改动。全屏不记：全屏
//! 期间不更新，下次按全屏前的大小打开。
//!
//! 坐标一律用物理像素，与 tauri 的 `outer_position`、`inner_size` 和显示器信息同一口径。打开时记下的窗口要还在某个
//! 显示器上才放回原处，否则按默认大小居中；比显示器大就缩到放得下。
use std::path::PathBuf;
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, Monitor, PhysicalPosition, PhysicalSize, WebviewWindow, WindowEvent};

const FILE: &str = "window.json";
/// 拖动、缩放时事件一秒几十个：停下来这么久以后才写文件。
const SETTLE: Duration = Duration::from_millis(800);
/// 记下的窗口至少要有这么大一块（物理像素）落在某个显示器的可用区域里，标题栏抓得住，才放回原处。
const VISIBLE: i64 = 48;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Saved {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    #[serde(default)]
    pub maximized: bool,
}

/// 一个显示器的可用区域（去掉菜单栏、Dock、任务栏），物理像素。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Area {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl From<&Monitor> for Area {
    fn from(monitor: &Monitor) -> Self {
        let rect = monitor.work_area();
        Self { x: rect.position.x, y: rect.position.y, width: rect.size.width, height: rect.size.height }
    }
}

fn overlap(a0: i64, a1: i64, b0: i64, b1: i64) -> i64 {
    (a1.min(b1) - a0.max(b0)).max(0)
}

/// 把记下的窗口放到现在的显示器上：找重叠最多的那个显示器，大小缩到放得下，位置挪进它的可用区域。哪个显示器上都
/// 露不出一块标题栏（显示器拔掉了、分辨率改了）就返回 `None`，由调用方按默认大小居中。
pub fn place(saved: Saved, areas: &[Area], min: (u32, u32)) -> Option<Saved> {
    let (x0, y0) = (i64::from(saved.x), i64::from(saved.y));
    let (x1, y1) = (x0 + i64::from(saved.width), y0 + i64::from(saved.height));
    // 标题栏在窗口顶上：只看顶部这一条有没有露出来。
    let title = (y0, y0 + VISIBLE);
    let area = areas
        .iter()
        .filter(|a| a.width > 0 && a.height > 0)
        .map(|a| {
            let (ax0, ay0) = (i64::from(a.x), i64::from(a.y));
            let (ax1, ay1) = (ax0 + i64::from(a.width), ay0 + i64::from(a.height));
            let shown = overlap(x0, x1, ax0, ax1) >= VISIBLE && overlap(title.0, title.1, ay0, ay1) > 0;
            (shown, overlap(x0, x1, ax0, ax1) * overlap(y0, y1, ay0, ay1), a)
        })
        .filter(|(shown, _, _)| *shown)
        .max_by_key(|(_, shared, _)| *shared)
        .map(|(_, _, a)| *a)?;
    let width = saved.width.max(min.0).min(area.width);
    let height = saved.height.max(min.1).min(area.height);
    let fit = |at: i32, start: i32, room: u32, size: u32| {
        let last = i64::from(start) + i64::from(room) - i64::from(size);
        i64::from(at).clamp(i64::from(start), last.max(i64::from(start))) as i32
    };
    Some(Saved {
        x: fit(saved.x, area.x, area.width, width),
        y: fit(saved.y, area.y, area.height, height),
        width,
        height,
        maximized: saved.maximized,
    })
}

/// 最近一次非全屏、非最小化时的窗口；最大化时保留最大化前的位置和大小，只记下「最大化了」。
#[derive(Default)]
pub struct Memory {
    last: Mutex<Option<Saved>>,
    save: Mutex<Option<Sender<()>>>,
}

fn path(app: &AppHandle) -> Option<PathBuf> {
    app.path().app_config_dir().ok().map(|dir| dir.join(FILE))
}

fn read(app: &AppHandle) -> Option<Saved> {
    let text = std::fs::read_to_string(path(app)?).ok()?;
    serde_json::from_str(&text).ok().filter(|s: &Saved| s.width > 0 && s.height > 0)
}

fn write(app: &AppHandle, saved: Saved) {
    let Some(path) = path(app) else { return };
    let Some(dir) = path.parent() else { return };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let temp = path.with_extension("json.tmp");
    let text = serde_json::to_string(&saved).expect("plain struct always serializes");
    if std::fs::write(&temp, text).is_ok() && std::fs::rename(&temp, &path).is_err() {
        let _ = std::fs::remove_file(&temp);
    }
}

/// 窗口建好、还没显示时调用：放回记下的位置和大小，然后开始跟踪。
pub fn restore(main: &WebviewWindow, min: (f64, f64)) {
    let app = main.app_handle();
    let scale = main.scale_factor().unwrap_or(1.0);
    let min = ((min.0 * scale).round() as u32, (min.1 * scale).round() as u32);
    let areas: Vec<Area> = main.available_monitors().unwrap_or_default().iter().map(Area::from).collect();
    match read(app).and_then(|saved| place(saved, &areas, min)) {
        Some(saved) => {
            let _ = main.set_position(PhysicalPosition::new(saved.x, saved.y));
            let _ = main.set_size(PhysicalSize::new(saved.width, saved.height));
            if saved.maximized {
                let _ = main.maximize();
            }
            *app.state::<Memory>().last.lock().expect("window memory lock") = Some(saved);
        }
        None => {
            let _ = main.center();
        }
    }
    let (sender, receiver) = channel::<()>();
    *app.state::<Memory>().save.lock().expect("window memory sender lock") = Some(sender);
    let handle = app.clone();
    thread::spawn(move || {
        while receiver.recv().is_ok() {
            // 等事件停下来再写。
            loop {
                match receiver.recv_timeout(SETTLE) {
                    Ok(()) => continue,
                    Err(RecvTimeoutError::Timeout) => break,
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            }
            save(&handle);
        }
    });
    let window = main.clone();
    main.on_window_event(move |event| {
        if matches!(event, WindowEvent::Moved(_) | WindowEvent::Resized(_)) {
            track(&window);
        }
    });
}

fn track(main: &WebviewWindow) {
    let app = main.app_handle();
    let hidden = !main.is_visible().unwrap_or(true);
    if hidden || main.is_minimized().unwrap_or(false) || main.is_fullscreen().unwrap_or(false) {
        return;
    }
    // macOS 没有真正的最大化：铺满屏幕的窗口也被报成「缩放」，记下来会在下次打开时多缩放一次。那边只记位置和大小。
    let maximized = cfg!(not(target_os = "macos")) && main.is_maximized().unwrap_or(false);
    let memory = app.state::<Memory>();
    {
        let mut last = memory.last.lock().expect("window memory lock");
        if maximized {
            match last.as_mut() {
                Some(saved) => saved.maximized = true,
                None => return,
            }
        } else {
            let (Ok(position), Ok(size)) = (main.outer_position(), main.inner_size()) else { return };
            *last = Some(Saved { x: position.x, y: position.y, width: size.width, height: size.height, maximized });
        }
    }
    let sender = memory.save.lock().expect("window memory sender lock").clone();
    if let Some(sender) = sender {
        let _ = sender.send(());
    }
}

/// 把最近一次的窗口写进文件；退出时也调用一次。
pub fn save(app: &AppHandle) {
    let Some(memory) = app.try_state::<Memory>() else { return };
    let last = *memory.last.lock().expect("window memory lock");
    if let Some(saved) = last {
        write(app, saved);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAIN: Area = Area { x: 0, y: 25, width: 2560, height: 1415 };
    const RIGHT: Area = Area { x: 2560, y: 0, width: 1920, height: 1040 };
    const MIN: (u32, u32) = (1440, 960);

    fn saved(x: i32, y: i32, width: u32, height: u32) -> Saved {
        Saved { x, y, width, height, maximized: false }
    }

    #[test]
    fn window_on_a_monitor_goes_back_where_it_was() {
        assert_eq!(place(saved(100, 120, 1600, 1000), &[MAIN, RIGHT], MIN), Some(saved(100, 120, 1600, 1000)));
        let right = saved(2700, 40, 1500, 980);
        assert_eq!(place(right, &[MAIN, RIGHT], MIN), Some(right));
        let maximized = Saved { maximized: true, ..right };
        assert_eq!(place(maximized, &[MAIN, RIGHT], MIN), Some(maximized));
    }

    #[test]
    fn unplugged_monitor_or_title_bar_out_of_reach_falls_back_to_default() {
        // 右边的显示器拔掉了。
        assert_eq!(place(saved(2700, 40, 1500, 980), &[MAIN], MIN), None);
        // 标题栏在屏幕上沿以上，抓不到。
        assert_eq!(place(saved(100, -2000, 1600, 1000), &[MAIN], MIN), None);
        // 只露出不到一个标题栏宽的一条边。
        assert_eq!(place(saved(2540, 100, 1600, 1000), &[MAIN], MIN), None);
        assert_eq!(place(saved(100, 120, 1600, 1000), &[], MIN), None);
        assert_eq!(place(saved(100, 120, 1600, 1000), &[Area { x: 0, y: 0, width: 0, height: 0 }], MIN), None);
    }

    #[test]
    fn too_large_or_partly_offscreen_is_pulled_into_the_monitor() {
        // 分辨率调小以后，窗口比显示器大：缩到可用区域，贴着左上角。
        assert_eq!(place(saved(0, 25, 3000, 2000), &[MAIN], MIN), Some(saved(0, 25, 2560, 1415)));
        // 一半在屏幕右边外面：整个挪回来。
        assert_eq!(place(saved(2000, 300, 1600, 1000), &[MAIN], MIN), Some(saved(960, 300, 1600, 1000)));
        // 跨两个显示器时放到重叠多的那个上。
        assert_eq!(place(saved(1800, 100, 1600, 960), &[MAIN, RIGHT], MIN), Some(saved(2560, 80, 1600, 960)));
        // 比最小尺寸小（手动改过文件）：放大到最小尺寸，但不超过显示器。
        assert_eq!(place(saved(10, 30, 200, 100), &[MAIN], MIN), Some(saved(10, 30, 1440, 960)));
        let small = Area { x: 0, y: 0, width: 1280, height: 800 };
        assert_eq!(place(saved(10, 30, 200, 100), &[small], MIN), Some(saved(0, 0, 1280, 800)));
    }
}
