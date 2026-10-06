//! Installed optional worker adapter. Never enabled by the base application.
use std::{
    env,
    path::PathBuf,
    process::{Command, ExitCode},
};
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
    let kind = exe.file_stem().and_then(|s| s.to_str()).unwrap_or("").strip_prefix("illustash-").unwrap_or("");
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
    // Linux keeps its interpreter until model/math acceptance completes.
    if illustash_terms::recognition_compute::production_adapter_enabled() && kind == "recognition" {
        illustash_terms::recognition_compute::producer::worker(&native_args)?;
        return Ok(0);
    }
    if cfg!(any(target_os = "macos", target_os = "windows")) && kind == "index" {
        illustash_terms::index_compute::worker(&native_args)?;
        return Ok(0);
    }
    // Learning, grouping, similarity/relations and mixed events share one research
    // dispatcher. Return before Python or installed script/resource setup for a
    // native request. Windows uses the fixed ILP64 resource; Linux keeps the
    // legacy calculator.
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
    // Diagnostics need no interpreter where every request is native.
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
    let root = exe.parent().ok_or("无法定位 worker 安装目录")?.join("../workers");
    let mut args = native_args.into_iter();
    let script = match kind {
        "recognition" => "web/recognition_worker.py",
        "research" => "web/research_worker.py",
        "index" => "tools/index_worker.py",
        "provenance" => "provenance/native_worker.py",
        _ => {
            return Err(
                "请使用 illustash-research / illustash-index / illustash-provenance / illustash-recognition".into()
            )
        }
    };
    let python = PathBuf::from(
        env::var_os("ILLUSTASH_WORKER_PYTHON").ok_or("请设置 ILLUSTASH_WORKER_PYTHON 为 Python 解释器绝对路径")?,
    );
    if !python.is_absolute() || !python.is_file() {
        return Err("Python 解释器路径无效".into());
    }
    let script = root.join(script);
    if !script.is_file() {
        return Err("缺少研究包资源，请安装可选 workers 包".into());
    }
    let first = args.next().ok_or("需要 request.json response.json 或 --check")?;
    let mut command = Command::new(python);
    // Request/snapshot files are UTF-8, including on non-English Windows installations.
    command.args(["-X", "utf8"]);
    command.env("PYTHONDONTWRITEBYTECODE", "1");
    let resources = illustash_terms::app_resources::default_root();
    command.env("ILLUSTASH_SHARED", illustash_terms::app_resources::shared_path(&resources)?);
    let models = exe.parent().unwrap().join("../resources/models");
    if models.is_dir() {
        command.env("ILLUSTASH_MODELS", models);
    }
    command.env(
        "ILLUSTASH_IQDB_BIN",
        exe.parent().unwrap().join(illustash_terms::platform_process::executable_name("illustash-iqdb")),
    );
    command.env(
        "ILLUSTASH_TERMS_BIN",
        exe.parent().unwrap().join(illustash_terms::platform_process::executable_name("illustash-terms")),
    );
    if first == "--check" {
        let imports = if matches!(kind, "index" | "research" | "recognition") {
            "import numpy, PIL, onnxruntime"
        } else {
            "import numpy, PIL"
        };
        command.args(["-c", imports]);
    } else {
        let response = args.next().ok_or("缺少 response.json")?;
        if args.next().is_some() {
            return Err("多余 worker 参数".into());
        }
        command.arg(script).args([first, response]);
    }
    Ok(illustash_terms::platform_process::run_worker_adapter(&mut command)?)
}
