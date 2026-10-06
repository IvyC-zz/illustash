use std::fmt;

/// 命令失败的原因。核心事务中的 `Invalid`/`Conflict` 不留下部分写入；应用文件发布与注册另有提交边界。
#[derive(Debug)]
pub enum Error {
    Sql(rusqlite::Error),
    Io(std::io::Error),
    /// Filesystem operation with its actual operand(s), retained for the application HTTP contract.
    IoAt {
        source: std::io::Error,
        path: std::path::PathBuf,
        destination: Option<std::path::PathBuf>,
    },
    Invalid(String),
    /// Safe, local label for an upstream text-source failure; never provider bodies or credentials.
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

pub(crate) fn invalid<T>(message: impl Into<String>) -> Result<T> {
    Err(Error::Invalid(message.into()))
}

/// Preserve legacy OSError text at the two explicit creation/registry boundaries.
pub fn filesystem_message(
    error: &std::io::Error,
    path: &std::path::Path,
    destination: Option<&std::path::Path>,
) -> String {
    let text = error.to_string();
    let message = if let Some(code) = error.raw_os_error() {
        let suffix = format!(" (os error {code})");
        format!("[Errno {code}] {}", text.strip_suffix(&suffix).unwrap_or(&text))
    } else {
        text
    };
    let quoted = crate::compat_text::python_repr(&path.to_string_lossy());
    if let Some(destination) = destination {
        format!("{message}: {quoted} -> {}", crate::compat_text::python_repr(&destination.to_string_lossy()))
    } else {
        format!("{message}: {quoted}")
    }
}
pub fn io_at(error: std::io::Error, path: &std::path::Path, destination: Option<&std::path::Path>) -> Error {
    Error::IoAt { source: error, path: path.to_owned(), destination: destination.map(ToOwned::to_owned) }
}
