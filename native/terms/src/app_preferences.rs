//! App-wide preferences (#173): one preferences.json beside libraries.json, shared by every library and browser
//! (the desktop shell reads it in a later stage). Only the interface language lives here; unknown keys are kept.
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

/// `system` follows the browser's languages (zh* shows Chinese, anything else English).
pub(crate) const LANGUAGES: &[&str] = &["system", "zh-CN", "en"];

fn file() -> Result<PathBuf> {
    Ok(app_support(&user_home()?).join("preferences.json"))
}

/// The configured language. A missing, unreadable or hand-broken file means the default, never an error:
/// the page must still load.
pub(crate) fn language() -> String {
    file().map(|file| language_at(&file)).unwrap_or_else(|_| LANGUAGES[0].into())
}
fn language_at(file: &Path) -> String {
    match read(file).get("language").and_then(Value::as_str) {
        Some(language) if LANGUAGES.contains(&language) => language.into(),
        _ => LANGUAGES[0].into(),
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

/// `preferences` in GET /api/settings.
pub(crate) fn info() -> Json {
    Json::object([("language", language().into())])
}

/// POST /api/preferences: `{"language": "en"}`. Preferences never grant network or start tasks.
pub(crate) fn set_json(body: &str) -> Result<()> {
    let Json::Object(changes) = crate::http_json::parse_request(body)? else {
        return Err(Error::Invalid("未知设置".into()));
    };
    if changes.is_empty() || changes.iter().any(|(key, _)| key != "language") {
        return Err(Error::Invalid("未知设置".into()));
    }
    let language = match changes.iter().find(|(key, _)| key == "language").map(|(_, value)| value) {
        Some(Json::Scalar(Value::String(language))) if LANGUAGES.contains(&language.as_str()) => language.clone(),
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
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn missing_or_broken_file_means_follow_system() {
        let f = Fixture::new();
        let file = f.file();
        assert_eq!(language_at(&file), "system");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        for text in ["", "not json", "[]", r#"{"language": 3}"#, r#"{"language": "fr"}"#, r#"{"language": "EN"}"#] {
            fs::write(&file, text).unwrap();
            assert_eq!(language_at(&file), "system", "{text}");
        }
        fs::write(&file, r#"{"language": "zh-CN"}"#).unwrap();
        assert_eq!(language_at(&file), "zh-CN");
    }

    #[test]
    fn writes_create_the_folder_keep_other_keys_and_leave_no_temporary_file() {
        let f = Fixture::new();
        let file = f.file();
        set_language_at(&file, "en").unwrap();
        assert_eq!(language_at(&file), "en");
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
        assert!(LANGUAGES.contains(&written["language"].as_str().unwrap()));
        assert!(!file.with_extension("tmp").exists());
    }

    #[test]
    fn request_validation_rejects_without_writing() {
        // Each body fails before the file path is resolved, so the user's real preferences are never touched.
        for body in
            ["[]", "{}", r#"{"language": "fr"}"#, r#"{"language": 1}"#, r#"{"language": "en", "theme": "dark"}"#]
        {
            assert!(matches!(set_json(body), Err(Error::Invalid(_))), "{body}");
        }
        let _ = language as fn() -> String;
        let _ = info as fn() -> Json;
    }
}
