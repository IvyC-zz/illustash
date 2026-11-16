//! 全应用偏好设置（#173）：libraries.json 旁边的一份 preferences.json，所有图库和浏览器共用（桌面外壳在后续阶段读取）。
//! 界面语言和「使用 GPU 加速识别」（#270/#271，由识别和索引 worker 读取，见 recognition_compute/backend.rs）放在这里；
//! 不认识的键保留。可选的语言就是 web/static/locales 里的各张表（#182）：新增语言不用改代码。
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

/// `system` 跟随浏览器的语言（由 web/static/i18n.js 在各张表中挑选）；这是默认值。
const SYSTEM: &str = "system";
/// 中文是原文：没有表，始终可选。
const SOURCE: &str = "zh-CN";

/// 一份 web/static/locales/<code>.json：以中文原文为键，另有 `"@name"`，即用该语言自己写的语言名。
pub(crate) struct Locale {
    pub(crate) code: String,
    pub(crate) name: String,
    pub(crate) table: Map<String, Value>,
}

pub(crate) fn locales_folder(root: &Path) -> PathBuf {
    root.join("web/static/locales")
}

/// 代码对应它的表：`en`、`ja`、`pt-BR`。只允许字母、数字和连字符，所以取自请求的代码不会跑出这个文件夹；
/// 大小写必须与文件名完全一致。
fn valid_code(code: &str) -> bool {
    let mut parts = code.split('-');
    let primary = parts.next().unwrap_or("");
    code != SOURCE
        && (2..=3).contains(&primary.len())
        && primary.bytes().all(|b| b.is_ascii_lowercase())
        && parts.all(|part| (1..=8).contains(&part.len()) && part.bytes().all(|b| b.is_ascii_alphanumeric()))
}

/// `folder` 里能解析成 JSON 对象的每张表，按代码排列。坏掉的表不列入（并记日志），这样设置里不会出现页面显示不了的
/// 语言；文件夹不存在时只剩中文。
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

/// 后端自己在页面之外显示的文字（macOS 文件选择器的提示语），用页面指定的语言（`X-Illustash-Language`）：
/// 取该表里以这段中文为键的条目，没有就用中文。页面显示的内容由页面自己翻译（web/static/i18n.js tb()），
/// 所以后端的回复和记录仍是中文。
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

/// 已设置的语言。文件不存在、读不出或被手工改坏都按默认值处理，绝不报错：页面必须仍能加载。
/// 语言的表后来没了也一样。
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

/// 识别能否使用 GPU 或神经网络引擎：除非关掉，否则开启。和语言一样，文件不存在或坏掉都按默认值处理。
/// worker 在启动时读取，所以改动从下一次同步起生效。
pub(crate) fn acceleration() -> bool {
    file().map(|file| acceleration_at(&file)).unwrap_or(true)
}
fn acceleration_at(file: &Path) -> bool {
    read(file).get("acceleration").and_then(Value::as_bool).unwrap_or(true)
}

/// 桌面 App 里关窗转到后台以后，任务做完时退出（`quit`，默认）还是留在后台（`stay`）。只有外壳读它
/// （desktop/src/background.rs，#451）；值不认识也按 `quit`。
pub(crate) const AFTER_TASKS: &str = "after_tasks";
const AFTER_TASKS_VALUES: [&str; 2] = ["quit", "stay"];
pub(crate) fn after_tasks() -> &'static str {
    file().map(|file| after_tasks_at(&file)).unwrap_or(AFTER_TASKS_VALUES[0])
}
fn after_tasks_at(file: &Path) -> &'static str {
    match read(file).get(AFTER_TASKS).and_then(Value::as_str) {
        Some("stay") => AFTER_TASKS_VALUES[1],
        _ => AFTER_TASKS_VALUES[0],
    }
}

/// POST /api/preferences 的回复：`{"language": …, "acceleration": …}`。`after_tasks` 随 GET /api/settings 的
/// `desktop` 给页面，只在桌面 App 里有。
pub(crate) fn info(locales: &[Locale]) -> Json {
    Json::object([("language", language(locales).into()), ("acceleration", acceleration().into())])
}

