//! Only currently implemented desktop capability queries; no mobile support claim.
pub fn photos() -> bool {
    cfg!(target_os = "macos")
}
