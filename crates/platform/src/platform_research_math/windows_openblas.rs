//! Fixed Windows backend; installed resources and explicit development overrides share it.
//! The two identities are the existing NumPy 2.3.3 / 2.5.3 reference wheels, not
//! interchangeable numerical oracles. Packaging must select its own fixed identity.
use std::{
    ffi::{c_char, c_void, CStr, OsString},
    fs::{File, OpenOptions},
    io::Read,
    os::windows::{
        ffi::{OsStrExt, OsStringExt},
        fs::OpenOptionsExt,
        io::AsRawHandle,
    },
    path::PathBuf,
    sync::OnceLock,
};

type Dot = unsafe extern "C" fn(i64, *const f32, i64, *const f32, i64) -> f32;
type Gemv = unsafe extern "C" fn(i32, i32, i64, i64, f32, *const f32, i64, *const f32, i64, f32, *mut f32, i64);
type Gemm =
    unsafe extern "C" fn(i32, i32, i32, i64, i64, i64, f32, *const f32, i64, *const f32, i64, f32, *mut f32, i64);
type Syrk = unsafe extern "C" fn(i32, i32, i32, i64, i64, f32, *const f32, i64, f32, *mut f32, i64);

type Dot64 = unsafe extern "C" fn(i64, *const f64, i64, *const f64, i64) -> f64;
type Gemv64 = unsafe extern "C" fn(i32, i32, i64, i64, f64, *const f64, i64, *const f64, i64, f64, *mut f64, i64);
type Gemm64 =
    unsafe extern "C" fn(i32, i32, i32, i64, i64, i64, f64, *const f64, i64, *const f64, i64, f64, *mut f64, i64);
type Syrk64 = unsafe extern "C" fn(i32, i32, i32, i64, i64, f64, *const f64, i64, f64, *mut f64, i64);

