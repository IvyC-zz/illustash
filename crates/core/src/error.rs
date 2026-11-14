use std::fmt;

/// 命令失败的原因。核心事务中的 `Invalid`/`Conflict` 不留下部分写入；应用文件发布与注册另有提交边界。
#[derive(Debug)]
pub enum Error {
    Sql(rusqlite::Error),
    Io(std::io::Error),
    /// 带实际操作对象的文件系统操作错误，为应用 HTTP 契约保留。
    IoAt {
        source: std::io::Error,
        path: std::path::PathBuf,
        destination: Option<std::path::PathBuf>,
    },
    Invalid(String),
    /// 上游文本来源失败时的安全本地标签；从不带提供方的响应正文或凭据。
    Network(String),
    Conflict(String),
    Schema(String),
    /// 命令要调用方给一个新 id（值是它在命令里的 JSON 指针）；什么都没写，调用方补上再执行一次。
    NeedId(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Sql(e) => write!(f, "数据库错误：{e}"),
            Error::Io(e) => write!(f, "读取失败：{e}"),
            Error::IoAt { source, path, destination } => {
                write!(f, "{}", filesystem_message(source, path, destination.as_deref()))
            }
            Error::Network(m) => write!(f, "{m}"),
            Error::Invalid(m) => write!(f, "不合法：{m}"),
            Error::Conflict(m) => write!(f, "冲突：{m}"),
            Error::Schema(m) => write!(f, "数据库版本：{m}"),
            Error::NeedId(m) => write!(f, "需要新的 id：{m}"),
        }
    }
}

impl std::error::Error for Error {}

impl Error {
    /// `Error::Invalid` 的简写；返回 `Result` 的写法见 [`invalid`]。
    pub fn invalid(message: impl Into<String>) -> Self {
        Error::Invalid(message.into())
    }
    /// 出了什么错，用于给人看的任务状态：只有消息本身，不带告诉代码如何应对的种类前缀
    /// （"不合法："、"冲突："）。
    pub fn reason(&self) -> String {
        match self {
            Error::Invalid(m) | Error::Network(m) | Error::Conflict(m) => m.clone(),
            other => other.to_string(),
        }
    }
}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Error::Sql(e)
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

/// 以 `Error::Invalid` 失败，供 `return invalid("…")` 一类写法使用。
pub fn invalid<T>(message: impl Into<String>) -> Result<T> {
    Err(Error::invalid(message))
}

/// 文件操作失败的显示文字，保留操作系统原因、错误号和两个操作路径。
pub fn filesystem_message(
    error: &std::io::Error,
    path: &std::path::Path,
    destination: Option<&std::path::Path>,
) -> String {
    let source = error.to_string();
    let path = crate::compat_text::python_repr(&path.to_string_lossy());
    let destination = destination.map(|value| crate::compat_text::python_repr(&value.to_string_lossy()));
    match (error.raw_os_error(), destination) {
        (Some(code), destination) => {
            let suffix = format!(" (os error {code})");
            let source = source.strip_suffix(&suffix).unwrap_or(&source);
            match destination {
                Some(other) => format!("文件操作失败（错误号 {code}）：{source}（路径：{path} → {other}）"),
                None => format!("文件操作失败（错误号 {code}）：{source}（路径：{path}）"),
            }
        }
        (None, Some(other)) => format!("文件操作失败：{source}（路径：{path} → {other}）"),
        (None, None) => format!("文件操作失败：{source}（路径：{path}）"),
    }
}
pub fn io_at(error: std::io::Error, path: &std::path::Path, destination: Option<&std::path::Path>) -> Error {
    Error::IoAt { source: error, path: path.to_owned(), destination: destination.map(ToOwned::to_owned) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn filesystem_messages_keep_os_details_and_both_quoted_paths() {
        let path = Path::new("合成/it's\n雪");
        let destination = Path::new("合成/目标");
        let quoted = crate::compat_text::python_repr(&path.to_string_lossy());
        let other = crate::compat_text::python_repr(&destination.to_string_lossy());
        for error in [std::io::Error::from_raw_os_error(13), std::io::Error::other("合成原因")] {
            let original = error.to_string();
            let (prefix, source) = if let Some(code) = error.raw_os_error() {
                let suffix = format!(" (os error {code})");
                (format!("文件操作失败（错误号 {code}）："), original.strip_suffix(&suffix).unwrap_or(&original))
            } else {
                ("文件操作失败：".to_owned(), original.as_str())
            };
            assert_eq!(filesystem_message(&error, path, None), format!("{prefix}{source}（路径：{quoted}）"));
            assert_eq!(
                filesystem_message(&error, path, Some(destination)),
                format!("{prefix}{source}（路径：{quoted} → {other}）")
            );
            let error = io_at(error, path, Some(destination));
            assert!(matches!(error, Error::IoAt { .. }));
            assert_eq!(error.reason(), error.to_string());
        }
    }
}
