//! 用图动作（#450）：打开原文件、在访达或资源管理器中显示、拖出。导出到文件夹在 `app_asset_export`。
//!
//! 页面只按资产 ID 指一张图；路径由这里从当前图库查出来，页面给的路径一律不收。打开、显示、拖出都不写原文件。
//! 有桌面外壳时经本机桥（desktop/src/bridge.rs）在外壳进程里做：Windows 上从拥有前台窗口的进程打开资源管理器才不会
//! 被压在后面，拖出也只能在外壳里起。浏览器模式（没有外壳）直接在宿主里打开和显示，不能拖出。
//! 照片来源的文件在「照片」的图库包里：不显示位置，打开和拖出用临时副本。安全边界见
//! docs/ARCHITECTURE_DIRECTION.md「用图动作」。
use crate::{app_library::Library, Error, Result};
use rusqlite::{Connection, OptionalExtension};
use std::{
    fs, io,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

/// 一次最多拖出几张。
pub(crate) const DRAG_LIMIT: usize = 100;
/// 临时副本留多久：下次做副本时清掉更早的。
const COPY_KEEP: Duration = Duration::from_secs(24 * 3600);

/// 一张图在磁盘上的位置。
pub(crate) struct Located {
    pub uuid: String,
    /// `assets.src_path`：文件夹来源是原图，照片来源可能是派生图。
    pub path: PathBuf,
    /// 用来起名的文件名（照片来源先用「照片」里的原文件名）。
    pub name: String,
    /// 照片来源的图库包路径（配置里写的原样），文件夹来源为 None。
    pub photos: Option<String>,
}
impl Located {
    /// 导出和副本用的源文件：照片来源本机有原图就用原图，否则用派生图；第二项表示用的是派生图。
    pub(crate) fn original(&self) -> (PathBuf, String, bool) {
        let Some(root) = &self.photos else { return (self.path.clone(), self.name.clone(), false) };
        let (original, name) = crate::platform_photos::original(root, &self.uuid).unwrap_or((None, String::new()));
        let name = if name.is_empty() { self.name.clone() } else { name };
        match original {
            Some(original) => {
                let name = with_extension(&name, &original);
                (original, name, false)
            }
            None => {
                let name = with_extension(&name, &self.path);
                (self.path.clone(), name, true)
            }
        }
    }
}
/// `name` 换成 `file` 的扩展名：派生图是 JPEG，原文件名可能是 .HEIC。
fn with_extension(name: &str, file: &Path) -> String {
    let stem = Path::new(name).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    match file.extension() {
        Some(ext) => format!("{stem}.{}", ext.to_string_lossy()),
        None => stem,
    }
}

/// 查一张图用的两个只读连接：应用库给路径，源库给来源。
pub(crate) struct Lookup<'a> {
    library: &'a Library,
    app: Connection,
    source: Connection,
}
impl<'a> Lookup<'a> {
    pub(crate) fn new(library: &'a Library) -> Result<Self> {
        Ok(Self {
            library,
            app: crate::app_library::read_connection(&library.db())?,
            source: crate::app_library::read_connection(&library.source())?,
        })
    }
    /// 资产 ID 对应的文件。图不在当前图库、没有路径、文件不在或不是普通文件，都拒绝。
    pub(crate) fn locate(&self, id: &str) -> Result<Located> {
        let row: Option<(String, String)> = self
            .app
            .query_row("SELECT coalesce(src_path,''),coalesce(filename,'') FROM assets WHERE uuid=?", [id], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()?;
        let Some((path, filename)) = row.filter(|(path, _)| !path.is_empty()) else {
            return Err(Error::invalid("图片不存在"));
        };
        let path = PathBuf::from(path);
        if !crate::platform_files::regular_file(&path) {
            return Err(Error::invalid("原文件不在了：来源文件夹里的文件可能被移动或删除"));
        }
        let source: Option<String> = self
            .source
            .query_row("SELECT coalesce(source_id,'') FROM assets WHERE uuid=?", [id], |r| r.get(0))
            .optional()?;
        let photos = source.and_then(|sid| {
            self.library.config()["sources"].as_array()?.iter().find_map(|s| {
                (s["id"].as_str() == Some(sid.as_str()) && s["kind"] == "photos")
                    .then(|| s["path"].as_str().map(str::to_owned))
                    .flatten()
            })
        });
        let name = if filename.is_empty() {
            path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
        } else {
            filename
        };
        Ok(Located { uuid: id.to_owned(), path, name, photos })
    }
}

/// 文件或文件夹名里用得了的一段：去掉控制字符和 Windows 不允许的字符，去掉首尾的点和空格，避开 Windows 的保留名，
/// 截到 `max` 个字符；什么都不剩时用 `fallback`。
pub(crate) fn safe_component(name: &str, fallback: &str, max: usize) -> String {
    let cleaned: String =
        name.chars().map(|c| if c.is_control() || "<>:\"/\\|?*".contains(c) { '_' } else { c }).take(max).collect();
    let cleaned = cleaned.trim_matches(['.', ' ']);
    let mut out = if cleaned.is_empty() { fallback.to_owned() } else { cleaned.to_owned() };
    let base = out.split('.').next().unwrap_or_default().trim_end().to_ascii_uppercase();
    if matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$")
        || base
            .strip_prefix("COM")
            .or_else(|| base.strip_prefix("LPT"))
            .is_some_and(|n| matches!(n, "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"))
    {
        out.insert(0, '_');
    }
    out
}

fn random() -> Result<String> {
    let mut raw = [0u8; 8];
    getrandom::fill(&mut raw).map_err(|e| Error::invalid(e.to_string()))?;
    Ok(raw.iter().map(|b| format!("{b:02x}")).collect())
}

/// 临时副本放在哪：系统临时文件夹下的 Illustash/打开的副本。
fn copies_root() -> PathBuf {
    std::env::temp_dir().join("Illustash").join("open-copies")
}
/// 清掉一天前的副本。失败不影响这次。
fn sweep(root: &Path) {
    let Ok(entries) = fs::read_dir(root) else { return };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .is_ok_and(|t| now.duration_since(t).is_ok_and(|age| age > COPY_KEEP));
        if old {
            let path = entry.path();
            // 副本设成了只读：Windows 上删只读文件要先去掉只读。
            if let Ok(files) = fs::read_dir(&path) {
                for file in files.flatten() {
                    if let Ok(meta) = file.metadata() {
                        let mut permissions = meta.permissions();
                        #[allow(clippy::permissions_set_readonly_false)]
                        permissions.set_readonly(false);
                        let _ = fs::set_permissions(file.path(), permissions);
                    }
                }
            }
            let _ = fs::remove_dir_all(path);
        }
    }
}
/// 照片来源的图复制一份到临时文件夹，起「照片」里的原文件名，设成只读。打开和拖出用这份，照片图库包里的文件不交出去。
pub(crate) fn temporary_copy(located: &Located) -> Result<PathBuf> {
    let root = copies_root();
    sweep(&root);
    let (source, name, _) = located.original();
    let directory = root.join(random()?);
    fs::create_dir_all(&directory)?;
    let target = directory.join(safe_component(&name, "image", 120));
    fs::copy(&source, &target)?;
    let mut permissions = fs::metadata(&target)?.permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&target, permissions)?;
    Ok(target)
}

