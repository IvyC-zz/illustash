//! 计算进程只传预定义阶段和类型。自由文本及路径始终走原有 stderr，供自检界面使用。
use std::io;

const PREFIX: &str = "ILLUSTASH-DIAGNOSTIC/1 ";

pub enum IoPhase {
    ReadRequest,
    ResolveRuntime,
}

pub fn worker_io(phase: IoPhase, error: &io::Error) {
    let phase = match phase {
        IoPhase::ReadRequest => "read-request",
        IoPhase::ResolveRuntime => "resolve-runtime",
    };
    let kind = match error.kind() {
        io::ErrorKind::NotFound => "not-found",
        io::ErrorKind::PermissionDenied => "permission",
        io::ErrorKind::InvalidData => "invalid-data",
        _ => "io",
    };
    let code = error.raw_os_error().filter(|code| (1..=65535).contains(code)).unwrap_or_default();
    eprintln!("{PREFIX}{phase} {kind} {code}");
}

pub fn worker_invalid_request() {
    eprintln!("{PREFIX}decode-request invalid-json 0");
}

pub fn worker_runtime_abi() {
    eprintln!("{PREFIX}init-runtime loader-panic 0");
}

pub enum OrtPhase {
    Initialize,
    LoadModel,
}

const ORT_CODES: &[(ort::ErrorCode, &str)] = &[
    (ort::ErrorCode::InvalidArgument, "ORT_INVALID_ARGUMENT"),
    (ort::ErrorCode::NoSuchFile, "ORT_NO_SUCHFILE"),
    (ort::ErrorCode::NoModel, "ORT_NO_MODEL"),
    (ort::ErrorCode::EngineError, "ORT_ENGINE_ERROR"),
    (ort::ErrorCode::RuntimeException, "ORT_RUNTIME_EXCEPTION"),
    (ort::ErrorCode::InvalidProtobuf, "ORT_INVALID_PROTOBUF"),
    (ort::ErrorCode::ModelLoaded, "ORT_MODEL_LOADED"),
    (ort::ErrorCode::NotImplemented, "ORT_NOT_IMPLEMENTED"),
    (ort::ErrorCode::InvalidGraph, "ORT_INVALID_GRAPH"),
    (ort::ErrorCode::ExecutionProviderFailure, "ORT_EP_FAIL"),
    (ort::ErrorCode::GenericFailure, "ORT_FAIL"),
];

pub fn worker_ort(phase: OrtPhase, code: ort::ErrorCode) {
    let phase = match phase {
        OrtPhase::Initialize => "init-runtime",
        OrtPhase::LoadModel => "load-model",
    };
    let kind = ORT_CODES.iter().find_map(|&(known, kind)| (code == known).then_some(kind)).unwrap_or("ORT_FAIL");
    eprintln!("{PREFIX}{phase} {kind} 0");
}

pub(super) struct WorkerDetail {
    pub phase: &'static str,
    pub kind: &'static str,
    pub code: Option<i32>,
}

pub(super) fn parse_worker_detail(line: &str) -> Option<WorkerDetail> {
    let mut fields = line.strip_prefix(PREFIX)?.split(' ');
    let phase = fields.next()?;
    let kind = fields.next()?;
    let code = fields.next()?.parse::<u16>().ok()?;
    if fields.next().is_some() {
        return None;
    }
    let (phase, kind) = match phase {
        "read-request" | "resolve-runtime" => {
            let phase = if phase == "read-request" { "读取 worker 请求" } else { "解析 ONNX Runtime 路径" };
            let kind = match kind {
                "not-found" => "找不到文件",
                "permission" => "系统拒绝访问",
                "invalid-data" => "文件内容无效",
                "io" => "系统读写失败",
                _ => return None,
            };
            (phase, kind)
        }
        "decode-request" if kind == "invalid-json" && code == 0 => ("解析 worker 请求", "JSON 格式无效"),
        "init-runtime" if kind == "loader-panic" && code == 0 => ("初始化 ONNX Runtime", "加载器崩溃"),
        "init-runtime" | "load-model" if code == 0 => {
            let phase = if phase == "init-runtime" { "初始化 ONNX Runtime" } else { "加载 ONNX 模型" };
            let kind = ORT_CODES.iter().find_map(|&(_, name)| (name == kind).then_some(name))?;
            (phase, kind)
        }
        _ => return None,
    };
    Some(WorkerDetail { phase, kind, code: (code != 0).then_some(i32::from(code)) })
}

pub(super) struct PanicSite {
    pub file: &'static str,
    pub line: u16,
    pub column: u16,
}

pub(super) fn panic_site(line: &str) -> Option<PanicSite> {
    let (header, location) = line.split_once(" panicked at ")?;
    if !header.starts_with("thread '") {
        return None;
    }
    let location = location.strip_suffix(':')?;
    let mut parts = location.rsplitn(3, ':');
    let column = parts.next()?.parse().ok()?;
    let line = parts.next()?.parse().ok()?;
    if !(1..=512).contains(&column) || !(1..=10000).contains(&line) {
        return None;
    }
    let path = parts.next()?.replace('\\', "/");
    // 只匹配已核实的第一方 Rust 源文件名。Cargo 可报告从本包或仓库根开始的相对写法。
    // 第三方、绝对路径、任意未知 src/ 文件以及 panic 正文无法进入日志。
    let files = [
        "src/app_http.rs",
        "src/app_service.rs",
        "src/app_tasks.rs",
        "src/app_sync.rs",
        "src/app_provenance.rs",
        "src/app_research.rs",
        "src/recognition_compute/producer.rs",
        "src/recognition_compute/session.rs",
        "src/index_compute/mod.rs",
        "src/provenance_compute/iqdb.rs",
        "src/provenance_compute/mod.rs",
        "src/research_compute/mod.rs",
    ];
    let relative = path.strip_prefix("native/terms/").unwrap_or(&path);
    let file = files.into_iter().find(|&name| name == relative)?;
    Some(PanicSite { file, line, column })
}
