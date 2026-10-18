//! App-wide preferences (#173): one preferences.json beside libraries.json, shared by every library and browser
//! (the desktop shell reads it in a later stage). Only the interface language lives here; unknown keys are kept.
//! The languages on offer are the tables in web/static/locales (#182): adding one needs no code.
use crate::{
    app_paths::{app_support, user_home},
    http_json::Json,
    Error, Result,
};
use serde_json::{Map, Value};
use std::{
    fs,
    path::{Path, PathBuf},
};

/// `system` follows the browser's languages (web/static/i18n.js picks among the tables); the default.
const SYSTEM: &str = "system";
/// Chinese is the source text: it has no table and is always on offer.
const SOURCE: &str = "zh-CN";

/// One web/static/locales/<code>.json: keyed by the Chinese text, plus `"@name"`, the language named in itself.
pub(crate) struct Locale {
    pub(crate) code: String,
    pub(crate) name: String,
    pub(crate) table: Map<String, Value>,
}

pub(crate) fn locales_folder(root: &Path) -> PathBuf {
    root.join("web/static/locales")
}

/// A code names its table: `en`, `ja`, `pt-BR`. Letters, digits and hyphens only, so a code taken from a request
/// never leaves the folder; the case must match the file name exactly.
fn valid_code(code: &str) -> bool {
    let mut parts = code.split('-');
    let primary = parts.next().unwrap_or("");
    code != SOURCE
        && (2..=3).contains(&primary.len())
        && primary.bytes().all(|b| b.is_ascii_lowercase())
        && parts.all(|part| (1..=8).contains(&part.len()) && part.bytes().all(|b| b.is_ascii_alphanumeric()))
}

/// Every table in `folder` that parses as a JSON object, by code. A broken one is left out (and logged), so the
/// settings never offer a language the page cannot show; a missing folder leaves only Chinese.
pub(crate) fn locales(folder: &Path) -> Vec<Locale> {
    let Ok(entries) = fs::read_dir(folder) else { return Vec::new() };
    let mut found: Vec<Locale> = entries
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            let code = path.file_name()?.to_str()?.strip_suffix(".json")?.to_string();
            if !valid_code(&code) {
                return None;
            }
            let table = fs::read(&path)
                .map_err(|e| e.to_string())
                .and_then(|bytes| serde_json::from_slice::<Map<String, Value>>(&bytes).map_err(|e| e.to_string()));
            let table = table.inspect_err(|e| eprintln!("HTTP i18n: {}: {e}", path.display())).ok()?;
            let name =
                table.get("@name").and_then(Value::as_str).filter(|name| !name.is_empty()).unwrap_or(&code).into();
            Some(Locale { code, name, table })
        })
        .collect();
    found.sort_by(|a, b| a.code.cmp(&b.code));
    found
}

/// Text the backend itself shows outside the page (the macOS file chooser's prompt), in the language the page named
/// (`X-Illustash-Language`): that table's entry keyed by the Chinese, or the Chinese. What the page shows it translates
/// itself (web/static/i18n.js tb()), so the backend's replies and records stay Chinese.
pub(crate) fn tr(root: &Path, language: Option<&str>, text: &str) -> String {
    let Some(code) = language.filter(|code| valid_code(code)) else { return text.into() };
    let table = fs::read(locales_folder(root).join(format!("{code}.json")))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Map<String, Value>>(&bytes).ok());
    let entry = table.as_ref().and_then(|table| table.get(text));
    match entry {
        Some(Value::String(out)) if !out.is_empty() => out.clone(),
        Some(Value::Object(forms)) => {
            forms.get("other").and_then(Value::as_str).filter(|o| !o.is_empty()).unwrap_or(text).into()
        }
        _ => text.into(),
    }
}

fn offered(language: &str, locales: &[Locale]) -> bool {
    language == SYSTEM || language == SOURCE || locales.iter().any(|locale| locale.code == language)
}

fn file() -> Result<PathBuf> {
    Ok(app_support(&user_home()?).join("preferences.json"))
}

/// The configured language. A missing, unreadable or hand-broken file means the default, never an error:
/// the page must still load. So does a language whose table has since gone.
pub(crate) fn language(locales: &[Locale]) -> String {
    file().map(|file| language_at(&file, locales)).unwrap_or_else(|_| SYSTEM.into())
}
fn language_at(file: &Path, locales: &[Locale]) -> String {
    match read(file).get("language").and_then(Value::as_str) {
        Some(language) if offered(language, locales) => language.into(),
        _ => SYSTEM.into(),
    }
}
fn read(file: &Path) -> Map<String, Value> {
    fs::read_to_string(file)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|value| match value {
            Value::Object(fields) => Some(fields),
            _ => None,
        })
        .unwrap_or_default()
}

