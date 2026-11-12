//! 非阻塞独占锁平台边界；调用方保留等待、取消和错误文案。
use std::{fs::File, io};

/// 在打开锁文件前检查能力，保持不支持平台原来没有文件副作用的行为。
pub fn ensure_supported() -> io::Result<()> {
    if cfg!(any(unix, windows)) {
        Ok(())
    } else {
        Err(io::Error::new(io::ErrorKind::Unsupported, "file locking is unsupported"))
    }
}

pub fn acquire(file: &File) -> io::Result<()> {
    platform::acquire(file)
}

pub fn release(file: &File) -> io::Result<()> {
    platform::release(file)
}

/// 同 `acquire`，但锁不挡读：Windows 的区间锁是强制锁，这里锁文件末尾之后很远的一个字节，
/// 其他进程（备份、同步工具、测试）仍可读这个空的锁文件；Unix 的 flock 本就只是建议锁。
/// 锁随句柄关闭或进程结束释放。
pub fn acquire_unobstructed(file: &File) -> io::Result<()> {
    platform::acquire_unobstructed(file)
}

/// 索引锁原有重试规则：Unix 重试所有错误，Windows 仅锁冲突（33）。
pub fn should_retry(error: &io::Error) -> bool {
    platform::should_retry(error)
}

#[cfg(unix)]
mod platform {
    use super::*;
    use std::os::fd::AsRawFd;

    pub(super) fn acquire(file: &File) -> io::Result<()> {
        // SAFETY: `file` 在借用期间保持打开，文件描述符有效。
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    pub(super) fn acquire_unobstructed(file: &File) -> io::Result<()> {
        acquire(file)
    }

    pub(super) fn release(file: &File) -> io::Result<()> {
        // SAFETY: `file` 在借用期间保持打开，文件描述符有效。
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    pub(super) fn should_retry(_: &io::Error) -> bool {
        true
    }
}

#[cfg(windows)]
mod platform {
    use super::*;
    use std::{ffi::c_void, os::windows::io::AsRawHandle};
    #[link(name = "kernel32")]
    extern "system" {
        fn LockFile(file: *mut c_void, lo: u32, hi: u32, length_lo: u32, length_hi: u32) -> i32;
        fn UnlockFile(file: *mut c_void, lo: u32, hi: u32, length_lo: u32, length_hi: u32) -> i32;
    }
    pub(super) fn acquire(file: &File) -> io::Result<()> {
        // Same region used by the compatibility msvcrt provenance lease.
        // SAFETY: `file` 在借用期间保持打开，原始句柄有效。
        if unsafe { LockFile(file.as_raw_handle(), 0, 0, 1, 0) } == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
    pub(super) fn acquire_unobstructed(file: &File) -> io::Result<()> {
        // Offset 2^62: far past the end of the empty file, so no read of it overlaps the lock.
        // SAFETY: `file` 在借用期间保持打开，原始句柄有效。
        if unsafe { LockFile(file.as_raw_handle(), 0, 0x4000_0000, 1, 0) } == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
    pub(super) fn release(file: &File) -> io::Result<()> {
        // SAFETY: `file` 在借用期间保持打开，原始句柄有效。
        if unsafe { UnlockFile(file.as_raw_handle(), 0, 0, 1, 0) } == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    pub(super) fn should_retry(error: &io::Error) -> bool {
        error.raw_os_error() == Some(33)
    }
}

#[cfg(not(any(unix, windows)))]
mod platform {
    use super::*;
    pub(super) fn acquire(_: &File) -> io::Result<()> {
        ensure_supported()
    }
    pub(super) fn acquire_unobstructed(_: &File) -> io::Result<()> {
        ensure_supported()
    }
    pub(super) fn release(_: &File) -> io::Result<()> {
        ensure_supported()
    }
    pub(super) fn should_retry(_: &io::Error) -> bool {
        false
    }
}

#[cfg(all(test, any(unix, windows)))]
mod tests {
    use super::*;

    #[test]
    fn competing_handles_and_crash_style_close_release_the_lease() {
        let mut nonce = [0u8; 8];
        getrandom::fill(&mut nonce).unwrap();
        let path = std::env::temp_dir().join(format!("lease-{nonce:?}"));
        let a = File::create(&path).unwrap();
        let b = std::fs::OpenOptions::new().read(true).write(true).open(&path).unwrap();
        ensure_supported().unwrap();
        acquire(&a).unwrap();
        let error = acquire(&b).unwrap_err();
        assert!(should_retry(&error));
        #[cfg(windows)]
        assert_eq!(error.raw_os_error(), Some(33));
        drop(a);
        // A child spawned concurrently by another test can inherit `a` until its exec closes it,
        // holding the lease a moment longer; callers retry, so the test does too.
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while let Err(error) = acquire(&b) {
            assert!(should_retry(&error) && std::time::Instant::now() < until, "lease not released: {error}");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        release(&b).unwrap();
        acquire(&b).unwrap();
        drop(b);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn unobstructed_lease_excludes_another_holder_and_stays_readable() {
        let mut nonce = [0u8; 8];
        getrandom::fill(&mut nonce).unwrap();
        let path = std::env::temp_dir().join(format!("owner-{nonce:?}"));
        let a = File::create(&path).unwrap();
        let b = std::fs::OpenOptions::new().read(true).write(true).open(&path).unwrap();
        acquire_unobstructed(&a).unwrap();
        assert!(acquire_unobstructed(&b).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), Vec::<u8>::new());
        drop(a);
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while acquire_unobstructed(&b).is_err() {
            assert!(std::time::Instant::now() < until, "lease not released");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        drop(b);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn index_retry_policy_preserves_platform_errors() {
        let error = io::Error::from_raw_os_error(5);
        #[cfg(unix)]
        assert!(should_retry(&error));
        #[cfg(windows)]
        assert!(!should_retry(&error));
    }
}
