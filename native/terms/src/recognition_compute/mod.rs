//! Narrow WD/CCIP computation used by recognition and the reference-index producer.
//! Raw embeddings remain unnormalised; learning owns its Features cache.
pub mod anchor_snapshot;
pub mod anchors;
pub mod claims;
pub mod models;
pub mod preprocess;
pub mod producer;
pub mod session;

/// Shared production adapter gate: the host's mandatory sidecar contract must
/// match the installed worker's native route. Linux is frozen (README).
pub fn production_adapter_enabled() -> bool {
    cfg!(any(target_os = "macos", target_os = "windows"))
}