#[link(name = "kernel32")]
extern "system" {
    fn LoadLibraryExW(name: *const u16, file: *mut c_void, flags: u32) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const c_char) -> *mut c_void;
    fn FreeLibrary(module: *mut c_void) -> i32;
    fn GetModuleFileNameW(module: *mut c_void, name: *mut u16, size: u32) -> u32;
    fn GetFileInformationByHandleEx(file: *mut c_void, class: i32, out: *mut c_void, size: u32) -> i32;
}
struct Library(usize);
impl Drop for Library {
    fn drop(&mut self) {
        // SAFETY: owns a successful LoadLibraryExW reference; no function escapes Backend.
        unsafe {
            FreeLibrary(self.0 as *mut c_void);
        }
    }
}
struct Backend {
    // Held together until process exit; failed initialization drops/unloads all state.
    _library: Library,
    _file: File,
    _module_file: File,
    identity: String,
    dot: Dot,
    gemv: Gemv,
    gemm: Gemm,
    syrk: Syrk,
    dot64: Dot64,
    gemv64: Gemv64,
    gemm64: Gemm64,
    syrk64: Syrk64,
}
static BACKEND: OnceLock<Result<Backend, String>> = OnceLock::new();
fn backend() -> Result<&'static Backend, String> {
    BACKEND.get_or_init(Backend::load).as_ref().map_err(Clone::clone)
}
impl Backend {
    fn load() -> Result<Self, String> {
        let path = match std::env::var_os("ILLUSTASH_RESEARCH_OPENBLAS_DLL") {
            Some(configured) => PathBuf::from(configured),
            None => {
                let root = std::env::var_os("ILLUSTASH_ROOT").map(PathBuf::from).filter(|p| p.is_absolute()).ok_or(
                    "Windows native research requires an absolute ILLUSTASH_ROOT or ILLUSTASH_RESEARCH_OPENBLAS_DLL",
                )?;
                root.join("openblas/openblas.dll")
            }
        };
        if !path.is_absolute()
            || !path.is_file()
            || path.extension().and_then(|x| x.to_str()).is_none_or(|x| !x.eq_ignore_ascii_case("dll"))
        {
            return Err("research OpenBLAS must be an existing absolute DLL path".into());
        }
        let path = path.canonicalize().map_err(|e| format!("research OpenBLAS path: {e}"))?;
        // FILE_SHARE_READ only: do not permit writes/replacement between digest and loading,
        // or for the lifetime of any function pointer. No global search path changes.
        let mut file = OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(&path)
            .map_err(|e| format!("research OpenBLAS read: {e}"))?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).map_err(|e| format!("research OpenBLAS fingerprint: {e}"))?;
        let digest = illustash_core::strict_json::sha256_hex(&bytes);
        let registry: serde_json::Value = serde_json::from_str(include_str!("../research_openblas.json"))
            .map_err(|e| format!("research OpenBLAS identity registry: {e}"))?;
        let version = registry[&digest]["version"]
            .as_str()
            .ok_or("research OpenBLAS DLL fingerprint is not a fixed reference identity")?;
        drop(bytes);
        let name: Vec<_> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        // SAFETY: NUL-terminated absolute canonical path. Dependencies search only the DLL
        // directory and System32 (LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | SYSTEM32), not PATH/cwd.
        let handle = unsafe { LoadLibraryExW(name.as_ptr(), std::ptr::null_mut(), 0x100 | 0x800) };
        if handle.is_null() {
            return Err(format!("research OpenBLAS load: {}", std::io::Error::last_os_error()));
        }
        let library = Library(handle as usize);
        // The Windows loader can redirect an absolute request (.local/SxS). Verify
        // the returned HMODULE BEFORE resolving/calling any OpenBLAS export; never
        // report the requested digest as the identity of a different loaded file.
        // LoadLibraryExW may already have run DllMain (including dependencies): this
        // post-load check cannot prevent/reverse that initialization or its effects.
        let module_file = verify_loaded_module(handle, &path, &file)?;
        let symbol = |name: &'static [u8]| -> Result<*mut c_void, String> {
            // SAFETY: all call sites supply a static NUL-terminated export name; library alive.
            let p = unsafe { GetProcAddress(handle, name.as_ptr().cast()) };
            if p.is_null() {
                Err(format!("research OpenBLAS missing symbol {}", String::from_utf8_lossy(&name[..name.len() - 1])))
            } else {
                Ok(p)
            }
        };
        // Resolve the COMPLETE contract before the first BLAS call. C enums stay i32;
        // signed dimensions and strides are i64 (Windows c_long would be only 32 bits).
        // SAFETY: pinned wheel digests and exact ILP64 exports establish these C signatures.
        let (dot, gemv, gemm, syrk, dot64, gemv64, gemm64, syrk64, config) = unsafe {
            (
                std::mem::transmute::<*mut c_void, Dot>(symbol(b"scipy_cblas_sdot64_\0")?),
                std::mem::transmute::<*mut c_void, Gemv>(symbol(b"scipy_cblas_sgemv64_\0")?),
                std::mem::transmute::<*mut c_void, Gemm>(symbol(b"scipy_cblas_sgemm64_\0")?),
                std::mem::transmute::<*mut c_void, Syrk>(symbol(b"scipy_cblas_ssyrk64_\0")?),
                std::mem::transmute::<*mut c_void, Dot64>(symbol(b"scipy_cblas_ddot64_\0")?),
                std::mem::transmute::<*mut c_void, Gemv64>(symbol(b"scipy_cblas_dgemv64_\0")?),
                std::mem::transmute::<*mut c_void, Gemm64>(symbol(b"scipy_cblas_dgemm64_\0")?),
                std::mem::transmute::<*mut c_void, Syrk64>(symbol(b"scipy_cblas_dsyrk64_\0")?),
                std::mem::transmute::<*mut c_void, unsafe extern "C" fn() -> *const c_char>(symbol(
                    b"scipy_openblas_get_config64_\0",
                )?),
            )
        };
        // SAFETY: fixed export returns a library-owned static NUL-terminated configuration.
        let config = unsafe { config() };
        if config.is_null() {
            return Err("research OpenBLAS empty configuration".into());
        }
        let config =
            unsafe { CStr::from_ptr(config) }.to_str().map_err(|_| "research OpenBLAS invalid configuration")?;
        if !config.starts_with(&format!("OpenBLAS {version} "))
            || !config.split_whitespace().any(|v| v == "USE64BITINT")
        {
            return Err("research OpenBLAS ILP64/version configuration mismatch".into());
        }
        Ok(Self {
            _library: library,
            _file: file,
            _module_file: module_file,
            identity: format!("{digest} {config}"),
            dot,
            gemv,
            gemm,
            syrk,
            dot64,
            gemv64,
            gemm64,
            syrk64,
        })
    }
}
#[repr(C)]
#[derive(Default, PartialEq, Eq)]
struct FileIdentity {
    volume: u64,
    id: [u8; 16],
}
fn file_identity(file: &File) -> Result<FileIdentity, String> {
    let mut identity = FileIdentity::default();
    // SAFETY: a live File HANDLE; FileIdInfo=18 receives the exact FILE_ID_INFO
    // layout (u64 volume serial + FILE_ID_128). No path/name-only identity claim.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            18,
            (&mut identity as *mut FileIdentity).cast(),
            std::mem::size_of::<FileIdentity>() as u32,
        )
    };
    if ok == 0 {
        return Err(format!("research OpenBLAS file identity: {}", std::io::Error::last_os_error()));
    }
    Ok(identity)
}
fn module_path(module: *mut c_void) -> Result<PathBuf, String> {
    let mut path = vec![0u16; 32768];
    // SAFETY: live borrowed HMODULE and writable WCHAR buffer of the passed size.
    let len = unsafe { GetModuleFileNameW(module, path.as_mut_ptr(), path.len() as u32) } as usize;
    if len == 0 || len >= path.len() {
        return Err("research OpenBLAS module path missing or truncated".into());
    }
    PathBuf::from(OsString::from_wide(&path[..len]))
        .canonicalize()
        .map_err(|e| format!("research OpenBLAS module path: {e}"))
}
fn verify_loaded_module(module: *mut c_void, requested: &std::path::Path, locked: &File) -> Result<File, String> {
    let actual_path = module_path(module)?;
    let actual = OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(&actual_path)
        .map_err(|e| format!("research OpenBLAS module file: {e}"))?;
    let same_file = file_identity(locked)? == file_identity(&actual)?;
    if actual_path != requested || !same_file {
        return Err("research OpenBLAS loaded module path/file identity mismatch".into());
    }
    // Keep this second handle too: neither requested nor observed module file may
    // change while the bound functions are retained. No export is called here.
    Ok(actual)
}
pub(super) fn identity() -> Result<String, String> {
    Ok(backend()?.identity.clone())
}
pub(super) fn dot(a: &[f32], b: &[f32]) -> Result<f32, String> {
    let backend = backend()?;
    // SAFETY: parent checks equal nonempty lengths and bounds; unit-stride immutable slices.
    Ok(unsafe { (backend.dot)(a.len() as i64, a.as_ptr(), 1, b.as_ptr(), 1) })
}
pub(super) fn cross(a: &[f32], m: usize, b: &[f32], p: usize, dim: usize, out: &mut [f32]) -> Result<(), String> {
    let backend = backend()?;
    // SAFETY: parent validates every slice length, allocation and dimension before entry.
    // Identical NumPy matrix-shape dispatch to the existing Mac backend: no dot substitution.
    unsafe {
        if m == 1 && p == 1 {
            out[0] = (backend.dot)(dim as i64, a.as_ptr(), 1, b.as_ptr(), 1);
        } else if dim == 1 {
            for i in 0..m {
                for j in 0..p {
                    out[i * p + j] = a[i] * b[j];
                }
            }
        } else if p == 1 {
            (backend.gemv)(
                102,
                112,
                dim as i64,
                m as i64,
                1.0,
                a.as_ptr(),
                dim as i64,
                b.as_ptr(),
                1,
                0.0,
                out.as_mut_ptr(),
                1,
            );
        } else if m == 1 {
            (backend.gemv)(
                102,
                112,
                dim as i64,
                p as i64,
                1.0,
                b.as_ptr(),
                dim as i64,
                a.as_ptr(),
                1,
                0.0,
                out.as_mut_ptr(),
                1,
            );
        } else if m == p && a.as_ptr() == b.as_ptr() {
            (backend.syrk)(
                101,
                121,
                111,
                m as i64,
                dim as i64,
                1.0,
                a.as_ptr(),
                dim as i64,
                0.0,
                out.as_mut_ptr(),
                p as i64,
            );
            for i in 0..m {
                for j in i + 1..p {
                    out[j * p + i] = out[i * p + j];
                }
            }
        } else {
            (backend.gemm)(
                101,
                111,
                112,
                m as i64,
                p as i64,
                dim as i64,
                1.0,
                a.as_ptr(),
                dim as i64,
                b.as_ptr(),
                dim as i64,
                0.0,
                out.as_mut_ptr(),
                p as i64,
            );
        }
    }
    Ok(())
}

