//! 用系统的默认应用打开一张图、在访达或资源管理器里显示它（#450）。只读：不写、不移动、不删除这个文件。
//! 调用方只传宿主按资产 ID 从图库里查出来的路径（见 docs/ARCHITECTURE_DIRECTION.md「用图动作」）；这里再查一遍
//! 是不是存在的普通文件，「打开」另外只放行图片扩展名，免得被拿去启动程序。
use std::{io, path::Path};

/// 「打开」放行的扩展名（不区分大小写）：都是图片，默认应用是看图或编辑图片的程序。
pub const OPENABLE: &[&str] =
    &["jpg", "jpeg", "png", "gif", "webp", "bmp", "tif", "tiff", "heic", "heif", "avif", "jxl"];

/// 绝对路径，指向存在的普通文件（跟随符号链接）。
pub fn regular_file(path: &Path) -> bool {
    path.is_absolute() && std::fs::metadata(path).is_ok_and(|m| m.is_file())
}

/// 扩展名是 [`OPENABLE`] 之一。
pub fn openable(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| OPENABLE.iter().any(|allowed| allowed.eq_ignore_ascii_case(e)))
}

fn refuse(path: &Path, open: bool) -> io::Result<()> {
    if !regular_file(path) {
        return Err(io::Error::new(io::ErrorKind::NotFound, "不是存在的文件"));
    }
    if open && !openable(path) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "不是图片文件"));
    }
    Ok(())
}

/// 用系统给这种文件设的默认应用打开 `path`。
pub fn open(path: &Path) -> io::Result<()> {
    refuse(path, true)?;
    imp::open(path)
}

/// 在访达或资源管理器里显示 `path` 所在的文件夹，并选中它。
pub fn reveal(path: &Path) -> io::Result<()> {
    refuse(path, false)?;
    imp::reveal(path)
}

/// 在访达或资源管理器里打开一个文件夹（导出完成后的「显示」）。
pub fn show_folder(path: &Path) -> io::Result<()> {
    if !path.is_absolute() || !path.is_dir() {
        return Err(io::Error::new(io::ErrorKind::NotFound, "不是存在的文件夹"));
    }
    imp::show_folder(path)
}

#[cfg(target_os = "macos")]
mod imp {
    use std::{io, path::Path, process::Command};

    fn run(command: &mut Command) -> io::Result<()> {
        if command.status()?.success() {
            Ok(())
        } else {
            Err(io::Error::other("open 失败"))
        }
    }
    pub(super) fn open(path: &Path) -> io::Result<()> {
        // 路径都是绝对路径，以 / 开头，不会被当成 open 的选项。
        run(Command::new("/usr/bin/open").arg(path))
    }
    pub(super) fn reveal(path: &Path) -> io::Result<()> {
        run(Command::new("/usr/bin/open").arg("-R").arg(path))
    }
    pub(super) fn show_folder(path: &Path) -> io::Result<()> {
        run(Command::new("/usr/bin/open").arg(path))
    }
}

#[cfg(windows)]
mod imp {
    use std::{ffi::c_void, io, os::windows::ffi::OsStrExt, path::Path, ptr::null};

    #[link(name = "ole32")]
    extern "system" {
        fn CoInitializeEx(reserved: *const c_void, flags: u32) -> i32;
        fn CoUninitialize();
    }
    #[link(name = "shell32")]
    extern "system" {
        fn ShellExecuteW(
            window: *const c_void,
            verb: *const u16,
            file: *const u16,
            parameters: *const u16,
            directory: *const u16,
            show: i32,
        ) -> isize;
        fn ILCreateFromPathW(path: *const u16) -> *mut c_void;
        fn ILFree(list: *mut c_void);
        fn SHOpenFolderAndSelectItems(folder: *const c_void, count: u32, items: *const *const c_void, flags: u32)
            -> i32;
    }

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain(Some(0)).collect()
    }
    /// 在单线程套间里调 `f`：外壳扩展可能需要 STA。调用方都在各自的工作线程上，不在事件循环上。
    fn apartment<T>(f: impl FnOnce() -> T) -> io::Result<T> {
        // SAFETY: 第一个参数按约定为空；标志是 COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE。
        let init = unsafe { CoInitializeEx(null(), 0x2 | 0x4) };
        if init < 0 {
            return Err(io::Error::other("CoInitializeEx 失败"));
        }
        let result = f();
        // SAFETY: 与上面成功的 CoInitializeEx 配对（S_FALSE 也要配对）。
        unsafe { CoUninitialize() };
        Ok(result)
    }
    fn execute(path: &Path) -> io::Result<()> {
        let file = wide(path);
        let verb: Vec<u16> = "open".encode_utf16().chain(Some(0)).collect();
        // SAFETY: file 和 verb 在调用期间有效且以 NUL 结尾；其余指针按约定为空。
        let status = apartment(|| unsafe { ShellExecuteW(null(), verb.as_ptr(), file.as_ptr(), null(), null(), 1) })?;
        if status > 32 {
            Ok(())
        } else {
            Err(io::Error::other("ShellExecuteW 失败"))
        }
    }
    pub(super) fn open(path: &Path) -> io::Result<()> {
        execute(path)
    }
    pub(super) fn show_folder(path: &Path) -> io::Result<()> {
        execute(path)
    }
    pub(super) fn reveal(path: &Path) -> io::Result<()> {
        let file = wide(path);
        let status = apartment(|| {
            // SAFETY: file 以 NUL 结尾，调用期间有效；返回的 ID 列表由下面的 ILFree 释放。
            let list = unsafe { ILCreateFromPathW(file.as_ptr()) };
            if list.is_null() {
                return -1;
            }
            // SAFETY: list 是上面拿到的完整 ID 列表；count 为 0 时打开它的父文件夹并选中它本身。
            let status = unsafe { SHOpenFolderAndSelectItems(list, 0, null(), 0) };
            // SAFETY: list 来自 ILCreateFromPathW，只释放这一次。
            unsafe { ILFree(list) };
            status
        })?;
        if status >= 0 {
            Ok(())
        } else {
            Err(io::Error::other("SHOpenFolderAndSelectItems 失败"))
        }
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod imp {
    use std::{io, path::Path};
    pub(super) fn open(_: &Path) -> io::Result<()> {
        Err(io::ErrorKind::Unsupported.into())
    }
    pub(super) fn reveal(_: &Path) -> io::Result<()> {
        Err(io::ErrorKind::Unsupported.into())
    }
    pub(super) fn show_folder(_: &Path) -> io::Result<()> {
        Err(io::ErrorKind::Unsupported.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_existing_image_files_are_opened() {
        let dir = std::env::temp_dir().join(format!("illustash-files-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("run.command");
        std::fs::write(&script, b"#!/bin/sh\n").unwrap();
        assert!(regular_file(&script) && !openable(&script));
        assert_eq!(open(&script).unwrap_err().kind(), io::ErrorKind::InvalidInput);
        assert_eq!(open(&dir.join("missing.jpg")).unwrap_err().kind(), io::ErrorKind::NotFound);
        assert_eq!(reveal(&dir).unwrap_err().kind(), io::ErrorKind::NotFound);
        assert_eq!(open(Path::new("relative.jpg")).unwrap_err().kind(), io::ErrorKind::NotFound);
        assert!(openable(Path::new("/a/B.JPG")) && openable(Path::new("/a/b.heic")) && !openable(Path::new("/a/b")));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
