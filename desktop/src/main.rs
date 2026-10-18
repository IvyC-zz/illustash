//! 桌面外壳复用 Rust CLI 的原生 HTTP 前门，并提供原生账号使用的 Pixiv 登录桥。
//! 核心命令直接执行、不创建窗口；窗口关闭等待宿主任务收尾。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod bridge;
#[cfg(any(windows, target_os = "macos", test))]
mod bundled_workers;
mod i18n;
mod pixiv;
mod picker;
mod server;
mod welcome;

#[cfg(not(windows))]
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::{Manager, RunEvent, WebviewUrl, WebviewWindowBuilder, WindowEvent};

fn main() {
    #[cfg(any(windows, target_os = "macos"))]
    let computation = bundled_workers::configure();
    #[cfg(not(any(windows, target_os = "macos")))]
    let computation: Result<(), String> = Ok(());
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|a| illustash_terms::cli::is_command(a)) {
        if let Err(error) = &computation {
            eprintln!("{error}");
            std::process::exit(2);
        }
        std::process::exit(illustash_terms::cli::run(&args).into());
    }
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(pixiv::Pixiv::default())
        .manage(server::Child::default())
        .manage(server::ErrorPage::default())
        .manage(server::StartupPage::default())
        .manage(server::Launch::default())
        .manage(welcome::Import::default())
        .manage(welcome::Requests::default())
        .invoke_handler(tauri::generate_handler![
            pixiv::pixiv_reply,
            welcome::welcome_pick,
            welcome::welcome_open,
            welcome::welcome_create,
            welcome::welcome_inspect,
            welcome::welcome_import,
            welcome::welcome_import_status,
            welcome::welcome_import_cancel,
            welcome::startup_choose,
            welcome::startup_retry
        ])
        .on_menu_event(|app, event| {
            if event.id() == "reload" {
                // Only the library page: reloading the Pixiv window would cut a request in flight.
                if let Some(main) = app.get_webview_window("main") {
                    let _ = main.reload();
                }
            }
        })
        .setup(move |app| {
            let handle = app.handle().clone();
            #[cfg(not(windows))]
            {
                // macOS keeps its native editing/quit menus and refresh command.
                let menu = Menu::default(&handle)?;
                let reload = MenuItem::with_id(app, "reload", i18n::tr("刷新页面"), true, Some("CmdOrCtrl+R"))?;
                // The default menu already has a View menu (full screen): refresh joins it instead of a second one.
                let view = menu
                    .items()?
                    .into_iter()
                    .filter_map(|item| item.as_submenu().cloned())
                    .find(|submenu| submenu.text().is_ok_and(|text| text == "View"));
                match view {
                    Some(view) => {
                        view.append(&PredefinedMenuItem::separator(app)?)?;
                        view.append(&reload)?;
                    }
                    None => menu.append(&Submenu::with_items(app, i18n::tr("视图"), true, &[&reload])?)?,
                }
                app.set_menu(menu)?;
            }
            let main = WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
                .title("Illustash")
                .initialization_script(i18n::page_script())
                .inner_size(1280.0, 840.0)
                .min_inner_size(720.0, 480.0)
                .on_page_load(|window, payload| {
                    if payload.event() == tauri::webview::PageLoadEvent::Finished { server::page_loaded(&window); }
                })
                .on_navigation(server::keep_inside)
                .on_new_window(|url, _| {
                    server::open_outside(&url);
                    tauri::webview::NewWindowResponse::Deny
                })
                .build()?;
            if let Err(error) = &computation {
                server::blocked(&main, error);
                return Ok(());
            }
            let token = bridge::token();
            let port = match bridge::start(handle.clone(), token.clone()) {
                Ok(port) => port,
                Err(error) => {
                    server::blocked(&main, &i18n::tr_with("无法启动本机登录通道：{error}；请重新启动应用", &[("error", &error)]));
                    return Ok(());
                }
            };
            server::boot(handle, main, port, token);
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("启动桌面外壳失败");
    app.run(|handle, event| {
        match event {
            RunEvent::WindowEvent { label, event: WindowEvent::CloseRequested { api, .. }, .. }
                if label == "main" => {
                    api.prevent_close();
                    handle.exit(0); // Hidden Pixiv windows must not keep the service alive.
                }
            RunEvent::Exit => {
                if let Some(child) = handle.try_state::<server::Child>() {
                    child.stop();
                }
            }
            _ => {}
        }
    });
}