pub(super) fn cross64(a: &[f64], m: usize, b: &[f64], p: usize, dim: usize, out: &mut [f64]) -> Result<(), String> {
    let backend = backend()?;
    // SAFETY: parent validates every slice length, allocation and dimension before entry.
    // Identical NumPy matrix-shape dispatch to the existing float32 backend: no dot substitution.
    unsafe {
        if m == 1 && p == 1 {
            out[0] = (backend.dot64)(dim as i64, a.as_ptr(), 1, b.as_ptr(), 1);
        } else if dim == 1 {
            for i in 0..m {
                for j in 0..p {
                    out[i * p + j] = a[i] * b[j];
                }
            }
        } else if p == 1 {
            (backend.gemv64)(
                102,
                112,
                dim as i64,
                m as i64,
                1.0,
                a.as_ptr(),
                dim as i64,
                b.as_ptr(),
                1,
                0.0,
                out.as_mut_ptr(),
                1,
            );
        } else if m == 1 {
            (backend.gemv64)(
                102,
                112,
                dim as i64,
                p as i64,
                1.0,
                b.as_ptr(),
                dim as i64,
                a.as_ptr(),
                1,
                0.0,
                out.as_mut_ptr(),
                1,
            );
        } else if m == p && a.as_ptr() == b.as_ptr() {
            (backend.syrk64)(
                101,
                121,
                111,
                m as i64,
                dim as i64,
                1.0,
                a.as_ptr(),
                dim as i64,
                0.0,
                out.as_mut_ptr(),
                p as i64,
            );
            for i in 0..m {
                for j in i + 1..p {
                    out[j * p + i] = out[i * p + j];
                }
            }
        } else {
            (backend.gemm64)(
                101,
                111,
                112,
                m as i64,
                p as i64,
                dim as i64,
                1.0,
                a.as_ptr(),
                dim as i64,
                b.as_ptr(),
                dim as i64,
                0.0,
                out.as_mut_ptr(),
                p as i64,
            );
        }
    }
    Ok(())
}

