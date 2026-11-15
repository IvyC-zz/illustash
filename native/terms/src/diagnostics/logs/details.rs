//! 自有进程直接记录固定阶段与类型，原有 stderr 仍供自检界面使用。
use super::{record, Kind, WORKER_SOURCE};
use std::{io, panic::Location};

fn worker_detail(phase: &'static str, kind: &'static str, code: Option<i32>) {
    if let Some(&source) = WORKER_SOURCE.get() {
        record(source, phase, code, Kind::WorkerDetail(kind));
    }
}

pub enum IoPhase {
    ReadRequest,
    ResolveRuntime,
}

pub fn worker_io(phase: IoPhase, error: &io::Error) {
    let phase = match phase {
        IoPhase::ReadRequest => "读取 worker 请求",
        IoPhase::ResolveRuntime => "解析 ONNX Runtime 路径",
    };
    let kind = match error.kind() {
        io::ErrorKind::NotFound => "找不到文件",
        io::ErrorKind::PermissionDenied => "系统拒绝访问",
        io::ErrorKind::InvalidData => "文件内容无效",
        _ => "系统读写失败",
    };
    worker_detail(phase, kind, error.raw_os_error());
}

pub fn worker_invalid_request() {
    worker_detail("解析 worker 请求", "JSON 格式无效", None);
}

pub(crate) fn worker_invalid_fields() {
    worker_detail("校验 worker 请求字段", "请求字段或类型无效", None);
}

pub fn worker_runtime_abi() {
    worker_detail("初始化 ONNX Runtime", "加载器崩溃", None);
}

pub enum OrtPhase {
    Initialize,
    LoadModel,
}

const ORT_CODES: &[&str] = &[
    "ORT_INVALID_ARGUMENT",
    "ORT_NO_SUCHFILE",
    "ORT_NO_MODEL",
    "ORT_ENGINE_ERROR",
    "ORT_RUNTIME_EXCEPTION",
    "ORT_INVALID_PROTOBUF",
    "ORT_MODEL_LOADED",
    "ORT_NOT_IMPLEMENTED",
    "ORT_INVALID_GRAPH",
    "ORT_EP_FAIL",
    "ORT_FAIL",
];

pub(crate) fn worker_ort(phase: OrtPhase, code: &'static str) {
    let phase = match phase {
        OrtPhase::Initialize => "初始化 ONNX Runtime",
        OrtPhase::LoadModel => "加载 ONNX 模型",
    };
    let kind = if ORT_CODES.contains(&code) { code } else { "ORT_FAIL" };
    worker_detail(phase, kind, None);
}

pub(super) struct PanicSite {
    pub file: &'static str,
    pub line: u32,
    pub column: u32,
}

pub(super) fn panic_site(location: &Location<'_>) -> Option<PanicSite> {
    let path = location.file().replace('\\', "/");
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
    Some(PanicSite { file, line: location.line(), column: location.column() })
}
