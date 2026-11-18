//! 私有的 Windows job：desktop 异常退出时不会留下孤儿宿主进程。宿主这一侧等外壳关管道的做法也在这里。
use std::{
    ffi::c_void,
    io,
    os::windows::io::{AsRawHandle, RawHandle},
    process::Child,
    ptr::null_mut,
    time::Duration,
};
#[repr(C)]
#[derive(Default)]
struct BasicLimit {
    process_time: i64,
    job_time: i64,
    flags: u32,
    min_working_set: usize,
    max_working_set: usize,
    active_processes: u32,
    affinity: usize,
    priority: u32,
    scheduling: u32,
}
#[repr(C)]
#[derive(Default)]
struct ExtendedLimit {
    basic: BasicLimit,
    io: [u64; 6],
    process_memory: usize,
    job_memory: usize,
    peak_process: usize,
    peak_job: usize,
}
#[link(name = "kernel32")]
extern "system" {
    fn CreateJobObjectW(attributes: *const c_void, name: *const u16) -> *mut c_void;
    fn SetInformationJobObject(job: *mut c_void, class: i32, info: *const c_void, size: u32) -> i32;
    fn AssignProcessToJobObject(job: *mut c_void, process: *mut c_void) -> i32;
    fn CloseHandle(handle: *mut c_void) -> i32;
    fn PeekNamedPipe(
        pipe: *mut c_void,
        buffer: *mut c_void,
        size: u32,
        read: *mut u32,
        available: *mut u32,
        left: *mut u32,
    ) -> i32;
    fn ReadFile(file: *mut c_void, buffer: *mut c_void, size: u32, read: *mut u32, overlapped: *mut c_void) -> i32;
}

/// `pipe` 的写端关闭或读出错时返回（#659）。只在管道里有数据时才读，这时读不会阻塞，所以别的线程随时可以
/// 对同一句柄 `PeekNamedPipe`；没有数据就隔 `every` 再查。`pipe` 不是管道（第一次查询就报错，而且不是写端
/// 已关闭）时，退回原来的阻塞读。
pub(super) fn wait_closed(pipe: RawHandle, every: Duration) {
    let mut buffer = [0u8; 64];
    let mut first = true;
    loop {
        let mut available = 0u32;
        // SAFETY: 调用方保证 `pipe` 在调用期间有效；缓冲区传空、长度 0，只写出可读字节数。
        let peeked = unsafe { PeekNamedPipe(pipe, null_mut(), 0, null_mut(), &mut available, null_mut()) } != 0;
        if !peeked {
            // ERROR_BROKEN_PIPE、ERROR_NO_DATA、ERROR_PIPE_NOT_CONNECTED：写端已关闭。
            let closed = matches!(io::Error::last_os_error().raw_os_error(), Some(109 | 232 | 233));
            if first && !closed {
                while read(pipe, &mut buffer) > 0 {}
            }
            return;
        }
        first = false;
        if available == 0 {
            std::thread::sleep(every);
        } else if read(pipe, &mut buffer[..(available as usize).min(64)]) == 0 {
            return;
        }
    }
}

/// 一次同步读，返回读到的字节数；出错或读到 EOF 时返回 0。
fn read(pipe: RawHandle, buffer: &mut [u8]) -> u32 {
    let mut read = 0u32;
    // SAFETY: 调用方保证 `pipe` 有效；`buffer` 在调用期间可写，长度按实际传入；同步读，不用 OVERLAPPED。
    let ok = unsafe { ReadFile(pipe, buffer.as_mut_ptr().cast(), buffer.len() as u32, &mut read, null_mut()) };
    if ok == 0 {
        0
    } else {
        read
    }
}

