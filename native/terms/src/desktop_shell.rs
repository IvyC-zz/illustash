//! What the desktop shell's welcome page needs before any library is open (#107): the recent libraries,
//! whether a picked folder is a library, and creating one. The same registry, factory and reference rule as
//! the settings page; nothing here opens, upgrades or writes an existing library.
use crate::app_create::NewSource;
use crate::{Error, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// Recent libraries that can still be opened, most recent first: `[{path, name}]`.
pub fn recent() -> Vec<Value> {
    crate::app_libraries::recent_entries()
}

/// The library opened at startup without `--library`: the most recent one that can still be opened.
pub fn startup_library() -> Option<PathBuf> {
    crate::app_libraries::startup().ok()
}

/// A folder the user picked is a library package only if it carries library.json.
pub fn is_library(path: &Path) -> bool {
    path.join("library.json").is_file()
}

/// Shown on the welcome page: where new libraries go, the origins a folder can have, and whether the
/// Photos library (macOS only) can be a source.
pub fn describe() -> Value {
    json!({
        "home": crate::app_libraries::home().ok().and_then(|p| p.to_str().map(str::to_owned)),
        "origins": crate::origin::descriptions(),
        "photos": illustash_platform::capabilities::photos(),
    })
}

/// One source picked on the welcome page.
pub enum Pick {
    Folder { path: PathBuf, preset: String },
    Photos,
}

/// Create a library under the libraries home (a numbered name when taken), list it among the recent ones and
/// return its path. `root` is the resource root, for the reference library.
pub fn create(root: &Path, name: &str, picks: Vec<Pick>) -> Result<PathBuf> {
    let name = crate::compat_text::trim(name);
    let name = if name.is_empty() { "新图库" } else { name };
    let mut sources = Vec::new();
    for pick in picks {
        sources.push(match pick {
            Pick::Folder { path, preset } => NewSource::Folder { path, preset },
            Pick::Photos => {
                if !illustash_platform::capabilities::photos() {
                    return Err(Error::Invalid("Unsupported：照片图库只在 macOS 上可用".into()));
                }
                NewSource::Photos { path: crate::app_libraries::photos_library()? }
            }
        });
    }
    let dest = crate::app_libraries::unique_path(name)?;
    crate::app_service::create(root, &dest, name, sources)
}
