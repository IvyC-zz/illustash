//! 固定的 Windows 后端；安装后的资源和开发时显式指定的覆盖共用它。
//! 两个身份分别是现有的 NumPy 2.3.3 / 2.5.3 参考 wheel，不是可以互相替代的数值参照。
//! 打包时必须选定自己的固定身份。
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
        // SAFETY: 持有一次成功的 LoadLibraryExW 引用；没有函数指针流出 Backend。
        unsafe {
            FreeLibrary(self.0 as *mut c_void);
        }
    }
}
struct Backend {
    // 这些字段一起保留到进程退出；初始化失败时释放并卸载全部状态。
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
        // 只给 FILE_SHARE_READ：从计算摘要到加载之间，以及任何函数指针存活期间，都不允许写入或替换文件。
        // 不改全局搜索路径。
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
        // SAFETY: NUL 结尾的规范化绝对路径。依赖只在 DLL 所在目录和 System32 中搜索
        // （LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | SYSTEM32），不搜索 PATH 和当前目录。
        let handle = unsafe { LoadLibraryExW(name.as_ptr(), std::ptr::null_mut(), 0x100 | 0x800) };
        if handle.is_null() {
            return Err(format!("research OpenBLAS load: {}", std::io::Error::last_os_error()));
        }
        let library = Library(handle as usize);
        // Windows 加载器可能重定向绝对路径的请求（.local/SxS）。在解析或调用任何 OpenBLAS 导出函数**之前**
        // 先核验返回的 HMODULE；绝不把请求文件的摘要报成另一个实际加载文件的身份。
        // LoadLibraryExW 可能已经运行过 DllMain（包括依赖的）：加载后的这项检查无法阻止或撤回那次初始化及其影响。
        let module_file = verify_loaded_module(handle, &path, &file)?;
        let symbol = |name: &'static [u8]| -> Result<*mut c_void, String> {
            // SAFETY: 所有调用处都传入静态的 NUL 结尾导出名；库仍处于加载状态。
            let p = unsafe { GetProcAddress(handle, name.as_ptr().cast()) };
            if p.is_null() {
                Err(format!("research OpenBLAS missing symbol {}", String::from_utf8_lossy(&name[..name.len() - 1])))
            } else {
                Ok(p)
            }
        };
        // 第一次调用 BLAS 之前先解析**完整的**契约。C 枚举仍是 i32；
        // 有符号的维度和步长是 i64（Windows 的 c_long 只有 32 位）。
        // SAFETY: 固定的 wheel 摘要和确切的 ILP64 导出名确定了这些 C 签名。
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
        // SAFETY: 这个固定的导出函数返回库内持有的静态 NUL 结尾配置字符串。
        let config = unsafe { config() };
        if config.is_null() {
            return Err("research OpenBLAS empty configuration".into());
        }
        let config =
            // SAFETY: 上面已确认指针非空；OpenBLAS 返回库内静态的 NUL 结尾字符串，库在 `Backend` 存活期间不卸载。
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
    // SAFETY: 存活的 File 句柄；FileIdInfo=18 写入的正是 FILE_ID_INFO 的布局
    // （u64 卷序列号 + FILE_ID_128）。不只凭路径或名称断言身份。
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
    // SAFETY: 借用的 HMODULE 仍然有效，WCHAR 缓冲区可写，大小与传入的一致。
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
    // 这第二个句柄也保留：绑定的函数保留期间，请求的和实际观察到的模块文件都不能被改动。
    // 这里不调用任何导出函数。
    Ok(actual)
}
pub(super) fn identity() -> Result<String, String> {
    Ok(backend()?.identity.clone())
}
pub(super) fn dot(a: &[f32], b: &[f32]) -> Result<f32, String> {
    let backend = backend()?;
    // SAFETY: 上层已检查长度相等、非空且不越界；切片不可变，步长为 1。
    Ok(unsafe { (backend.dot)(a.len() as i64, a.as_ptr(), 1, b.as_ptr(), 1) })
}
pub(super) fn cross(a: &[f32], m: usize, b: &[f32], p: usize, dim: usize, out: &mut [f32]) -> Result<(), String> {
    let backend = backend()?;
    // SAFETY: 进入前上层已校验每个切片的长度、分配和维度。
    // 与现有 Mac 后端相同的 NumPy 矩阵形状分派：不用点积替代。
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
    // SAFETY: 进入前上层已校验每个切片的长度、分配和维度。
    // 与现有 float32 后端相同的 NumPy 矩阵形状分派：不用点积替代。
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

// 索引 NPY 的运算保留原有的列主序或行主序布局。把 F 顺序存储转成 C 顺序、换用另一种 GEMV 形状，
// 可能改变阈值的最后一位。
pub(super) fn index_product32(
    rows: &[f32],
    n: usize,
    dim: usize,
    anchor: &[f32],
    out: &mut [f32],
) -> Result<(), String> {
    let backend = backend()?;
    // SAFETY: 上层已校验长度非空且不越界；这些是 F 顺序的行。
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
    // SAFETY: 上层已校验长度非空且不越界，也校验了传入的 NPY 布局。
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
        // SAFETY: 只取已加载的系统模块，从不加载测试 DLL，也不运行 DllMain。
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
        // 模块或路径不对、文件不对都必须失败。再用实际路径配上锁定的错误句柄：
        // 光是规范化后的路径写法一致不能通过检查。
        for expected_path in [&path, &actual_path] {
            let error = verify_loaded_module(module, expected_path, &file).expect_err("different file rejected");
            assert!(error.contains("path/file identity mismatch"), "{error}");
        }
        drop(file);
        std::fs::remove_dir_all(root).unwrap();
    }
}
