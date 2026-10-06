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

#[cfg(test)]
mod tests {
    use super::*;
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
