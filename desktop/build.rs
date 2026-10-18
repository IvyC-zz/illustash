fn main() {
    // The start page plays the library page's launch animation: one copy of splash.js, taken from web/static.
    println!("cargo:rerun-if-changed=../web/static/splash.js");
    std::fs::copy("../web/static/splash.js", "dist/splash.js")
        .expect("copy web/static/splash.js to dist");
    // The shell speaks the page's language (#173): the same i18n.js and the same tables as the library page. The
    // tables are compiled in, so the menu, the pickers and the startup errors are translated even when the
    // resources are missing; the start page gets the chosen one from an initialization script (src/i18n.rs).
    println!("cargo:rerun-if-changed=../web/static/i18n.js");
    std::fs::copy("../web/static/i18n.js", "dist/i18n.js")
        .expect("copy web/static/i18n.js to dist");
    println!("cargo:rerun-if-changed=../web/static/locales");
    let folder = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../web/static/locales");
    let mut tables: Vec<(String, std::path::PathBuf)> = std::fs::read_dir(&folder)
        .expect("read web/static/locales")
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            let code = path
                .file_name()?
                .to_str()?
                .strip_suffix(".json")?
                .to_owned();
            Some((code, path))
        })
        .collect();
    tables.sort();
    let entries: String = tables
        .iter()
        .map(|(code, path)| {
            format!(
                "    ({code:?}, include_str!({:?})),\n",
                path.to_str().expect("UTF-8 path")
            )
        })
        .collect();
    let out = std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("locales.rs");
    std::fs::write(
        out,
        format!("pub(crate) const LOCALES: &[(&str, &str)] = &[\n{entries}];\n"),
    )
    .expect("write locales.rs");
    // Only the Pixiv window (www.pixiv.net) may call pixiv_reply (capabilities/pixiv.json); only the main window's
    // local start page may call the start page commands (capabilities/main.json).
    const COMMANDS: &[&str] = &[
        "pixiv_reply",
        "welcome_pick",
        "welcome_open",
        "welcome_create",
        "welcome_inspect",
        "welcome_import",
        "welcome_import_status",
        "welcome_import_cancel",
        "startup_choose",
        "startup_retry",
        "startup_upgrade",
    ];
    tauri_build::try_build(
        tauri_build::Attributes::new()
            .app_manifest(tauri_build::AppManifest::new().commands(COMMANDS)),
    )
    .expect("tauri build");
}
