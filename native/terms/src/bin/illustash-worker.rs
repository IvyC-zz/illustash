//! Installed optional worker adapter. Never enabled by the base application.
//! One binary serves all four kinds: the host names the kind explicitly per launch
//! (ILLUSTASH_WORKER_KIND); legacy `illustash-<kind>` copies still dispatch by name.
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
    // Launchers set ROOT (or the DLL) for the fixed Windows OpenBLAS; an adapter run
    // directly resolves the same installed resource. Still single-threaded here.
    #[cfg(target_os = "windows")]
    if env::var_os("ILLUSTASH_RESEARCH_OPENBLAS_DLL").is_none() && env::var_os("ILLUSTASH_ROOT").is_none() {
        let root = illustash_terms::app_resources::default_root();
        let root = if root.is_absolute() { root } else { env::current_dir()?.join(root) };
        env::set_var("ILLUSTASH_RESEARCH_OPENBLAS_DLL", root.join("openblas/openblas.dll"));
    }
    // The validated macOS/Windows recognition producer owns CPU model execution.
    if illustash_terms::recognition_compute::production_adapter_enabled() && kind == "recognition" {
        illustash_terms::recognition_compute::producer::worker(&native_args)?;
        return Ok(0);
    }
    if cfg!(any(target_os = "macos", target_os = "windows")) && kind == "index" {
        illustash_terms::index_compute::worker(&native_args)?;
        return Ok(0);
    }
    // Learning, grouping, similarity/relations and mixed events share one research
    // dispatcher. Windows uses the fixed ILP64 resource.
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
    // Diagnostics of the native calculators.
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
    // Local provenance steps share the same gate.
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
    // Everything else is refused: no interpreter fallback remains, and Linux is frozen (README).
    Err(if cfg!(any(target_os = "macos", target_os = "windows")) {
        "不支持的计算请求"
    } else {
        "本平台不提供本地计算"
    }
    .into())
}
