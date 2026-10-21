//! DirectML for the recognition sessions on Windows (#271). The bundled ONNX Runtime is built from the v1.30.0
//! source with `--use_dml` (docs/BACKEND_TASKS.md) and loads the DirectML.dll beside it.
//!
//! The adapter is chosen by ONNX Runtime's DirectML API (`SessionOptionsAppendExecutionProvider_DML2`): the
//! high-performance GPU, so on a laptop with an integrated and a discrete GPU the discrete one. The older
//! `device_id` entry point takes the adapter index, where 0 is the display adapter, usually the integrated GPU.
use ort::{session::builder::SessionBuilder, AsPointer};
use std::ffi::c_void;

/// `OrtDmlDeviceOptions` (dml_provider_factory.h): `OrtDmlPerformancePreference`, then `OrtDmlDeviceFilter`.
#[repr(C)]
struct DeviceOptions {
    preference: u32,
    filter: u32,
}
const HIGH_PERFORMANCE: u32 = 1;
const MINIMUM_POWER: u32 = 2;
const GPU: u32 = 1;
/// `minimum_power` picks the power-saving GPU instead, the integrated one on a hybrid laptop: for measuring it
/// on a machine that has both. Unset or anything else: the high-performance GPU.
const PREFERENCE_ENV: &str = "ILLUSTASH_DIRECTML_GPU";
type Status = ort::sys::OrtStatusPtr;
/// The head of `OrtDmlApi`, up to `SessionOptionsAppendExecutionProvider_DML2` (its sixth entry since ORT 1.17).
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
    // SAFETY: a status returned by the ONNX Runtime API, released by the binding.
    unsafe { ort::error::status_to_result(status) }.map_err(|e| ort::Error::new(plain(&e.to_string())))
}

/// ONNX Runtime's provider errors start with the source location and function, `<file>.cc:<line> <function> `,
/// which name the build machine's folders: the settings page shows only the message after them.
fn plain(message: &str) -> &str {
    let Some(at) = message.find(".cc:").or_else(|| message.find(".h:")) else { return message };
    let rest = &message[at..];
    match rest.find(' ').and_then(|i| rest[i + 1..].find(' ').map(|j| i + 1 + j + 1)) {
        Some(start) if start < rest.len() => &rest[start..],
        _ => message,
    }
}

/// Session options DirectML requires (ORT's DirectML page: no memory pattern, sequential execution), then the
/// provider on the high-performance GPU. An error here means no usable DirectX 12 GPU, driver or DirectML.dll;
/// the caller falls back to the CPU.
pub(super) fn configure(builder: SessionBuilder) -> ort::Result<SessionBuilder> {
    let mut builder = builder.with_memory_pattern(false)?.with_parallel_execution(false)?;
    let mut api: *const c_void = std::ptr::null();
    // SAFETY: GetExecutionProviderApi writes a pointer to the static OrtDmlApi of the loaded runtime.
    status(unsafe { (ort::api().GetExecutionProviderApi)(c"DML".as_ptr(), ort::sys::ORT_API_VERSION, &mut api) })?;
    if api.is_null() {
        return Err(ort::Error::new("ONNX Runtime 没有 DirectML 接口"));
    }
    let preference = match std::env::var(PREFERENCE_ENV).as_deref() {
        Ok("minimum_power") => MINIMUM_POWER,
        _ => HIGH_PERFORMANCE,
    };
    let mut device = DeviceOptions { preference, filter: GPU };
    // SAFETY: `api` points at OrtDmlApi (layout above); the options pointer is the builder's own.
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
