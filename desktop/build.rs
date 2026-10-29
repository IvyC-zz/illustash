fn main() {
    // 启动页播放图库页的启动动画：只有一份 splash.js，从 web/static 取。
    println!("cargo:rerun-if-changed=../web/static/splash.js");
    std::fs::copy("../web/static/splash.js", "dist/splash.js")
        .expect("copy web/static/splash.js to dist");
    // 外壳和页面说同一种语言（#173）：与图库页用同一份 i18n.js 和同样的译文表。表编译进程序，所以资源缺失时
    // 菜单、选择框和启动错误也有翻译；启动页通过初始化脚本拿到选中的那份表（src/i18n.rs）。
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
    let out = std::path::Path::new(&std::env::var("OUT_DIR").expect("cargo sets OUT_DIR for build scripts")).join("locales.rs");
    std::fs::write(
        out,
        format!("pub(crate) const LOCALES: &[(&str, &str)] = &[\n{entries}];\n"),
    )
    .expect("write locales.rs");
    // 只有 Pixiv 窗口（www.pixiv.net）可以调用 pixiv_reply（capabilities/pixiv.json）；只有主窗口的本地启动页
    // 可以调用启动页的命令（capabilities/main.json）。
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
        "startup_rescue",
    ];
    tauri_build::try_build(
        tauri_build::Attributes::new()
            .app_manifest(tauri_build::AppManifest::new().commands(COMMANDS)),
    )
    .expect("tauri build");
}
