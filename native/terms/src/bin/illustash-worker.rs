//! 安装的可选 worker 适配器。基础应用从不启用它。
//! 一个二进制承担全部四种 worker：宿主每次启动时显式指定种类（ILLUSTASH_WORKER_KIND）；
//! 旧式的 `illustash-<kind>` 副本仍按文件名分派。
use illustash_terms::app_tasks::{worker_kind, WORKER_KIND_ENV};
use std::{env, path::PathBuf, process::ExitCode};
fn main() -> ExitCode {
    match run() {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("可选 worker 不可用：{error}");
            ExitCode::FAILURE
        }
    }
}
fn run() -> Result<u8, Box<dyn std::error::Error>> {
    let exe = env::current_exe()?;
    let stem = exe.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    let requested = env::var(WORKER_KIND_ENV).ok();
    let kind = worker_kind(stem, requested.as_deref())?;
    let native_args: Vec<String> = env::args().skip(1).collect();
    // 启动器为固定的 Windows OpenBLAS 设置 ROOT（或 DLL 路径）；直接运行的适配器解析同一个已安装资源。
    // 此处仍是单线程。
    #[cfg(target_os = "windows")]
    if env::var_os("ILLUSTASH_RESEARCH_OPENBLAS_DLL").is_none() && env::var_os("ILLUSTASH_ROOT").is_none() {
        let root = illustash_terms::app_resources::default_root();
        let root = if root.is_absolute() { root } else { env::current_dir()?.join(root) };
        env::set_var("ILLUSTASH_RESEARCH_OPENBLAS_DLL", root.join("openblas/openblas.dll"));
    }
    // 模型执行（CPU，以及 macOS 的 CoreML、Windows 的 DirectML 加速，#270、#271）归已验证的 macOS/Windows
    // 识别生产者负责。
    if illustash_terms::recognition_compute::production_adapter_enabled() && kind == "recognition" {
        illustash_terms::recognition_compute::producer::worker(&native_args)?;
        return Ok(0);
    }
    if cfg!(any(target_os = "macos", target_os = "windows")) && kind == "index" {
        illustash_terms::index_compute::worker(&native_args)?;
        return Ok(0);
    }
    // 学习、分组、相似度与关系以及混合事件共用一个研究分派器。Windows 使用固定的 ILP64 资源。
    if cfg!(any(target_os = "macos", target_os = "windows"))
        && kind == "research"
        && native_args.len() == 2
        && native_args[0] != "--check"
    {
        let input = PathBuf::from(&native_args[0]);
        let request = illustash_terms::strict_json::parse(&std::fs::read_to_string(&input)?)?;
        if illustash_terms::research_compute::handles(&request) {
            illustash_terms::research_compute::run_files(&input, &PathBuf::from(&native_args[1]))?;
            return Ok(0);
        }
    }
    // 原生计算器的诊断。
    if cfg!(any(target_os = "macos", target_os = "windows"))
        && native_args == ["--check"]
        && matches!(kind, "research" | "provenance")
    {
        if kind == "research" {
            illustash_terms::research_compute::check()?;
        } else {
            illustash_terms::provenance_compute::check()?;
        }
        return Ok(0);
    }
    // 本地溯源步骤共用同一道门槛。
    if cfg!(any(target_os = "macos", target_os = "windows"))
        && kind == "provenance"
        && native_args.len() == 2
        && native_args[0] != "--check"
    {
        let input = PathBuf::from(&native_args[0]);
        let request = illustash_terms::strict_json::parse(&std::fs::read_to_string(&input)?)?;
        if request["step"].as_str().is_some_and(illustash_terms::provenance_compute::handles) {
            illustash_terms::provenance_compute::run_files(&input, &PathBuf::from(&native_args[1]))?;
            return Ok(0);
        }
    }
    // 其余一律拒绝：已不再有解释器回退，Linux 已冻结（见 README）。
    Err(if cfg!(any(target_os = "macos", target_os = "windows")) {
        "不支持的计算请求"
    } else {
        "本平台不提供本地计算"
    }
    .into())
}
