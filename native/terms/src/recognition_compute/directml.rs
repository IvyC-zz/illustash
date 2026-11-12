//! Windows 上识别会话用的 DirectML（#271）。随包的 ONNX Runtime 由 v1.30.0 源码加 `--use_dml` 构建
//! （docs/BACKEND_TASKS.md），从它旁边加载 DirectML.dll。
//!
//! 显卡由 ONNX Runtime 的 DirectML 接口（`SessionOptionsAppendExecutionProvider_DML2`）选：选高性能 GPU，
//! 所以在有核显和独显的笔记本上选独显。旧的 `device_id` 入口按适配器序号选，0 是接显示器的那块，通常是核显。
use ort::{session::builder::SessionBuilder, AsPointer};
use std::ffi::c_void;

/// `OrtDmlDeviceOptions`（dml_provider_factory.h）：先是 `OrtDmlPerformancePreference`，再是 `OrtDmlDeviceFilter`。
#[repr(C)]
struct DeviceOptions {
    preference: u32,
    filter: u32,
}
const HIGH_PERFORMANCE: u32 = 1;
const MINIMUM_POWER: u32 = 2;
const GPU: u32 = 1;
/// `minimum_power` 改选省电的 GPU，在混合模式的笔记本上就是核显：用于在两块都有的机器上测核显。
/// 不设或设成其他值：高性能 GPU。
const PREFERENCE_ENV: &str = "ILLUSTASH_DIRECTML_GPU";
type Status = ort::sys::OrtStatusPtr;
/// `OrtDmlApi` 的开头部分，到 `SessionOptionsAppendExecutionProvider_DML2` 为止（ORT 1.17 起是第六项）。
#[repr(C)]
struct DmlApi {
    _append_by_index: *const c_void,
    _append_with_device: *const c_void,
    _create_allocation: *const c_void,
    _free_allocation: *const c_void,
    _resource_from_allocation: *const c_void,
    append_with_options:
        unsafe extern "system" fn(options: *mut ort::sys::OrtSessionOptions, device: *mut DeviceOptions) -> Status,
}

fn status(status: Status) -> ort::Result<()> {
    // SAFETY: ONNX Runtime API 返回的 status，由绑定负责释放。
    unsafe { ort::error::status_to_result(status) }.map_err(|e| ort::Error::new(plain(&e.to_string())))
}

/// ONNX Runtime 的 provider 错误以源码位置和函数名开头（`<file>.cc:<line> <function> `），
/// 里面带着构建机的文件夹名：设置页只显示其后的信息。
fn plain(message: &str) -> &str {
    let Some(at) = message.find(".cc:").or_else(|| message.find(".h:")) else { return message };
    let rest = &message[at..];
    match rest.find(' ').and_then(|i| rest[i + 1..].find(' ').map(|j| i + 1 + j + 1)) {
        Some(start) if start < rest.len() => &rest[start..],
        _ => message,
    }
}

/// 先设 DirectML 要求的会话选项（ORT 的 DirectML 页面：关闭 memory pattern、按顺序执行），再在高性能 GPU 上
/// 加 provider。这里出错说明没有可用的 DirectX 12 GPU、驱动或 DirectML.dll；调用方回退到 CPU。
pub(super) fn configure(builder: SessionBuilder) -> ort::Result<SessionBuilder> {
    let mut builder = builder.with_memory_pattern(false)?.with_parallel_execution(false)?;
    let mut api: *const c_void = std::ptr::null();
    // SAFETY: GetExecutionProviderApi 写入一个指针，指向已加载运行库里静态的 OrtDmlApi。
    status(unsafe { (ort::api().GetExecutionProviderApi)(c"DML".as_ptr(), ort::sys::ORT_API_VERSION, &mut api) })?;
    if api.is_null() {
        return Err(ort::Error::new("ONNX Runtime 没有 DirectML 接口"));
    }
    let preference = match std::env::var(PREFERENCE_ENV).as_deref() {
        Ok("minimum_power") => MINIMUM_POWER,
        _ => HIGH_PERFORMANCE,
    };
    let mut device = DeviceOptions { preference, filter: GPU };
    // SAFETY: `api` 指向 OrtDmlApi（布局见上）；options 指针是 builder 自己的。
    status(unsafe { ((*api.cast::<DmlApi>()).append_with_options)(builder.ptr_mut(), &mut device) })?;
    Ok(builder)
}

#[cfg(test)]
mod tests {
    #[test]
    fn provider_errors_lose_their_source_location() {
        let error = concat!(
            r"X:\build\onnxruntime\core\providers\dml\dml_provider_factory.cc:363 ",
            "onnxruntime::DMLProviderFactoryCreator::CreateFromDeviceOptions ",
            "No devices detected that match the filter criteria."
        );
        assert_eq!(super::plain(error), "No devices detected that match the filter criteria.");
        assert_eq!(super::plain("Specified provider is not supported."), "Specified provider is not supported.");
    }
}
