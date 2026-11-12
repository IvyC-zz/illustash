//! 识别和参考索引生成器共用的窄范围 WD/CCIP 计算。
//! 原始 embedding 保持未归一化；Features 缓存归学习部分所有。
pub mod anchor_snapshot;
pub mod anchors;
pub mod backend;
pub mod claims;
mod directml;
pub mod models;
pub mod preprocess;
pub mod producer;
pub mod session;
mod wd_coreml;

/// 共用的生产适配器门槛：宿主强制的 sidecar 契约必须与安装版 worker 的原生路线一致。
/// Linux 已冻结（见 README）。
pub fn production_adapter_enabled() -> bool {
    cfg!(any(target_os = "macos", target_os = "windows"))
}