/// The reply of POST /api/preferences: `{"language": …}`.
pub(crate) fn info(locales: &[Locale]) -> Json {
    Json::object([("language", language(locales).into())])
}

/// POST /api/preferences: `{"language": "en"}`. Preferences never grant network or start tasks.
pub(crate) fn set_json(body: &str, locales: &[Locale]) -> Result<()> {
    let Json::Object(changes) = crate::http_json::parse_request(body)? else {
        return Err(Error::Invalid("未知设置".into()));
    };
    if changes.is_empty() || changes.iter().any(|(key, _)| key != "language") {
        return Err(Error::Invalid("未知设置".into()));
    }
    let language = match changes.iter().find(|(key, _)| key == "language").map(|(_, value)| value) {
        Some(Json::Scalar(Value::String(language))) if offered(language, locales) => language.clone(),
        _ => return Err(Error::Invalid("语言取值不正确".into())),
    };
    set_language_at(&file()?, &language)
}

fn set_language_at(file: &Path, language: &str) -> Result<()> {
    // Hosts of different libraries share this file: same lock and rename as libraries.json.
    crate::app_libraries::registry_locked(file, || {
        let mut fields = read(file);
        fields.insert("language".into(), language.into());
        let text =
            serde_json::to_string_pretty(&Value::Object(fields)).map_err(|e| Error::Invalid(e.to_string()))? + "\n";
        fs::create_dir_all(file.parent().ok_or_else(|| Error::Invalid("无效的偏好设置路径".into()))?)?;
        let tmp = file.with_extension("tmp");
        fs::write(&tmp, text)?;
        fs::rename(&tmp, file).inspect_err(|_| {
            let _ = fs::remove_file(&tmp);
        })?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "illustash-preferences-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn file(&self) -> PathBuf {
            self.0.join("support").join("preferences.json")
        }
        /// A locales folder holding these files.
        fn locales(&self, files: &[(&str, &str)]) -> Vec<Locale> {
            let folder = self.0.join("locales");
            fs::create_dir_all(&folder).unwrap();
            for (name, text) in files {
                fs::write(folder.join(name), text).unwrap();
            }
            locales(&folder)
        }
        fn english(&self) -> Vec<Locale> {
            self.locales(&[("en.json", r#"{"@name": "English", "设置": "Settings"}"#)])
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn backend_text_takes_the_named_table_or_stays_chinese() {
        let f = Fixture::new();
        let root = f.0.join("root");
        let folder = locales_folder(&root);
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join("en.json"), r#"{"@name": "English", "选择图片文件夹": "Choose a picture folder", "{n} 张": {"one": "{n} picture", "other": "{n} pictures"}}"#).unwrap();
        assert_eq!(tr(&root, Some("en"), "选择图片文件夹"), "Choose a picture folder");
        assert_eq!(tr(&root, Some("en"), "{n} 张"), "{n} pictures");
        assert_eq!(tr(&root, Some("en"), "没有译文"), "没有译文");
        for language in [None, Some("zh-CN"), Some("fr"), Some("../en"), Some("EN")] {
            assert_eq!(tr(&root, language, "选择图片文件夹"), "选择图片文件夹", "{language:?}");
        }
    }

    #[test]
    fn languages_on_offer_are_the_tables_that_parse() {
        let f = Fixture::new();
        assert!(locales(&f.0.join("missing")).is_empty());
        let found = f.locales(&[
            ("en.json", r#"{"@name": "English", "设置": "Settings", "{n} 张": {"one": "{n} picture", "other": "{n} pictures"}}"#),
            ("pt-BR.json", r#"{"@name": "Português (Brasil)"}"#),
            ("ja.json", "{}"),
            ("fr.json", "{broken"),
            ("de.json", "[]"),
            // Chinese is the source; the rest are not codes. EN.json is left to valid_code below: on a
            // case-insensitive disk it would overwrite en.json.
            ("zh-CN.json", "{}"),
            ("e.json", "{}"),
            ("en_US.json", "{}"),
            ("en..json", "{}"),
            ("readme.txt", "{}"),
        ]);
        let listed: Vec<_> = found.iter().map(|l| (l.code.as_str(), l.name.as_str(), l.table.len())).collect();
        // Without "@name" the code stands in for the name.
        assert_eq!(listed, [("en", "English", 3), ("ja", "ja", 0), ("pt-BR", "Português (Brasil)", 1)]);
        for code in ["en", "ja", "pt-BR", "zh-Hant", "yue-HK", "es-419"] {
            assert!(valid_code(code), "{code}");
        }
        for code in [
            "zh-CN",
            "EN",
            "En",
            "e",
            "engl",
            "en-",
            "en--GB",
            "-en",
            "en_GB",
            "../en",
            "en/../en",
            "en.json",
            "",
            "en-abcdefghi",
        ] {
            assert!(!valid_code(code), "{code}");
        }
    }

    #[test]
    fn missing_or_broken_file_means_follow_system() {
        let f = Fixture::new();
        let english = f.english();
        let file = f.file();
        assert_eq!(language_at(&file, &english), "system");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        for text in ["", "not json", "[]", r#"{"language": 3}"#, r#"{"language": "fr"}"#, r#"{"language": "EN"}"#] {
            fs::write(&file, text).unwrap();
            assert_eq!(language_at(&file, &english), "system", "{text}");
        }
        fs::write(&file, r#"{"language": "zh-CN"}"#).unwrap();
        assert_eq!(language_at(&file, &english), "zh-CN");
        // A language whose table has gone follows the system again; Chinese needs no table.
        fs::write(&file, r#"{"language": "en"}"#).unwrap();
        assert_eq!(language_at(&file, &english), "en");
        assert_eq!(language_at(&file, &[]), "system");
        fs::write(&file, r#"{"language": "zh-CN"}"#).unwrap();
        assert_eq!(language_at(&file, &[]), "zh-CN");
    }

    #[test]
    fn writes_create_the_folder_keep_other_keys_and_leave_no_temporary_file() {
        let f = Fixture::new();
        let file = f.file();
        set_language_at(&file, "en").unwrap();
        assert_eq!(language_at(&file, &f.english()), "en");
        assert_eq!(fs::read_to_string(&file).unwrap(), "{\n  \"language\": \"en\"\n}\n");
        fs::write(&file, r#"{"later": {"kept": true}, "language": "en"}"#).unwrap();
        set_language_at(&file, "zh-CN").unwrap();
        let written: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
        assert_eq!(written, serde_json::json!({"later": {"kept": true}, "language": "zh-CN"}));
        assert!(!file.with_extension("tmp").exists());
        // A broken file is replaced rather than blocking the change.
        fs::write(&file, "{broken").unwrap();
        set_language_at(&file, "system").unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "{\n  \"language\": \"system\"\n}\n");
    }

    #[test]
    fn failed_write_keeps_the_previous_file() {
        let f = Fixture::new();
        let file = f.file();
        set_language_at(&file, "en").unwrap();
        let before = fs::read(&file).unwrap();
        fs::create_dir(file.with_extension("tmp")).unwrap();
        assert!(set_language_at(&file, "zh-CN").is_err());
        assert_eq!(fs::read(&file).unwrap(), before);
    }

    #[test]
    fn concurrent_writers_never_corrupt_the_file() {
        let f = Fixture::new();
        let file = f.file();
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, r#"{"kept": 1}"#).unwrap();
        std::thread::scope(|scope| {
            for language in ["en", "zh-CN", "system", "en"] {
                let file = &file;
                scope.spawn(move || {
                    for _ in 0..20 {
                        set_language_at(file, language).unwrap();
                    }
                });
            }
        });
        let written: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
        assert_eq!(written["kept"], 1);
        assert!(offered(written["language"].as_str().unwrap(), &f.english()));
        assert!(!file.with_extension("tmp").exists());
    }

    #[test]
    fn request_validation_rejects_without_writing() {
        // Each body fails before the file path is resolved, so the user's real preferences are never touched.
        let f = Fixture::new();
        let offer = f.locales(&[("en.json", "{}"), ("fr.json", "{broken"), ("ja.json", r#"{"@name": "日本語"}"#)]);
        for body in [
            "[]",
            "{}",
            r#"{"language": "fr"}"#,
            r#"{"language": "de"}"#,
            r#"{"language": "EN"}"#,
            r#"{"language": "ja-JP"}"#,
            r#"{"language": "../en"}"#,
            r#"{"language": "en.json"}"#,
            r#"{"language": ""}"#,
            r#"{"language": 1}"#,
            r#"{"language": "en", "theme": "dark"}"#,
        ] {
            assert!(matches!(set_json(body, &offer), Err(Error::Invalid(_))), "{body}");
        }
        // Accepted values are exactly system, zh-CN and the tables found.
        for language in ["system", "zh-CN", "en", "ja"] {
            assert!(offered(language, &offer), "{language}");
        }
        assert!(!offered("en", &[]));
        let _ = language as fn(&[Locale]) -> String;
        let _ = info as fn(&[Locale]) -> Json;
    }
}
