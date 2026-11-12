pub use illustash_platform::platform_process::*;
#[cfg(all(test, windows))]
mod windows_worker {
    #[cfg(test)]
    mod tests {
        use std::{ffi::c_void, os::windows::io::AsRawHandle, process::Command};
        type Handle = *mut c_void;
        #[link(name = "kernel32")]
        extern "system" {
            fn CloseHandle(handle: Handle) -> i32;
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
        use crate::app_tasks::{Task, TaskIdentity, TaskOutcome};
        use std::{
            path::Path,
            thread,
            time::{Duration, Instant},
        };
        #[link(name = "kernel32")]
        extern "system" {
            fn GetCurrentProcess() -> Handle;
            fn OpenProcess(access: u32, inherit: i32, pid: u32) -> Handle;
            fn DuplicateHandle(
                source: Handle,
                handle: Handle,
                target: Handle,
                copy: *mut Handle,
                access: u32,
                inherit: i32,
                options: u32,
            ) -> i32;
            fn GetProcessId(process: Handle) -> u32;
            fn GetProcessTimes(
                process: Handle,
                creation: *mut u64,
                exit: *mut u64,
                kernel: *mut u64,
                user: *mut u64,
            ) -> i32;
            fn GetExitCodeProcess(process: Handle, code: *mut u32) -> i32;
            fn WaitForSingleObject(handle: Handle, milliseconds: u32) -> u32;
        }
        fn creation_time(process: Handle) -> u64 {
            let (mut creation, mut exit, mut kernel, mut user) = (0, 0, 0, 0);
            // SAFETY: `process` 是调用方持有的有效进程句柄，四个出参都是本地变量。
            assert_ne!(unsafe { GetProcessTimes(process, &mut creation, &mut exit, &mut kernel, &mut user) }, 0);
            creation
        }
        fn fixture_command(mode: &str, path: &Path) -> Command {
            let mut command = Command::new(std::env::current_exe().unwrap());
            command
                .args(["--ignored", "--exact", "platform_process::windows_worker::tests::worker_fixture"])
                .env("ILLUSTASH_JOB_FIXTURE", mode)
                .env("ILLUSTASH_JOB_PID", path)
                .env("ILLUSTASH_JOB_OBSERVER", std::process::id().to_string());
            command
        }
        #[test]
        #[allow(clippy::zombie_processes)] // 故意留下一个存活的后代进程，交给所属的 Job 回收。
        #[ignore = "subprocess fixture, invoked only by owned job tests"]
        fn worker_fixture() {
            let mode = std::env::var("ILLUSTASH_JOB_FIXTURE").unwrap();
            if mode == "child" {
                thread::sleep(Duration::from_secs(60));
                return;
            }
            let path = std::env::var("ILLUSTASH_JOB_PID").unwrap();
            let child = fixture_command("child", Path::new(&path)).spawn().unwrap();
            // 在这个首进程退出之前，把原始子进程的句柄发布到负责观察的测试进程里。
            // 保留的句柄对象可防止之后 PID 被复用造成的竞争。
            let observer = std::env::var("ILLUSTASH_JOB_OBSERVER").unwrap().parse().unwrap();
            // SAFETY: 无指针参数；下一行断言句柄非空。
            let target = unsafe { OpenProcess(0x0040, 0, observer) }; // PROCESS_DUP_HANDLE，仅用于测试夹具
            assert!(!target.is_null());
            let target = OwnedHandle(target);
            let mut copy = std::ptr::null_mut();
            assert_ne!(
                // SAFETY: 源进程用当前进程伪句柄，`child` 的原始句柄在 `child` 存活期间有效，`target`
                // 是上面打开的观察进程，`copy` 是本地出参。
                unsafe {
                    DuplicateHandle(
                        GetCurrentProcess(),
                        child.as_raw_handle(),
                        target.0,
                        &mut copy,
                        0x100000 | 0x1000,
                        0,
                        0,
                    )
                },
                0
            ); // SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION，不可继承
            let creation = creation_time(child.as_raw_handle());
            // `copy` 属于观察进程的句柄表；只有观察进程关闭它。
            let ready = Path::new(&path).with_extension("ready");
            std::fs::write(&ready, format!("{} {} {creation}", child.id(), copy as usize)).unwrap();
            std::fs::rename(ready, &path).unwrap();
            match mode.as_str() {
                "ok" => (),
                "fail" => std::process::exit(7),
                _ => thread::sleep(Duration::from_secs(60)),
            }
        }
        fn published_identity(text: &str) -> Option<(u32, usize, u64)> {
            let mut fields = text.split_whitespace();
            let identity = (fields.next()?.parse().ok()?, fields.next()?.parse().ok()?, fields.next()?.parse().ok()?);
            fields.next().is_none().then_some(identity)
        }
        #[test]
        fn timeout_cancel_success_and_failed_parent_clean_descendants() {
            for mode in ["timeout", "cancel", "ok", "fail"] {
                let root = std::env::temp_dir();
                let identity = TaskIdentity::at(&root, "synthetic", 1, "job-test").unwrap();
                let path = root.join(format!("job-{}", identity.task_id));
                let output = path.clone();
                let timeout = (mode == "timeout").then_some(Duration::from_secs(2));
                let mut task = Task::start(identity, timeout, move |ctx| {
                    let status = ctx.process(&mut fixture_command(mode, &output))?;
                    if status.success() {
                        Ok(())
                    } else {
                        Err(crate::Error::Invalid("worker failed".into()))
                    }
                });
                let deadline = Instant::now() + Duration::from_secs(5);
                let (pid, handle, creation) = loop {
                    if let Ok(text) = std::fs::read_to_string(&path) {
                        if let Some(identity) = published_identity(&text) {
                            break identity;
                        }
                    }
                    assert!(Instant::now() < deadline, "no worker descendant: {:?}", task.outcome());
                    thread::sleep(Duration::from_millis(10));
                };
                let descendant = OwnedHandle(handle as Handle);
                // SAFETY: `descendant` 包装观察进程收到的复制句柄，此时尚未关闭。
                assert_eq!(unsafe { GetProcessId(descendant.0) }, pid);
                assert_eq!(creation_time(descendant.0), creation);
                if mode == "cancel" {
                    task.stop();
                } else {
                    assert!(task.wait(Duration::from_secs(5)));
                }
                // TerminateJobObject 会停止执行，但进程对象变为有信号状态是异步的。用户批准在既有的
                // 5 秒观察上限内做一次内核等待（#124）；生产环境的清理和超时语义保持不变。
                let started = Instant::now();
                // SAFETY: `descendant` 包装观察进程收到的复制句柄，此时尚未关闭。
                let wait = unsafe { WaitForSingleObject(descendant.0, 5000) };
                let mut exit = 0;
                // SAFETY: `descendant` 的句柄尚未关闭，`exit` 是本地出参。
                assert_ne!(unsafe { GetExitCodeProcess(descendant.0, &mut exit) }, 0);
                eprintln!("worker cleanup mode={mode} pid={pid} creation={creation} wait={wait} exit={exit} wait_us={} outcome={:?}", started.elapsed().as_micros(), task.outcome());
                assert_eq!(wait, 0, "residual worker descendant {pid}; mode={mode}; outcome={:?}", task.outcome());
                // SAFETY: `descendant` 包装观察进程收到的复制句柄，此时尚未关闭。
                assert_eq!(unsafe { GetProcessId(descendant.0) }, pid);
                assert_eq!(creation_time(descendant.0), creation);
                assert_eq!(exit, 1, "worker descendant did not exit through Job termination");
                match mode {
                    "timeout" | "cancel" => assert_eq!(task.outcome(), TaskOutcome::Cancelled),
                    "ok" => assert_eq!(task.outcome(), TaskOutcome::Done),
                    _ => assert!(matches!(task.outcome(), TaskOutcome::Failed(_))),
                }
                std::fs::remove_file(path).unwrap();
            }
        }
    }
}
