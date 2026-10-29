//! Host filesystem details. Callers retain creation, validation and error policy.
use std::{fs, io, path::Path};

pub fn private_file_options(options: &mut fs::OpenOptions) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    #[cfg(not(unix))]
    let _ = options;
}

pub fn set_private_file_permissions(file: &fs::File) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    let _ = file;
    Ok(())
}

pub fn create_private_dir(path: &Path) -> io::Result<()> {
    fs::create_dir(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

pub fn read_nofollow_options() -> fs::OpenOptions {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    options
}

pub fn extension_matches(actual: &[u8], expected: &[u8]) -> bool {
    if cfg!(windows) {
        actual.eq_ignore_ascii_case(expected)
    } else {
        actual == expected
    }
}

/// Bytes the current user can still write on the volume holding `path` (an existing directory).
pub fn available_space(path: &Path) -> io::Result<u64> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let path = std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(io::Error::other)?;
        let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        // SAFETY: `path` 是存活的 NUL 结尾 C 字符串，`stat` 指向足够大的未初始化缓冲区，由 statvfs 填写。
        if unsafe { libc::statvfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: 上面的 statvfs 返回 0，已完整写入 `stat`。
        let stat = unsafe { stat.assume_init() };
        #[allow(clippy::unnecessary_cast)] // Field widths differ between Unix platforms.
        Ok((stat.f_bavail as u64).saturating_mul(stat.f_frsize as u64))
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        #[link(name = "kernel32")]
        extern "system" {
            fn GetDiskFreeSpaceExW(path: *const u16, available: *mut u64, total: *mut u64, free: *mut u64) -> i32;
        }
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut available = 0u64;
        // SAFETY: `wide` 是存活的 NUL 结尾 UTF-16 路径，`available` 是本地出参，另外两个出参可以为空。
        if unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut available, std::ptr::null_mut(), std::ptr::null_mut()) }
            == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(available)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        Err(io::Error::new(io::ErrorKind::Unsupported, "free space is unknown on this platform"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn available_space_reports_the_volume_and_rejects_missing_paths() {
        let here = std::env::temp_dir();
        assert!(available_space(&here).unwrap() > 0);
        assert!(available_space(&here.join("illustash-no-such-directory-for-space")).is_err());
    }
    #[test]
    fn private_permissions_nofollow_and_extension_contract() {
        let root = std::env::temp_dir().join(format!("illustash-platform-fs-{}", crate::store::new_key().unwrap()));
        create_private_dir(&root).unwrap();
        assert!(create_private_dir(&root).is_err());
        let file = root.join("synthetic.bin");
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        private_file_options(&mut options);
        drop(options.open(&file).unwrap());
        assert!(options.open(&file).is_err());
        assert!(read_nofollow_options().open(&file).is_ok());
        let existing = fs::OpenOptions::new().write(true).open(&file).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            existing.set_permissions(fs::Permissions::from_mode(0o644)).unwrap();
        }
        set_private_file_permissions(&existing).unwrap();
        drop(existing);
        #[cfg(unix)]
        {
            use std::os::unix::fs::{symlink, PermissionsExt};
            assert_eq!(fs::metadata(&root).unwrap().permissions().mode() & 0o777, 0o700);
            assert_eq!(fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
            let link = root.join("synthetic-link");
            symlink(&file, &link).unwrap();
            assert_eq!(read_nofollow_options().open(link).unwrap_err().raw_os_error(), Some(libc::ELOOP));
        }
        assert!(extension_matches(b".jpg", b".jpg"));
        assert!(!extension_matches(b".jpeg", b".jpg"));
        assert_eq!(extension_matches(b".JpG", b".jpg"), cfg!(windows));
        fs::remove_dir_all(root).unwrap();
    }
}
