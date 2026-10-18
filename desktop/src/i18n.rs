//! The shell's interface language (#173, docs/WEB_V2.md「界面语言」): the app-wide setting in preferences.json
//! beside libraries.json, and the tables of web/static/locales, compiled in by build.rs. `tr("中文原文")` translates
//! the shell's own text (menu, pickers, startup errors); the start page gets i18n.js and the chosen table from
//! `page_script`. The language is settled once per launch: a change in the settings shows in the shell the next time
//! its window opens. `system` follows the system's preferred languages here, by i18n.js's rule, and the start page is
//! told the result, so the menu, the pickers and the start page always agree.
use std::sync::OnceLock;

use serde_json::{Map, Value};

include!(concat!(env!("OUT_DIR"), "/locales.rs"));

const SYSTEM: &str = "system";
/// Chinese is the source text: it has no table.
const SOURCE: &str = "zh-CN";

struct Locale {
    code: &'static str,
    name: String,
    table: Map<String, Value>,
}

/// The same rule as native/terms/src/app_preferences.rs: `en`, `ja`, `pt-BR`, the case of the file name.
fn valid_code(code: &str) -> bool {
    let mut parts = code.split('-');
    let primary = parts.next().unwrap_or("");
    code != SOURCE
        && (2..=3).contains(&primary.len())
        && primary.bytes().all(|b| b.is_ascii_lowercase())
        && parts.all(|part| (1..=8).contains(&part.len()) && part.bytes().all(|b| b.is_ascii_alphanumeric()))
}

fn parse(files: &[(&'static str, &'static str)]) -> Vec<Locale> {
    files
        .iter()
        .filter(|(code, _)| valid_code(code))
        .filter_map(|&(code, text)| {
            let table = serde_json::from_str::<Map<String, Value>>(text)
                .inspect_err(|e| eprintln!("[i18n] locales/{code}.json: {e}"))
                .ok()?;
            let name = table.get("@name").and_then(Value::as_str).filter(|n| !n.is_empty()).unwrap_or(code).into();
            Some(Locale { code, name, table })
        })
        .collect()
}

fn locales() -> &'static [Locale] {
    static LOCALES_PARSED: OnceLock<Vec<Locale>> = OnceLock::new();
    LOCALES_PARSED.get_or_init(|| parse(LOCALES))
}

/// The setting as the library page reads it: a missing, unreadable or hand-broken file, or a language whose table is
/// gone, means `system`.
fn preference(text: Option<&str>, codes: &[&str]) -> String {
    let value = text.and_then(|t| serde_json::from_str::<Value>(t).ok());
    match value.as_ref().and_then(|v| v.get("language")).and_then(Value::as_str) {
        Some(language) if language == SYSTEM || language == SOURCE || codes.contains(&language) => language.into(),
        _ => SYSTEM.into(),
    }
}

fn preference_file() -> Option<String> {
    let home = illustash_platform::app_paths::user_home().ok()?;
    std::fs::read_to_string(illustash_platform::app_paths::app_support(&home).join("preferences.json")).ok()
}

/// web/static/i18n.js's rule for `system`: the first wanted language that has a table, by its exact code, then by its
/// primary language (en-GB → en); any Chinese is 简体中文; nothing matching is English when there is a table.
fn resolve(preference: &str, wanted: &[String], codes: &[&str]) -> String {
    if preference == SOURCE || codes.contains(&preference) {
        return preference.into();
    }
    // Like navigator.language when the system names none.
    let fallback = ["zh".to_owned()];
    let wanted = if wanted.is_empty() { &fallback[..] } else { wanted };
    for tag in wanted.iter().map(|w| w.to_ascii_lowercase()) {
        let primary = tag.split('-').next().unwrap_or("");
        if let Some(code) = codes.iter().find(|c| c.to_ascii_lowercase() == tag) {
            return (*code).into();
        }
        if primary == "zh" {
            return SOURCE.into();
        }
        if let Some(code) = codes.iter().find(|c| c.to_ascii_lowercase().split('-').next() == Some(primary)) {
            return (*code).into();
        }
    }
    if codes.contains(&"en") { "en".into() } else { SOURCE.into() }
}

/// The language the shell shows for this launch: `zh-CN` or a table's code.
pub fn language() -> &'static str {
    // Unit tests never read the person's settings or system; they see the Chinese the existing tests expect.
    if cfg!(test) {
        return SOURCE;
    }
    static LANGUAGE: OnceLock<String> = OnceLock::new();
    LANGUAGE.get_or_init(|| {
        let codes: Vec<&str> = locales().iter().map(|l| l.code).collect();
        resolve(&preference(preference_file().as_deref(), &codes), &system_languages(), &codes)
    })
}

fn table() -> Option<&'static Map<String, Value>> {
    let language = language();
    locales().iter().find(|l| l.code == language).map(|l| &l.table)
}

fn lookup<'a>(table: Option<&'a Map<String, Value>>, text: &'a str) -> &'a str {
    let entry = table.and_then(|t| t.get(text));
    // Plural forms pick by {n} on the page; the shell has no counts, so it takes `other`.
    let found = entry.and_then(|e| e.as_str().or_else(|| e.get("other").and_then(Value::as_str)));
    found.filter(|s| !s.is_empty()).unwrap_or(text)
}