// Index NPY arithmetic retains its original column/row layout. Converting F
// storage to C and using another GEMV shape can change a threshold's last bit.
pub(super) fn index_product32(
    rows: &[f32],
    n: usize,
    dim: usize,
    anchor: &[f32],
    out: &mut [f32],
) -> Result<(), String> {
    let backend = backend()?;
    // SAFETY: parent validated nonempty lengths/bounds; these are F-order rows.
    unsafe {
        if n == 1 {
            out[0] = (backend.dot)(dim as i64, rows.as_ptr(), 1, anchor.as_ptr(), 1);
        } else {
            (backend.gemv)(
                102,
                111,
                n as i64,
                dim as i64,
                1.,
                rows.as_ptr(),
                n as i64,
                anchor.as_ptr(),
                1,
                0.,
                out.as_mut_ptr(),
                1,
            );
        }
    }
    Ok(())
}
pub(super) fn index_product64(
    rows: &[f64],
    n: usize,
    dim: usize,
    fortran: bool,
    anchor: &[f64],
    out: &mut [f64],
) -> Result<(), String> {
    let backend = backend()?;
    // SAFETY: parent validated nonempty lengths/bounds and the supplied NPY layout.
    unsafe {
        if n == 1 {
            out[0] = (backend.dot64)(dim as i64, rows.as_ptr(), 1, anchor.as_ptr(), 1);
        } else {
            let (trans, m, k, lda) = if fortran { (111, n, dim, n) } else { (112, dim, n, dim) };
            (backend.gemv64)(
                102,
                trans,
                m as i64,
                k as i64,
                1.,
                rows.as_ptr(),
                lda as i64,
                anchor.as_ptr(),
                1,
                0.,
                out.as_mut_ptr(),
                1,
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[link(name = "kernel32")]
    extern "system" {
        fn GetModuleHandleW(name: *const u16) -> *mut c_void;
    }
    fn known_module() -> *mut c_void {
        let name: Vec<_> = std::ffi::OsStr::new("kernel32.dll").encode_wide().chain(Some(0)).collect();
        // SAFETY: retrieves an already loaded OS module, never loads a test DLL or runs DllMain.
        let module = unsafe { GetModuleHandleW(name.as_ptr()) };
        assert!(!module.is_null());
        module
    }
    #[test]
    fn loaded_module_identity_accepts_the_same_locked_file() {
        let module = known_module();
        let path = module_path(module).unwrap();
        let file = OpenOptions::new().read(true).share_mode(1).open(&path).unwrap();
        assert!(verify_loaded_module(module, &path, &file).is_ok());
    }
    #[test]
    fn different_module_cannot_inherit_requested_file_identity() {
        let module = known_module();
        let actual_path = module_path(module).unwrap();
        let root = std::env::temp_dir().join(format!(
            "illustash-module-identity-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("owned-synthetic-file.dll");
        std::fs::write(&path, b"identity fixture, never loaded").unwrap();
        let path = path.canonicalize().unwrap();
        let file = OpenOptions::new().read(true).share_mode(1).open(&path).unwrap();
        // Wrong module/path and wrong file must fail. Then use the actual path but
        // the wrong locked HANDLE: canonical spelling alone cannot pass the check.
        for expected_path in [&path, &actual_path] {
            let error = verify_loaded_module(module, expected_path, &file).expect_err("different file rejected");
            assert!(error.contains("path/file identity mismatch"), "{error}");
        }
        drop(file);
        std::fs::remove_dir_all(root).unwrap();
    }
}
