fn main() {
    // The start page plays the library page's launch animation: one copy of splash.js, taken from web/static.
    println!("cargo:rerun-if-changed=../web/static/splash.js");
    std::fs::copy("../web/static/splash.js", "dist/splash.js").expect("copy web/static/splash.js to dist");
    // Only the Pixiv window (www.pixiv.net) may call pixiv_reply (capabilities/pixiv.json); only the main window's
    // local start page may call the start page commands (capabilities/main.json).
    const COMMANDS: &[&str] =
        &["pixiv_reply", "welcome_pick", "welcome_open", "welcome_create", "startup_choose", "startup_retry"];
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(tauri_build::AppManifest::new().commands(COMMANDS)))
        .expect("tauri build");
}
