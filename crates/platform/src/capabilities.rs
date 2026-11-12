//! 只有目前已实现的桌面能力查询；不表示支持移动端。
pub fn photos() -> bool {
    cfg!(target_os = "macos")
}
