//! 只是原生的后备实现：不涉及 HTTP、token、generation 或图库会话。
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
    /// 手动下载的模型文件（#168）：可多选，ONNX、JSON 或 ZIP。
    Models,
    /// 便携人工结论文件。
    HumanConclusions,
    /// 选中的图导出到哪个文件夹（#450）。
    Export,
}
impl Kind {
    fn id(&self) -> &'static str {
        match self {
            Self::Folder => "folder",
            Self::Library => "library",
            Self::Zip => "zip",
            Self::Archive => "archive",
            Self::Models => "models",
            Self::HumanConclusions => "human-conclusions",
            Self::Export => "export",
        }
    }
    /// 选择窗口的提示语，用中文写；调用方可以显示翻译后的文案（即 `choose` 的 `title`）。
    pub fn title(&self) -> &'static str {
        match self {
            Self::Folder => "选择图片文件夹",
            Self::Library => "选择图库（.pilib）",
            Self::Zip => "选择导出的图库（.zip）",
            Self::Archive => "选择 X 数据归档（.zip）",
            Self::Models => "选择下载好的模型文件（.onnx、.json 或 .zip）",
            Self::HumanConclusions => "选择人工结论文件（.illustash-human.jsonl）",
            Self::Export => "选择导出到的文件夹",
        }
    }
}
#[derive(Debug, PartialEq)]
pub enum Choice {
    Selected(Vec<String>),
    Cancelled,
    Unsupported,
}
pub fn choose(
    kind: Kind,
    title: &str,
    multiple: bool,
    stopping: &dyn Fn() -> bool,
    timeout: Duration,
) -> Result<Choice> {
    if !cfg!(target_os = "macos") {
        return Ok(Choice::Unsupported);
    }
    // 转成 AppleScript 字符串字面量。
    let title = title.replace('\\', "\\\\").replace('"', "\\\"");
    let kind = kind.id();
    let command = if matches!(kind, "folder" | "library" | "export") { "choose folder" } else { "choose file" };
    let filter = if matches!(kind, "zip" | "archive") { " of type {\"zip\", \"public.zip-archive\"}" } else { "" };
    // 多个路径用列表和 NUL 分隔（文件名可能含换行）。
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
    // 轮询的同时读空两个管道：多选的结果很大时，不能因 stdout 管道写满而死锁。
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
            "无法打开选择窗口：{reason}",
            reason = stderr.chars().rev().take(120).collect::<String>().chars().rev().collect::<String>()
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
