//! A private Windows job: abnormal desktop exit cannot orphan its host.
use std::{ffi::c_void, io, os::windows::io::AsRawHandle, process::Child};
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
}
pub struct Job(*mut c_void);
unsafe impl Send for Job {}
impl Job {
    pub fn assign(child: &Child) -> io::Result<Self> {
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        let job = Self(handle);
        let mut limits = ExtendedLimit::default();
        limits.basic.flags = 0x2000; // JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
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
        // Exiting skips destructors: the kernel closes the desktop job handle.
        std::process::exit(0);
    }

    fn alive(child: &std::process::Child) -> bool {
        #[link(name = "kernel32")]
        extern "system" {
            fn WaitForSingleObject(handle: *mut c_void, milliseconds: u32) -> u32;
        }
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
        let handle = unsafe { OpenProcess(0x100000, 0, pid) };
        if !handle.is_null() {
            assert_eq!(unsafe { WaitForSingleObject(handle, 5000) }, 0, "orphaned host");
            unsafe {
                CloseHandle(handle);
            }
        }
        std::fs::remove_file(path).unwrap();
    }
}