/// 打开和拖出时交给系统的那个文件：文件夹来源是原图本身，照片来源是临时副本。
fn handed_out(located: &Located) -> Result<PathBuf> {
    if located.photos.is_some() {
        temporary_copy(located)
    } else {
        Ok(located.path.clone())
    }
}

fn locally(result: io::Result<()>, what: &str) -> Result<serde_json::Value> {
    result.map(|()| serde_json::json!({"ok": true})).map_err(|e| Error::invalid(format!("{what}：{e}")))
}
fn bridged(route: &str, body: serde_json::Value) -> Result<serde_json::Value> {
    crate::app_desktop::post_files(route, &body, 10)?;
    Ok(serde_json::json!({"ok": true}))
}

/// 用默认应用打开这张图。
pub(crate) fn open(library: &Library, id: &str) -> Result<serde_json::Value> {
    let located = Lookup::new(library)?.locate(id)?;
    let path = handed_out(&located)?;
    if crate::app_desktop::available() {
        return bridged("/platform/files/open", serde_json::json!({"path": path}));
    }
    locally(crate::platform_files::open(&path), "无法打开")
}

/// 在访达或资源管理器里显示这张图。照片来源的不显示。
pub(crate) fn reveal(library: &Library, id: &str) -> Result<serde_json::Value> {
    let located = Lookup::new(library)?.locate(id)?;
    if located.photos.is_some() {
        return Err(Error::invalid("照片里的图不能在访达中显示，可以导出一份"));
    }
    if crate::app_desktop::available() {
        return bridged("/platform/files/reveal", serde_json::json!({"path": located.path}));
    }
    locally(crate::platform_files::reveal(&located.path), "无法显示")
}

/// 显示一个文件夹：导出完成后的「显示」。目录由导出任务自己记下，不来自页面。
pub(crate) fn show_folder(path: &Path) -> Result<serde_json::Value> {
    if crate::app_desktop::available() {
        return bridged("/platform/files/folder", serde_json::json!({"path": path}));
    }
    locally(crate::platform_files::show_folder(path), "无法显示")
}

/// 把这几张图拖到别的应用：外壳起系统的拖放，只允许「复制」。`icon` 是拖动时显示的小图（缩略图缓存，可能没有）。
pub(crate) fn drag(
    library: &Library,
    ids: &[String],
    icon: impl Fn(&Located) -> Option<PathBuf>,
) -> Result<serde_json::Value> {
    if !crate::app_desktop::available() {
        return Err(Error::invalid("拖出需要在 Illustash App 里"));
    }
    if ids.is_empty() || ids.len() > DRAG_LIMIT {
        return Err(Error::invalid("一次拖出的张数不正确"));
    }
    let lookup = Lookup::new(library)?;
    let mut paths = Vec::with_capacity(ids.len());
    let mut picture = None;
    for id in ids {
        let located = lookup.locate(id)?;
        if picture.is_none() {
            picture = icon(&located);
        }
        paths.push(handed_out(&located)?);
    }
    bridged("/platform/files/drag", serde_json::json!({"paths": paths, "icon": picture}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn components_are_portable() {
        assert_eq!(safe_component("初音ミク", "x", 80), "初音ミク");
        assert_eq!(safe_component("a/b:c*?", "x", 80), "a_b_c__");
        assert_eq!(safe_component(" .. ", "未识别角色", 80), "未识别角色");
        assert_eq!(safe_component("CON", "x", 80), "_CON");
        assert_eq!(safe_component("com1.jpg", "x", 80), "_com1.jpg");
        assert_eq!(safe_component("Console", "x", 80), "Console");
        assert_eq!(safe_component(&"長".repeat(100), "x", 80).chars().count(), 80);
    }

    #[test]
    fn copies_take_the_real_files_extension() {
        assert_eq!(with_extension("IMG_0001.HEIC", Path::new("/p/x_1_105_c.jpeg")), "IMG_0001.jpeg");
        assert_eq!(with_extension("IMG_0001.HEIC", Path::new("/p/ABC.heic")), "IMG_0001.heic");
    }
}
