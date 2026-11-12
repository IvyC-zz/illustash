//! Start suspended, attach to a private kill-on-close job, then resume. No child can
//! spawn an unowned descendant between CreateProcess and AssignProcessToJobObject.
use std::{
    ffi::c_void,
    io,
    os::windows::{io::AsRawHandle, process::CommandExt},
    process::{Child, Command, Stdio},
};
type Handle = *mut c_void;
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
#[repr(C)]
#[derive(Default)]
struct Accounting {
    user_time: i64,
    kernel_time: i64,
    period_user: i64,
    period_kernel: i64,
    faults: u32,
    total: u32,
    active: u32,
    terminated: u32,
}
#[repr(C)]
#[derive(Default)]
struct ThreadEntry {
    size: u32,
    usage: u32,
    id: u32,
    process: u32,
    priority: i32,
    delta: i32,
    flags: u32,
}
#[link(name = "kernel32")]
extern "system" {
    fn CreateJobObjectW(attributes: *const c_void, name: *const u16) -> Handle;
    fn SetInformationJobObject(job: Handle, class: i32, info: *const c_void, size: u32) -> i32;
    fn QueryInformationJobObject(job: Handle, class: i32, info: *mut c_void, size: u32, returned: *mut u32) -> i32;
    fn AssignProcessToJobObject(job: Handle, process: Handle) -> i32;
    fn TerminateJobObject(job: Handle, code: u32) -> i32;
    fn CreateToolhelp32Snapshot(flags: u32, process: u32) -> Handle;
    fn Thread32First(snapshot: Handle, entry: *mut ThreadEntry) -> i32;
    fn Thread32Next(snapshot: Handle, entry: *mut ThreadEntry) -> i32;
    fn OpenThread(access: u32, inherit: i32, id: u32) -> Handle;
    fn ResumeThread(thread: Handle) -> u32;
    fn CloseHandle(handle: Handle) -> i32;
}
#[repr(C)]
struct ProcessEntry {
    size: u32,
    usage: u32,
    id: u32,
    heap: usize,
    module: u32,
    threads: u32,
    parent: u32,
    priority: i32,
    flags: u32,
    executable: [u16; 260],
}
#[link(name = "kernel32")]
extern "system" {
    fn Process32FirstW(snapshot: Handle, entry: *mut ProcessEntry) -> i32;
    fn Process32NextW(snapshot: Handle, entry: *mut ProcessEntry) -> i32;
}
/// Same direct-parent identity that the legacy worker obtains with os.getppid().
pub(super) fn parent_pid() -> io::Result<u32> {
    // SAFETY: 无指针参数；返回的句柄在下面检查 INVALID_HANDLE_VALUE 后交给 `OwnedHandle`。
    let raw = unsafe { CreateToolhelp32Snapshot(2, 0) }; // TH32CS_SNAPPROCESS
    if raw as isize == -1 {
        return Err(io::Error::last_os_error());
    }
    let snapshot = OwnedHandle(raw);
    // SAFETY: ProcessEntry 是只含整数和 u16 数组的 repr(C) 结构，全零是合法值。
    let mut entry: ProcessEntry = unsafe { std::mem::zeroed() };
    entry.size = std::mem::size_of::<ProcessEntry>() as u32;
    // SAFETY: `snapshot` 是有效的进程快照句柄，`entry` 是 size 已正确填写的本地结构。
    let mut present = unsafe { Process32FirstW(snapshot.0, &mut entry) };
    while present != 0 {
        if entry.id == std::process::id() {
            return Ok(entry.parent);
        }
        // SAFETY: `snapshot` 是有效的进程快照句柄，`entry` 是 size 已正确填写的本地结构。
        present = unsafe { Process32NextW(snapshot.0, &mut entry) };
    }
    Err(io::Error::last_os_error())
}
struct OwnedHandle(Handle);
impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: `OwnedHandle` 只包装已检查有效的句柄，每个只在这里关闭一次。
        unsafe {
            CloseHandle(self.0);
        }
    }
}
pub struct ProcessJob {
    pub child: Child,
    job: OwnedHandle,
}
impl ProcessJob {
    pub fn spawn(command: &mut Command) -> io::Result<Self> {
        // SAFETY: 两个可选参数都传空指针：默认安全属性、匿名作业。
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        let job = OwnedHandle(handle);
        let mut limits = ExtendedLimit::default();
        limits.basic.flags = 0x2000; // JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, no breakaway
                                     // SAFETY: `handle` 是刚创建的作业；信息类 9 是 JobObjectExtendedLimitInformation，
                                     // `limits` 是对应布局的 repr(C) 结构，长度按实际大小传入。
        if unsafe {
            SetInformationJobObject(
                handle,
                9,
                &limits as *const _ as *const c_void,
                std::mem::size_of_val(&limits) as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        command
            .creation_flags(0x00000004 | 0x08000000) // CREATE_SUSPENDED | CREATE_NO_WINDOW
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = command.spawn()?;
        // SAFETY: `handle` 是存活的作业，`child` 的原始句柄在 `child` 存活期间有效。
        if unsafe { AssignProcessToJobObject(handle, child.as_raw_handle()) } == 0 {
            let error = io::Error::last_os_error();
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        let group = Self { child, job };
        resume(group.child.id())?;
        Ok(group)
    }
}
fn resume(process: u32) -> io::Result<()> {
    // Rust 1.82 has no stable ChildExt::main_thread_handle. A suspended freshly
    // created process has one thread; use the documented Toolhelp/ResumeThread API.
    // SAFETY: 无指针参数；返回的句柄在下面检查 INVALID_HANDLE_VALUE 后交给 `OwnedHandle`。
    let handle = unsafe { CreateToolhelp32Snapshot(4, 0) }; // TH32CS_SNAPTHREAD
    if handle == -1isize as Handle {
        return Err(io::Error::last_os_error());
    }
    let snapshot = OwnedHandle(handle);
    let mut entry = ThreadEntry { size: std::mem::size_of::<ThreadEntry>() as u32, ..Default::default() };
    // SAFETY: `snapshot` 是有效的线程快照句柄，`entry` 是 size 已正确填写的本地结构。
    let mut found = unsafe { Thread32First(snapshot.0, &mut entry) };
    while found != 0 {
        if entry.process == process {
            // SAFETY: 无指针参数；返回的句柄在下面检查空值后交给 `OwnedHandle`。
            let handle = unsafe { OpenThread(2, 0, entry.id) }; // THREAD_SUSPEND_RESUME
            if handle.is_null() {
                return Err(io::Error::last_os_error());
            }
            let thread = OwnedHandle(handle);
            // SAFETY: `thread` 是刚以 THREAD_SUSPEND_RESUME 权限打开的有效线程句柄。
            if unsafe { ResumeThread(thread.0) } == u32::MAX {
                return Err(io::Error::last_os_error());
            }
            return Ok(());
        }
        // SAFETY: `snapshot` 是有效的线程快照句柄，`entry` 是 size 已正确填写的本地结构。
        found = unsafe { Thread32Next(snapshot.0, &mut entry) };
    }
    Err(io::Error::new(io::ErrorKind::NotFound, "worker primary thread missing"))
}
impl Drop for ProcessJob {
    fn drop(&mut self) {
        // Keep the job open until every descendant has exited, even if the leader
        // has already exited. Closing the last handle also protects host crashes.
        // SAFETY: `self.job` 在 `ProcessJob` 存活期间有效；信息类 1 是 JobObjectBasicAccountingInformation，`info`
        // 是对应布局的 repr(C) 结构，长度按实际大小传入。
        unsafe {
            if TerminateJobObject(self.job.0, 1) != 0 {
                loop {
                    let mut info = Accounting::default();
                    if QueryInformationJobObject(
                        self.job.0,
                        1,
                        &mut info as *mut _ as *mut c_void,
                        std::mem::size_of_val(&info) as u32,
                        std::ptr::null_mut(),
                    ) == 0
                        || info.active == 0
                    {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            }
        }
        let _ = self.child.wait();
    }
}
