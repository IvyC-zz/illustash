//! Host system description; preserves the about endpoint format.
#[cfg(unix)]
pub fn platform() -> String {
    // SAFETY: utsname 只含 c_char 数组，全零是合法值。
    let mut info: libc::utsname = unsafe { std::mem::zeroed() };
    // SAFETY: `info` 是本地结构，uname 只向其中写入。
    if unsafe { libc::uname(&mut info) } != 0 {
        return std::env::consts::OS.into();
    }
    // SAFETY: uname 成功后各字段都是 NUL 结尾的字符串，`info` 在本函数内存活。
    let text = |v: &[libc::c_char]| unsafe { std::ffi::CStr::from_ptr(v.as_ptr()) }.to_string_lossy().into_owned();
    format!("{} {} ({})", text(&info.sysname), text(&info.release), text(&info.machine))
}
#[cfg(not(unix))]
pub fn platform() -> String {
    format!("{} ({})", std::env::consts::OS, std::env::consts::ARCH)
}