fn fill(text: &str, params: &[(&str, &dyn std::fmt::Display)]) -> String {
    let mut out = text.to_owned();
    for (name, value) in params {
        out = out.replace(&format!("{{{name}}}"), &value.to_string());
    }
    out
}

/// The shell's text in the language of this launch; the Chinese itself when there is no translation. Always called
/// with a literal, so tests/i18n.test.cjs can check it against the tables.
pub fn tr(text: &str) -> String {
    lookup(table(), text).to_owned()
}

/// `tr` with `{name}` placeholders: `tr_with("无法启动本地服务：{error}", &[("error", &e)])`.
pub fn tr_with(text: &str, params: &[(&str, &dyn std::fmt::Display)]) -> String {
    fill(lookup(table(), text), params)
}

/// The initialization script of the main window: on the shell's own start page only (never the library page on
/// 127.0.0.1, which gets /i18n.js from the service), the three values /i18n.js starts with, for dist/i18n.js.
pub fn page_script() -> String {
    script(language(), locales())
}

fn script(language: &str, locales: &[Locale]) -> String {
    let names: Map<String, Value> = locales.iter().map(|l| (l.code.to_owned(), l.name.clone().into())).collect();
    let tables: Map<String, Value> = locales
        .iter()
        .filter(|l| l.code == language)
        .map(|l| (l.code.to_owned(), Value::Object(l.table.clone())))
        .collect();
    format!(
        "if (location.protocol === 'tauri:' || location.hostname === 'tauri.localhost') {{\n\
         self.ILLUSTASH_LANGUAGE = {};\nself.ILLUSTASH_LANGUAGES = {};\nself.ILLUSTASH_TABLES = {};\n}}\n",
        Value::from(language),
        Value::Object(names),
        Value::Object(tables)
    )
}

/// The system's preferred languages, most preferred first, as language tags (`zh-Hans-CN`, `en-US`).
#[cfg(target_os = "macos")]
fn system_languages() -> Vec<String> {
    use std::ffi::{c_char, c_void, CStr};
    type Ref = *const c_void;
    const UTF8: u32 = 0x0800_0100;
    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFLocaleCopyPreferredLanguages() -> Ref;
        fn CFArrayGetCount(array: Ref) -> isize;
        fn CFArrayGetValueAtIndex(array: Ref, index: isize) -> Ref;
        fn CFStringGetCString(string: Ref, buffer: *mut c_char, size: isize, encoding: u32) -> u8;
        fn CFRelease(value: Ref);
    }
    // SAFETY: the copied array is owned here and released once; its strings are borrowed from it while it lives.
    unsafe {
        let array = CFLocaleCopyPreferredLanguages();
        if array.is_null() {
            return Vec::new();
        }
        let mut found = Vec::new();
        for i in 0..CFArrayGetCount(array) {
            let mut buffer = [0 as c_char; 64];
            let value = CFArrayGetValueAtIndex(array, i);
            if !value.is_null() && CFStringGetCString(value, buffer.as_mut_ptr(), buffer.len() as isize, UTF8) != 0 {
                found.push(CStr::from_ptr(buffer.as_ptr()).to_string_lossy().into_owned());
            }
        }
        CFRelease(array);
        found
    }
}

#[cfg(windows)]
fn system_languages() -> Vec<String> {
    const MUI_LANGUAGE_NAME: u32 = 0x8;
    #[link(name = "kernel32")]
    extern "system" {
        fn GetUserPreferredUILanguages(flags: u32, count: *mut u32, buffer: *mut u16, size: *mut u32) -> i32;
    }
    let (mut count, mut size) = (0u32, 0u32);
    // SAFETY: the first call only reports the size; the second fills a buffer of exactly that many UTF-16 units.
    unsafe {
        if GetUserPreferredUILanguages(MUI_LANGUAGE_NAME, &mut count, std::ptr::null_mut(), &mut size) == 0 || size == 0 {
            return Vec::new();
        }
        let mut buffer = vec![0u16; size as usize];
        if GetUserPreferredUILanguages(MUI_LANGUAGE_NAME, &mut count, buffer.as_mut_ptr(), &mut size) == 0 {
            return Vec::new();
        }
        // A list of NUL-terminated names, ended by an empty one.
        buffer.split(|&u| u == 0).filter(|s| !s.is_empty()).map(String::from_utf16_lossy).collect()
    }
}

