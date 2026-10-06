//! Native fallback only: no HTTP, token, generation or library session.
use crate::{Error, Result};
use std::{
    io::Read,
    thread,
    time::{Duration, Instant},
};
pub enum Kind {
    Folder,
    Library,
    Zip,
    Archive,
}
impl Kind {
    fn id(&self) -> &'static str {
        match self {
            Self::Folder => "folder",
            Self::Library => "library",
            Self::Zip => "zip",
            Self::Archive => "archive",
        }
    }
    fn title(&self) -> &'static str {
        match self {
            Self::Folder => "选择图片文件夹",
            Self::Library => "选择图库（.pilib）",
            Self::Zip => "选择导出的图库（.zip）",
            Self::Archive => "选择 X 数据归档（.zip）",
        }
    }
}
#[derive(Debug, PartialEq)]
pub enum Choice {
    Selected(Vec<String>),
    Cancelled,
    Unsupported,
}
pub fn choose(kind: Kind, multiple: bool, stopping: &dyn Fn() -> bool, timeout: Duration) -> Result<Choice> {
    if !cfg!(target_os = "macos") {
        return Ok(Choice::Unsupported);
    }
    let title = kind.title();
    let kind = kind.id();
    let command = if matches!(kind, "folder" | "library") { "choose folder" } else { "choose file" };
    let filter = if matches!(kind, "zip" | "archive") { " of type {\"zip\", \"public.zip-archive\"}" } else { "" };
    // Use a list and NUL separators for multiple paths (names may contain newlines).
    let script = if multiple {
        format!("set chosen to {command} with prompt \"{title}\" with multiple selections allowed\nset resultPaths to \"\"\nrepeat with chosenPath in chosen\nset resultPaths to resultPaths & (POSIX path of chosenPath) & (ASCII character 0)\nend repeat\nreturn resultPaths")
    } else {
        format!("POSIX path of ({command} with prompt \"{title}\"{filter})")
    };
    let mut child = std::process::Command::new("/usr/bin/osascript")
        .args(["-e", &script])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    // Drain both pipes while polling: a large multiselect must not deadlock on a full stdout pipe.
    fn drain(pipe: impl Read + Send + 'static) -> thread::JoinHandle<std::io::Result<Vec<u8>>> {
        thread::spawn(move || {
            let mut bytes = Vec::new();
            pipe.take(5 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
            Ok(bytes)
        })
    }
    let stdout = drain(child.stdout.take().expect("picker stdout was piped"));
    let stderr = drain(child.stderr.take().expect("picker stderr was piped"));
    let deadline = Instant::now() + timeout;
    let status = wait_picker(&mut child, stopping, deadline)?;
    let stdout = stdout.join().map_err(|_| Error::Invalid("选择窗口输出读取失败".into()))??;
    let stderr = stderr.join().map_err(|_| Error::Invalid("选择窗口输出读取失败".into()))??;
    if stdout.len() > 5 * 1024 * 1024 || stderr.len() > 5 * 1024 * 1024 {
        return Err(Error::Invalid("选择窗口输出超过限额".into()));
    }
    picker_result(std::process::Output { status, stdout, stderr }, multiple)
}

fn wait_picker(
    child: &mut std::process::Child,
    stopping: &dyn Fn() -> bool,
    deadline: Instant,
) -> Result<std::process::ExitStatus> {
    loop {
        if child.try_wait()?.is_some() {
            break;
        }
        if stopping() || Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Error::Invalid("选择窗口已取消或超时".into()));
        }
        thread::sleep(Duration::from_millis(100));
    }
    child.wait().map_err(Into::into)
}

fn normalize(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        path.to_owned()
    } else {
        trimmed.to_owned()
    }
}
fn picker_result(output: std::process::Output, multiple: bool) -> Result<Choice> {
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() {
        if stderr.contains("-128") {
            return Ok(Choice::Cancelled);
        }
        return Err(Error::Invalid(format!(
            "无法打开选择窗口：{}",
            stderr.chars().rev().take(120).collect::<String>().chars().rev().collect::<String>()
        )));
    }
    let text = std::str::from_utf8(&output.stdout).map_err(|e| Error::Invalid(e.to_string()))?;
    let text = text.strip_suffix('\n').unwrap_or(text);
    let paths = if multiple {
        text.split('\0').filter(|p| !p.is_empty()).map(normalize).collect()
    } else if text.is_empty() {
        vec![]
    } else {
        vec![normalize(text)]
    };
    Ok(Choice::Selected(paths))
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use super::*;
    #[cfg(unix)]
    #[test]
    fn supplied_stop_and_deadline_reap_only_the_synthetic_picker() {
        for stop in [true, false] {
            let mut child = std::process::Command::new("/bin/sh").args(["-c", "exec sleep 30"]).spawn().unwrap();
            let deadline = Instant::now() + if stop { Duration::from_secs(600) } else { Duration::ZERO };
            let error = wait_picker(&mut child, &|| stop, deadline).unwrap_err();
            assert!(matches!(error, Error::Invalid(ref text) if text == "选择窗口已取消或超时"));
            assert!(child.try_wait().unwrap().is_some());
        }
    }
    #[cfg(unix)]
    #[test]
    fn script_results_preserve_special_names_and_cancel() {
        use std::os::unix::process::ExitStatusExt;
        let output = |status, text: &str, error: &str| std::process::Output {
            status: std::process::ExitStatus::from_raw(status),
            stdout: text.as_bytes().to_vec(),
            stderr: error.as_bytes().to_vec(),
        };
        let result = picker_result(output(0, " /synthetic/雪\nPixiv/\0/synthetic/X/\0\n", ""), true).unwrap();
        assert!(matches!(result, Choice::Selected(ref paths) if paths[0] == " /synthetic/雪\nPixiv"));
        assert_eq!(picker_result(output(256, "", "cancel (-128)"), false).unwrap(), Choice::Cancelled);
        assert!(picker_result(output(256, "", "failure"), false).is_err());
    }
}
