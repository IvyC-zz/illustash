//! 进程平台边界：保留 worker 与 desktop 各自的启动和收尾语义。
#[cfg(not(any(unix, windows)))]
use crate::Error;
use crate::Result;
use std::process::{Command, ExitStatus};

#[cfg(windows)]
mod windows_desktop;
#[cfg(windows)]
mod windows_worker;

/// Direct parent identity for the fixed worker owner protocol; Job cleanup remains separate.
#[cfg(any(unix, windows))]
pub fn parent_pid() -> std::io::Result<u32> {
    #[cfg(unix)]
    {
        // SAFETY: getppid 无参数，总是成功。
        Ok(unsafe { libc::getppid() } as u32)
    }
    #[cfg(windows)]
    {
        windows_worker::parent_pid()
    }
}

pub fn worker(
    command: &mut Command,
    mut check: impl FnMut() -> Result<()>,
    mut observe: impl FnMut() -> Result<()>,
) -> Result<ExitStatus> {
    #[cfg(any(unix, windows))]
    {
        check()?;
        #[cfg(unix)]
        let mut group = {
            use std::{os::unix::process::CommandExt, process::Stdio};
            command.process_group(0).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
            ProcessGroup(command.spawn()?)
        };
        #[cfg(windows)]
        let mut group = windows_worker::ProcessJob::spawn(command)?;
        loop {
            check()?;
            // An observer error stops the worker like cancellation: dropping the group kills it.
            observe()?;
            #[cfg(unix)]
            let status = group.0.try_wait()?;
            #[cfg(windows)]
            let status = group.child.try_wait()?;
            if let Some(status) = status {
                return Ok(status);
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (command, check, observe);
        Err(Error::Invalid("此平台尚未提供 worker 进程树收尾；拒绝启动".into()))
    }
}
#[cfg(unix)]
struct ProcessGroup(std::process::Child);
#[cfg(unix)]
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        // SIGKILL is deterministic even for uncooperative children and leaders already exited.
        // SAFETY: kill 只接收整数；负 PID 指向 `spawn` 时用 process_group(0) 建立的这个进程组。
        unsafe {
            libc::kill(-(self.0.id() as i32), libc::SIGKILL);
        }
        let _ = self.0.wait();
    }
}

use std::sync::atomic::{AtomicBool, Ordering};
static STOP: AtomicBool = AtomicBool::new(false);
#[cfg(unix)]
extern "C" fn signal_stop(_: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
}
#[cfg(windows)]
unsafe extern "system" fn console_stop(event: u32) -> i32 {
    if event <= 2 || event == 5 || event == 6 {
        STOP.store(true, Ordering::SeqCst);
        1
    } else {
        0
    }
}
pub fn install_signals() {
    STOP.store(false, Ordering::SeqCst);
    #[cfg(unix)]
    // SAFETY: `signal_stop` 是 extern "C" 函数，只写一个原子量，在信号处理函数里可以安全调用。
    unsafe {
        libc::signal(libc::SIGINT, signal_stop as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, signal_stop as *const () as libc::sighandler_t);
    }
    #[cfg(windows)]
    // SAFETY: `console_stop` 符合 PHANDLER_ROUTINE 签名，只写一个原子量；程序运行期间一直有效。
    unsafe {
        #[link(name = "kernel32")]
        extern "system" {
            fn SetConsoleCtrlHandler(handler: Option<unsafe extern "system" fn(u32) -> i32>, add: i32) -> i32;
        }
        SetConsoleCtrlHandler(Some(console_stop), 1);
    }
}
pub fn request_stop() {
    STOP.store(true, Ordering::SeqCst);
}

pub fn stopping() -> bool {
    STOP.load(Ordering::SeqCst)
}

