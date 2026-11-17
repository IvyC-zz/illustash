//! 平台服务：桌面原生对话框，macOS 上可回退到命令行。
//! 会话归属仍在 app_http；本模块既不打开也不写入图库。
use crate::{http_json::Json, Error, Result};
use std::time::Duration;

/// `translate` 按页面语言给出回退选择器的提示语（app_preferences::tr）。
pub fn choose(
    kind: &str,
    multiple: bool,
    stopping: &dyn Fn() -> bool,
    translate: &dyn Fn(&str) -> String,
) -> Result<Json> {
    let choice_kind = match kind {
        "folder" => illustash_platform::picker::Kind::Folder,
        "library" => illustash_platform::picker::Kind::Library,
        "zip" => illustash_platform::picker::Kind::Zip,
        "archive" => illustash_platform::picker::Kind::Archive,
        "models" => illustash_platform::picker::Kind::Models,
        "human-conclusions" => illustash_platform::picker::Kind::HumanConclusions,
        "export" => illustash_platform::picker::Kind::Export,
        _ => return Err(Error::Invalid("未知的选择类型".into())),
    };
    if multiple && !matches!(kind, "folder" | "models") {
        return Err(Error::Invalid("此类型不支持多选".into()));
    }
    if crate::app_desktop::available() {
        let reply = crate::app_desktop::request(
            "POST",
            &format!("/platform/choose?kind={kind}&multiple={}", u8::from(multiple)),
            600,
        )?;
        let Some(Json::Array(paths)) = reply.get("paths") else {
            return Err(Error::Invalid("选择窗口响应不正确".into()));
        };
        let paths = paths
            .iter()
            .map(|p| match p {
                Json::Scalar(serde_json::Value::String(p)) if !p.is_empty() => Ok(p.clone()),
                _ => Err(Error::Invalid("选择窗口路径不正确".into())),
            })
            .collect::<Result<Vec<_>>>()?;
        if (!multiple && paths.len() > 1) || paths.len() > 4096 {
            return Err(Error::Invalid("选择窗口返回过多路径".into()));
        }
        return Ok(selection(kind, paths, multiple));
    }
    let title = translate(choice_kind.title());
    let choice = illustash_platform::picker::choose(choice_kind, &title, multiple, stopping, Duration::from_secs(600))?;
    let paths = match choice {
        illustash_platform::picker::Choice::Selected(paths) => paths,
        illustash_platform::picker::Choice::Cancelled => vec![],
        illustash_platform::picker::Choice::Unsupported => {
            return Err(Error::Invalid("Unsupported：这台电脑上请直接填写路径".into()))
        }
    };
    Ok(selection(kind, paths, multiple))
}

fn selection(kind: &str, paths: Vec<String>, multiple: bool) -> Json {
    let path = paths.first().map(|s| s.as_str().into()).unwrap_or_else(|| serde_json::Value::Null.into());
    let mut result = Json::object([("path", path)]);
    if kind == "folder" {
        // 旧的单来源表单要这个字段；这是中性的默认值，从不是猜测。
        if !multiple && !paths.is_empty() {
            result.set("preset", "other".into());
        }
        if let Some(path) = paths.first() {
            result.set(
                "name",
                std::path::Path::new(path).file_name().unwrap_or_default().to_string_lossy().as_ref().into(),
            );
        }
    }
    if multiple {
        result.set("paths", Json::Array(paths.into_iter().map(Into::into).collect()));
    }
    result
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selected_paths_never_guess_provenance() {
        let result = selection("folder", vec!["/synthetic/Pixiv".into(), "/synthetic/X".into()], true);
        assert!(result.get("preset").is_none());
        assert_eq!(result.get("path").unwrap().text(), "/synthetic/Pixiv");
        assert!(matches!(result.get("paths"), Some(Json::Array(paths)) if paths.len() == 2));
        assert_eq!(selection("folder", vec![], true).dump(), r#"{"path": null, "paths": []}"#);
    }
}
