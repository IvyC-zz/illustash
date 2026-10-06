//! Host system description; preserves the about endpoint format.
#[cfg(unix)]
pub fn platform() -> String {
    let mut info: libc::utsname = unsafe { std::mem::zeroed() };
    if unsafe { libc::uname(&mut info) } != 0 {
        return std::env::consts::OS.into();
    }
    let text = |v: &[libc::c_char]| unsafe { std::ffi::CStr::from_ptr(v.as_ptr()) }.to_string_lossy().into_owned();
    format!("{} {} ({})", text(&info.sysname), text(&info.release), text(&info.machine))
}
#[cfg(not(unix))]
pub fn platform() -> String {
    format!("{} ({})", std::env::consts::OS, std::env::consts::ARCH)
}