/// Workers use separate process groups, so timeout cleanup must include descendants.
/// Read only PID/PPID metadata and start from the still-unreaped owned host.
#[cfg(unix)]
pub fn kill_owned_tree(pid: u32) {
    if let Ok(output) = Command::new("/bin/ps").args(["-axo", "pid=,ppid="]).output() {
        if output.status.success() {
            let edges: Vec<(u32, u32)> = String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter_map(|line| {
                    let mut fields = line.split_whitespace();
                    Some((fields.next()?.parse().ok()?, fields.next()?.parse().ok()?))
                })
                .collect();
            let mut owned = vec![pid];
            let mut cursor = 0;
            while cursor < owned.len() {
                let parent = owned[cursor];
                for &(child, ppid) in &edges {
                    if ppid == parent && !owned.contains(&child) {
                        owned.push(child);
                    }
                }
                cursor += 1;
            }
            for child in owned.into_iter().skip(1).rev() {
                // SAFETY: kill 只接收整数；PID 来自上面以 `pid` 为根枚举出的后代。
                unsafe {
                    libc::kill(child as i32, libc::SIGKILL);
                }
            }
        } else {
            eprintln!("无法枚举本地服务后代；继续终止自身进程组");
        }
    } else {
        eprintln!("无法启动进程收尾检查；继续终止自身进程组");
    }
    // SAFETY: killpg 只接收整数；`pid` 是仍未回收的自有进程组组长，组号不会被复用。
    unsafe {
        libc::killpg(pid as i32, libc::SIGKILL);
    }
}

#[cfg(not(unix))]
pub fn kill_owned_tree(_pid: u32) {}

/// Desktop retains its existing post-spawn job assignment and piped I/O.
#[cfg(windows)]
pub use windows_desktop::Job as DesktopJob;
#[cfg(not(windows))]
pub struct DesktopJob;
#[cfg(not(windows))]
impl DesktopJob {
    pub fn assign(_child: &std::process::Child) -> std::io::Result<Self> {
        Ok(Self)
    }
}
pub fn prepare_desktop(command: &mut Command) {
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(command, 0);
    #[cfg(not(unix))]
    let _ = command;
}

pub struct DesktopParent {
    #[cfg(unix)]
    pid: Option<libc::pid_t>,
}
impl DesktopParent {
    pub fn capture() -> Self {
        Self {
            #[cfg(unix)]
            // SAFETY: getppid 无参数，总是成功。
            pid: std::env::var_os("ILLUSTASH_DESKTOP_BRIDGE").map(|_| unsafe { libc::getppid() }),
        }
    }
    pub fn lost(&self) -> bool {
        #[cfg(unix)]
        {
            // SAFETY: getppid 无参数，总是成功。
            self.pid.is_some_and(|pid| unsafe { libc::getppid() } != pid)
        }
        #[cfg(not(unix))]
        {
            false
        }
    }
}

pub fn executable_name(stem: &str) -> String {
    if cfg!(windows) {
        format!("{stem}.exe")
    } else {
        stem.into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn executable_name_preserves_platform_suffix() {
        #[cfg(windows)]
        assert_eq!(executable_name("illustash-terms"), "illustash-terms.exe");
        #[cfg(not(windows))]
        assert_eq!(executable_name("illustash-terms"), "illustash-terms");
    }

    #[test]
    #[ignore = "isolated synthetic stop-signal subprocess"]
    fn stop_signal_fixture() {
        #[cfg(windows)]
        // SAFETY: 这些控制台函数只接收整数；在隔离的测试子进程里调用。
        unsafe {
            #[link(name = "kernel32")]
            extern "system" {
                fn FreeConsole() -> i32;
                fn AllocConsole() -> i32;
                fn GenerateConsoleCtrlEvent(event: u32, group: u32) -> i32;
            }
            FreeConsole();
            assert_ne!(AllocConsole(), 0);
            install_signals();
            assert_ne!(GenerateConsoleCtrlEvent(1, 0), 0);
        }
        #[cfg(unix)]
        {
            install_signals();
            // SAFETY: raise 只接收整数；处理函数已由上一行安装。
            unsafe {
                libc::raise(libc::SIGTERM);
            }
        }
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !stopping() && std::time::Instant::now() < until {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(stopping());
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn installed_stop_signal_reaches_isolated_process() {
        let status = Command::new(std::env::current_exe().unwrap())
            .args(["--ignored", "--exact", "platform_process::tests::stop_signal_fixture"])
            .status()
            .unwrap();
        assert!(status.success());
    }
}