pub struct Job(*mut c_void);
// SAFETY: 作业句柄是内核对象，可以在任意线程使用和关闭；`Job` 独占它，不共享可变状态。
unsafe impl Send for Job {}
impl Job {
    pub fn assign(child: &Child) -> io::Result<Self> {
        // SAFETY: 两个可选参数都传空指针：默认安全属性、匿名作业。
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        let job = Self(handle);
        let mut limits = ExtendedLimit::default();
        limits.basic.flags = 0x2000; // JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
                                     // SAFETY: `handle` 是刚创建的作业；信息类 9 是 JobObjectExtendedLimitInformation，
                                     // `limits` 是对应布局的 repr(C) 结构，长度按实际大小传入；`child`
                                     // 的原始句柄在借用期间有效。
        unsafe {
            if SetInformationJobObject(
                handle,
                9,
                &limits as *const _ as *const c_void,
                std::mem::size_of_val(&limits) as u32,
            ) == 0
                || AssignProcessToJobObject(handle, child.as_raw_handle()) == 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(job)
    }
}
impl Drop for Job {
    fn drop(&mut self) {
        // SAFETY: `Job` 只包装 CreateJobObjectW 返回的非空句柄，只在这里关闭一次。
        unsafe {
            CloseHandle(self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{process::Command, thread, time::Duration};

    fn fixture(mode: &str, path: &std::path::Path) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--ignored", "--exact", "platform_process::windows_desktop::tests::desktop_fixture"])
            .env("ILLUSTASH_DESKTOP_JOB_FIXTURE", mode)
            .env("ILLUSTASH_DESKTOP_JOB_PID", path);
        command
    }

    #[test]
    #[ignore = "synthetic subprocess fixture"]
    #[allow(clippy::zombie_processes)]
    fn desktop_fixture() {
        let mode = std::env::var("ILLUSTASH_DESKTOP_JOB_FIXTURE").unwrap();
        if mode == "child" {
            thread::sleep(Duration::from_secs(60));
            return;
        }
        let path = std::path::PathBuf::from(std::env::var_os("ILLUSTASH_DESKTOP_JOB_PID").unwrap());
        let child = fixture("child", &path).spawn().unwrap();
        let _job = Job::assign(&child).unwrap();
        std::fs::write(path, child.id().to_string()).unwrap();
        // 直接退出会跳过析构：由内核关闭 desktop 的 job 句柄。
        std::process::exit(0);
    }

    fn alive(child: &std::process::Child) -> bool {
        #[link(name = "kernel32")]
        extern "system" {
            fn WaitForSingleObject(handle: *mut c_void, milliseconds: u32) -> u32;
        }
        // SAFETY: `child` 的原始句柄在借用期间有效；超时 0 只查询状态。
        unsafe { WaitForSingleObject(child.as_raw_handle(), 0) == 258 }
    }

    #[test]
    fn job_close_kills_host_and_preserves_bystander() {
        let path = std::env::temp_dir().join("illustash-unused-desktop-job-pid");
        let mut child = fixture("child", &path).spawn().unwrap();
        let mut bystander = fixture("child", &path).spawn().unwrap();
        let job = Job::assign(&child).unwrap();
        assert!(alive(&child));
        drop(job);
        child.wait().unwrap();
        assert!(!alive(&child));
        assert!(alive(&bystander));
        bystander.kill().unwrap();
        bystander.wait().unwrap();
    }

    #[test]
    fn abrupt_desktop_exit_closes_job_and_kills_host() {
        #[link(name = "kernel32")]
        extern "system" {
            fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut c_void;
            fn WaitForSingleObject(handle: *mut c_void, milliseconds: u32) -> u32;
        }
        let path = std::env::temp_dir().join(format!("illustash-desktop-job-{}", std::process::id()));
        let mut desktop = fixture("desktop", &path).spawn().unwrap();
        assert!(desktop.wait().unwrap().success());
        let pid: u32 = std::fs::read_to_string(&path).unwrap().parse().unwrap();
        // SAFETY: 无指针参数；返回空指针时下面跳过。
        let handle = unsafe { OpenProcess(0x100000, 0, pid) };
        if !handle.is_null() {
            // SAFETY: `handle` 是上面检查过非空的进程句柄。
            assert_eq!(unsafe { WaitForSingleObject(handle, 5000) }, 0, "orphaned host");
            // SAFETY: `handle` 是上面打开的进程句柄，只在这里关闭一次。
            unsafe {
                CloseHandle(handle);
            }
        }
        std::fs::remove_file(path).unwrap();
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn CreatePipe(read: *mut RawHandle, write: *mut RawHandle, attributes: *const c_void, size: u32) -> i32;
        fn WriteFile(
            file: RawHandle,
            buffer: *const c_void,
            size: u32,
            written: *mut u32,
            overlapped: *mut c_void,
        ) -> i32;
    }

    fn peek(pipe: RawHandle) -> bool {
        let mut available = 0u32;
        // SAFETY: `pipe` 是测试持有的管道读端；只写出可读字节数。
        unsafe { PeekNamedPipe(pipe, null_mut(), 0, null_mut(), &mut available, null_mut()) != 0 }
    }

    /// #659：等待期间别的线程对同一句柄的 PeekNamedPipe 立即返回；写进的数据不算关闭，关掉写端才返回。
    #[test]
    fn waiting_for_close_leaves_the_pipe_free_for_peeks() {
        let (mut read_end, mut write_end) = (null_mut(), null_mut());
        // SAFETY: 两个输出指针指向本地变量；默认安全属性和缓冲区大小。
        assert_ne!(unsafe { CreatePipe(&mut read_end, &mut write_end, std::ptr::null(), 0) }, 0);
        let pipe = read_end as usize;
        let waiter = thread::spawn(move || wait_closed(pipe as RawHandle, Duration::from_millis(10)));
        thread::sleep(Duration::from_millis(100));
        let (sent, peeked) = std::sync::mpsc::channel();
        thread::spawn(move || sent.send(peek(pipe as RawHandle)).unwrap());
        assert!(peeked.recv_timeout(crate::test_wait::limit()).expect("PeekNamedPipe 排在挂着的读后面"));
        let mut written = 0u32;
        // SAFETY: 写端是上面创建的；缓冲区是长度 3 的静态字节串。
        assert_ne!(unsafe { WriteFile(write_end, b"abc".as_ptr().cast(), 3, &mut written, null_mut()) }, 0);
        thread::sleep(Duration::from_millis(100));
        assert!(!waiter.is_finished(), "读到数据就当成了关闭");
        // SAFETY: 写端只在这里关闭一次。
        unsafe { CloseHandle(write_end) };
        let deadline = crate::test_wait::deadline();
        while !waiter.is_finished() {
            assert!(std::time::Instant::now() < deadline, "写端关闭后没有返回");
            thread::sleep(Duration::from_millis(10));
        }
        waiter.join().unwrap();
        // SAFETY: 读端只在这里关闭一次，等待线程已经结束。
        unsafe { CloseHandle(read_end) };
    }

    #[test]
    #[ignore = "synthetic subprocess fixture"]
    fn stdin_fixture() {
        if std::env::var_os("ILLUSTASH_STDIN_FIXTURE").is_none() {
            return;
        }
        thread::spawn(crate::platform_process::wait_stdin_closed);
        thread::sleep(Duration::from_millis(200));
        let product = crate::platform_research_math::cross(&[1.0, 2.0], 1, &[3.0, 4.0], 1, 2).unwrap();
        assert_eq!(product, vec![11.0]);
        std::process::exit(0);
    }

    /// #659：宿主等外壳关 stdin 的同时第一次加载 OpenBLAS，不能卡在加载器锁上。
    /// 需要 ILLUSTASH_RESEARCH_OPENBLAS_DLL，和本 crate 的其他 OpenBLAS 测试一样。
    #[test]
    fn loading_openblas_while_waiting_for_stdin_close_does_not_hang() {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--ignored", "--exact", "platform_process::windows_desktop::tests::stdin_fixture"])
            .env("ILLUSTASH_STDIN_FIXTURE", "1")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let deadline = crate::test_wait::deadline();
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if std::time::Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("加载 OpenBLAS 时卡住");
            }
            thread::sleep(Duration::from_millis(20));
        };
        assert!(status.success(), "{status}");
    }
}