/// Elsewhere (Linux is frozen) the POSIX locale variables: `en_US.UTF-8` → `en-US`.
#[cfg(not(any(target_os = "macos", windows)))]
fn system_languages() -> Vec<String> {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .filter_map(|key| std::env::var(key).ok().filter(|v| !v.is_empty()))
        .map(|v| v.split(['.', '@']).next().unwrap_or("").replace('_', "-"))
        .find(|v| !v.is_empty() && v != "C" && v != "POSIX")
        .into_iter()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILES: &[(&str, &str)] = &[
        ("en", r#"{"@name": "English", "设置": "Settings", "{n} 张": {"one": "{n} picture", "other": "{n} pictures"}, "空": ""}"#),
        ("fr", r#"{"@name": "Français"}"#),
        ("pt-BR", "{}"),
        ("bad", "not json"),
        ("EN", r#"{"@name": "upper case"}"#),
    ];

    #[test]
    fn tables_that_parse_with_valid_codes_are_on_offer() {
        let parsed = parse(FILES);
        let found: Vec<(&str, &str)> = parsed.iter().map(|l| (l.code, l.name.as_str())).collect();
        assert_eq!(found, [("en", "English"), ("fr", "Français"), ("pt-BR", "pt-BR")]);
        // The compiled-in tables are the page's: English at least, each with its name.
        assert!(locales().iter().any(|l| l.code == "en" && l.name == "English"));
    }

    #[test]
    fn the_setting_is_read_like_the_library_page_reads_it() {
        let codes = ["en", "fr"];
        for (text, expected) in [
            (Some(r#"{"language": "en"}"#), "en"),
            (Some(r#"{"language": "zh-CN", "other": 1}"#), "zh-CN"),
            (Some(r#"{"language": "system"}"#), "system"),
            (Some(r#"{"language": "de"}"#), "system"),
            (Some(r#"{"language": 3}"#), "system"),
            (Some("[1]"), "system"),
            (Some("{broken"), "system"),
            (None, "system"),
        ] {
            assert_eq!(preference(text, &codes), expected, "{text:?}");
        }
    }

    #[test]
    fn system_follows_the_rule_of_i18n_js() {
        let wanted = |tags: &[&str]| tags.iter().map(|t| t.to_string()).collect::<Vec<_>>();
        let codes = ["en", "fr", "pt-BR"];
        for (preference, tags, expected) in [
            ("en", &["zh-CN"][..], "en"),
            ("zh-CN", &["en-US"][..], "zh-CN"),
            ("system", &["zh-Hans-CN", "en"][..], "zh-CN"),
            ("system", &["zh-TW"][..], "zh-CN"),
            ("system", &["en-GB", "zh-CN"][..], "en"),
            ("system", &["fr-CA", "en"][..], "fr"),
            ("system", &["ja", "fr"][..], "fr"),
            ("system", &["ja", "zh-CN", "fr"][..], "zh-CN"),
            ("system", &["pt-br"][..], "pt-BR"),
            ("system", &["pt-PT"][..], "pt-BR"),
            ("system", &["ja-JP"][..], "en"),
            ("system", &[][..], "zh-CN"),
        ] {
            assert_eq!(resolve(preference, &wanted(tags), &codes), expected, "{preference} {tags:?}");
        }
        // Without English, a language nobody translated stays Chinese.
        assert_eq!(resolve("system", &wanted(&["ja"]), &["fr"]), "zh-CN");
        assert_eq!(resolve("system", &wanted(&["ja"]), &[]), "zh-CN");
    }

    #[test]
    fn text_without_a_translation_stays_chinese_and_placeholders_are_filled() {
        let locales = parse(FILES);
        let en = Some(&locales[0].table);
        assert_eq!(lookup(en, "设置"), "Settings");
        assert_eq!(lookup(en, "没有译文"), "没有译文");
        assert_eq!(lookup(en, "空"), "空");
        assert_eq!(lookup(en, "{n} 张"), "{n} pictures");
        assert_eq!(lookup(None, "设置"), "设置");
        let error = "boom";
        assert_eq!(fill("无法启动（{path}）：{error}；{error}", &[("path", &"/x"), ("error", &error)]), "无法启动（/x）：boom；boom");
        // Unit tests see the source text.
        assert_eq!(tr("刷新页面"), "刷新页面");
        assert_eq!(tr_with("无法启动本地服务（{path}）：{error}", &[("path", &"p"), ("error", &1)]), "无法启动本地服务（p）：1");
    }

    #[test]
    fn the_start_page_gets_the_language_names_and_only_its_table() {
        let locales = parse(FILES);
        let english = script("en", &locales);
        let lines: Vec<&str> = english.lines().collect();
        assert_eq!(lines[0], "if (location.protocol === 'tauri:' || location.hostname === 'tauri.localhost') {");
        assert_eq!(lines[1], "self.ILLUSTASH_LANGUAGE = \"en\";");
        assert_eq!(lines[2], r#"self.ILLUSTASH_LANGUAGES = {"en":"English","fr":"Français","pt-BR":"pt-BR"};"#);
        let tables: Value = serde_json::from_str(lines[3].strip_prefix("self.ILLUSTASH_TABLES = ").unwrap().strip_suffix(';').unwrap()).unwrap();
        assert_eq!(tables.as_object().unwrap().keys().collect::<Vec<_>>(), ["en"]);
        assert_eq!(tables["en"]["设置"], "Settings");
        assert_eq!(lines[4], "}");
        assert!(script("zh-CN", &locales).contains("self.ILLUSTASH_TABLES = {};"));
    }
}