/// POST /api/preferences：`{"language": "en"}`、`{"acceleration": false}` 或 `{"after_tasks": "stay"}`，可以一起给。
/// 偏好设置从不授予联网，
/// 也不启动任务。
pub(crate) fn set_json(body: &str, locales: &[Locale]) -> Result<()> {
    let Json::Object(changes) = crate::http_json::parse_request(body)? else {
        return Err(Error::Invalid("未知设置".into()));
    };
    if changes.is_empty() || changes.iter().any(|(key, _)| !["language", "acceleration", AFTER_TASKS].contains(&key.as_str())) {
        return Err(Error::Invalid("未知设置".into()));
    }
    let mut validated = Vec::new();
    for (key, value) in changes {
        let value = match (key.as_str(), value) {
            ("language", Json::Scalar(Value::String(language))) if offered(&language, locales) => language.into(),
            ("language", _) => return Err(Error::Invalid("语言取值不正确".into())),
            ("acceleration", Json::Scalar(value @ Value::Bool(_))) => value,
            (AFTER_TASKS, Json::Scalar(Value::String(v))) if AFTER_TASKS_VALUES.contains(&v.as_str()) => v.into(),
            _ => return Err(Error::Invalid("开关取值不正确".into())),
        };
        validated.push((key, value));
    }
    set_at(&file()?, validated)
}

#[cfg(test)]
fn set_language_at(file: &Path, language: &str) -> Result<()> {
    set_at(file, vec![("language".into(), language.into())])
}

fn set_at(file: &Path, changes: Vec<(String, Value)>) -> Result<()> {
    // 不同图库的宿主共用这个文件：与 libraries.json 用同样的锁和改名方式。
    crate::app_libraries::registry_locked(file, || {
        let mut fields = read(file);
        fields.extend(changes);
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
        /// 放着这些文件的 locales 文件夹。
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
            // 中文是原文；其余几个都不是代码。EN.json 留给下面的 valid_code：在不区分大小写的磁盘上它会覆盖 en.json。
            ("zh-CN.json", "{}"),
            ("e.json", "{}"),
            ("en_US.json", "{}"),
            ("en..json", "{}"),
            ("readme.txt", "{}"),
        ]);
        let listed: Vec<_> = found.iter().map(|l| (l.code.as_str(), l.name.as_str(), l.table.len())).collect();
        // 没有 "@name" 时用代码代替名称。
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
        // 语言的表没了就重新跟随系统；中文不需要表。
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
        // 坏掉的文件直接替换，不挡住这次修改。
        fs::write(&file, "{broken").unwrap();
        set_language_at(&file, "system").unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "{\n  \"language\": \"system\"\n}\n");
    }

    #[test]
    fn after_tasks_defaults_to_quit_and_only_takes_known_values() {
        let f = Fixture::new();
        let file = f.file();
        assert_eq!(after_tasks_at(&file), "quit", "no file");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        for text in ["", "not json", "[]", r#"{"after_tasks": true}"#, r#"{"after_tasks": "later"}"#] {
            fs::write(&file, text).unwrap();
            assert_eq!(after_tasks_at(&file), "quit", "{text}");
        }
        set_at(&file, vec![(AFTER_TASKS.into(), "stay".into())]).unwrap();
        assert_eq!(after_tasks_at(&file), "stay");
    }

    #[test]
    fn acceleration_is_on_unless_switched_off() {
        let f = Fixture::new();
        let file = f.file();
        assert!(acceleration_at(&file), "no file: on");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        for text in
            ["", "not json", "[]", r#"{"acceleration": "no"}"#, r#"{"acceleration": 0}"#, r#"{"acceleration": true}"#]
        {
            fs::write(&file, text).unwrap();
            assert!(acceleration_at(&file), "{text}");
        }
        set_language_at(&file, "en").unwrap();
        set_at(&file, vec![("acceleration".into(), false.into())]).unwrap();
        assert!(!acceleration_at(&file));
        assert_eq!(language_at(&file, &f.english()), "en", "the language is kept");
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
        // 每个请求体都在解析文件路径之前失败，所以绝不会碰到用户真实的偏好设置。
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
            r#"{"acceleration": 1}"#,
            r#"{"acceleration": "off"}"#,
            r#"{"acceleration": null}"#,
            r#"{"acceleration": false, "language": "fr"}"#,
        ] {
            assert!(matches!(set_json(body, &offer), Err(Error::Invalid(_))), "{body}");
        }
        // 接受的值恰好是 system、zh-CN 和找到的各张表。
        for language in ["system", "zh-CN", "en", "ja"] {
            assert!(offered(language, &offer), "{language}");
        }
        assert!(!offered("en", &[]));
        let _ = language as fn(&[Locale]) -> String;
        let _ = info as fn(&[Locale]) -> Json;
    }
}
