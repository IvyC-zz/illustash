//! 关掉 ONNX Runtime 的遥测（#381）。
//!
//! 上游官方构建默认开启遥测。随包的 macOS 库（PyPI wheel 里的 1.30.0）内置 1DS SDK，经 HTTPS 发往
//! `mobile.events.data.microsoft.com`，并在 `~/Library/Application Support/Microsoft/DeveloperTools/.onnxruntime/`
//! 写持久的设备标识和事件缓存；只有在 ORT 初始化之前设了 `ORT_DISABLE_TELEMETRY`，才完全不建上传器、
//! 不发事件、不写设备标识。C API 的 `DisableTelemetryEvents` 只压掉之后的非必要事件。
//! Windows 的 ETW 不看这个变量，靠 `DisableTelemetryEvents`（`session::initialise` 里的 `with_telemetry(false)`）。
//!
//! 三道，缺一道也不会漏：
//! 1. 宿主启动每个 worker 时写进子进程的环境（`app_tasks::TaskContext::process_observed`）；
//! 2. worker 和合成驱动在 `main` 开头、还没起别的线程时补上（`at_startup`），直接运行也生效；
//! 3. `session::initialise` 看不到这个值就拒绝加载运行库（`require`），以后新增的入口漏设会直接报错。
use crate::{Error, Result};
use std::{ffi::OsStr, process::Command};

/// ONNX Runtime 读取的变量名。
pub const ENV: &str = "ORT_DISABLE_TELEMETRY";
/// 写入的值。上游也接受 true/yes/on，这里只写、只认 `1`。
pub const VALUE: &str = "1";

/// 给即将启动的子进程关掉遥测；覆盖调用方或父进程环境里的其他值。
pub fn child(command: &mut Command) {
    command.env(ENV, VALUE);
}

/// 在 `main` 的第一行调用，当时进程里只有一个线程（`set_var` 在多线程下不安全）。
pub fn at_startup() {
    std::env::set_var(ENV, VALUE);
}

/// `session::initialise` 在加载运行库之前调用：本进程没关掉遥测就拒绝。
pub(super) fn require() -> Result<()> {
    if is_off(std::env::var_os(ENV).as_deref()) {
        Ok(())
    } else {
        Err(Error::invalid("ORT_DISABLE_TELEMETRY 不是 1，拒绝加载 ONNX Runtime（会默认开启微软遥测）"))
    }
}

fn is_off(value: Option<&OsStr>) -> bool {
    value == Some(OsStr::new(VALUE))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_exactly_one_counts_as_off() {
        assert!(is_off(Some(OsStr::new("1"))));
        for value in [None, Some(""), Some("0"), Some("true"), Some(" 1"), Some("false")] {
            assert!(!is_off(value.map(OsStr::new)), "{value:?}");
        }
    }

    /// 加载 ONNX Runtime 只经 `session::initialise` 一处：别处直接用 `ort`，它会自己按默认路径加载库，绕过 `require`。
    #[test]
    fn ort_is_loaded_only_through_initialise() {
        fn uses(text: &str, needle: &str) -> bool {
            text.match_indices(needle).any(|(at, _)| {
                !text[..at].chars().next_back().is_some_and(|c| c.is_alphanumeric() || c == '_' || c == ':')
            })
        }
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut pending = vec![src.join("src"), src.join("examples")];
        // 拆开写，免得这个文件自己被算进去。
        let (prefix, init) = (concat!("or", "t::"), concat!("or", "t::init"));
        let (mut users, mut inits) = (Vec::new(), Vec::new());
        while let Some(dir) = pending.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    pending.push(path);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    let text = std::fs::read_to_string(&path).unwrap();
                    let name = path.strip_prefix(src).unwrap().to_string_lossy().replace('\\', "/");
                    if uses(&text, prefix) {
                        users.push(name.clone());
                    }
                    if uses(&text, init) {
                        inits.push(name);
                    }
                }
            }
        }
        users.sort();
        assert_eq!(
            users,
            [
                "src/recognition_compute/backend.rs",
                "src/recognition_compute/directml.rs",
                "src/recognition_compute/session.rs"
            ]
        );
        assert_eq!(inits, ["src/recognition_compute/session.rs"]);
    }

    #[test]
    fn child_overrides_whatever_the_parent_had() {
        let mut command = Command::new("worker");
        command.env(ENV, "0");
        child(&mut command);
        let set: Vec<_> = command.get_envs().filter(|(k, _)| *k == OsStr::new(ENV)).collect();
        assert_eq!(set, [(OsStr::new(ENV), Some(OsStr::new(VALUE)))]);
    }
}
