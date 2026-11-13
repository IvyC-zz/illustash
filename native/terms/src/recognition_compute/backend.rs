//! 识别会话的执行后端（#270：macOS 上用 CoreML；#271：Windows 上用 DirectML）。
//! 以 CPU 为准。加速会话建不起来或运行中失败时，这个 worker 余下的部分都回退到 CPU，原因记在
//! Shared/acceleration.json。加速输出与 CPU 输出只差 fp32 舍入（docs/TERMS_PERFORMANCE.md 第 6 节）。
use crate::{Error, Result};
use ort::{
    execution_providers::{
        coreml::{CoreMLComputeUnits, CoreMLModelFormat},
        CoreMLExecutionProvider,
    },
    session::builder::SessionBuilder,
};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

/// `ILLUSTASH_CPU_ONLY=1` 让所有会话都留在 CPU，不管设置怎么写：逐位一致的验收运行和要排除加速器的人设置它。
/// 空值和 `0` 视同未设置。
pub const CPU_ONLY_ENV: &str = "ILLUSTASH_CPU_ONLY";
/// 这个构建有没有加速后端；只在有的地方提供这项设置。
pub const AVAILABLE: bool =
    cfg!(any(all(target_os = "macos", target_arch = "aarch64"), all(windows, target_arch = "x86_64")));
/// 编译好的 CoreML 模型，在共享文件夹下按模型分别存放，每个键（cache_key）一个文件夹。
const COREML_CACHE: &str = "coreml-cache";
/// 上一个 worker 按模型分别用了什么、为什么回退：供 GET /api/models 读取。
pub const STATUS_FILE: &str = "acceleration.json";
/// 放在 ONNX Runtime 库旁边：这个运行库构建打算让每个模型跑在哪个后端上，例如
/// `{"format": 1, "wd": "directml", "ccip": "directml"}`（#271）。运行库和它的后端是一个配置单元：
/// 替换运行库文件夹（比如换成 Windows ML 的）就换了后端，不用动这里的代码。
/// 没有这个文件时使用 `Acceleration::backend` 里的平台默认值。
pub const RUNTIME_BACKENDS: &str = "backends.json";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Model {
    Wd,
    Ccip,
}
impl Model {
    pub fn name(self) -> &'static str {
        match self {
            Self::Wd => "wd",
            Self::Ccip => "ccip",
        }
    }
    /// 动态 batch 维；对需要静态形状的加速器固定为 1。
    fn batch(self) -> &'static str {
        match self {
            Self::Wd => "batch_size",
            Self::Ccip => "batch",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Backend {
    Cpu,
    CoreMl,
    DirectMl,
}
impl Backend {
    pub fn name(self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::CoreMl => "coreml",
            Self::DirectMl => "directml",
        }
    }
    fn named(name: &str) -> Option<Self> {
        [Self::Cpu, Self::CoreMl, Self::DirectMl].into_iter().find(|b| b.name() == name)
    }
}

/// 运行库的 RUNTIME_BACKENDS：没有文件时为 `Ok(None)`；读不了或有未知条目时报错，
/// 此时所有模型都留在 CPU，不去猜。
fn declared(runtime: &Path) -> std::result::Result<Option<[Backend; 2]>, String> {
    let Some(file) = runtime.parent().map(|d| d.join(RUNTIME_BACKENDS)).filter(|f| f.exists()) else {
        return Ok(None);
    };
    let fail = |e: &dyn std::fmt::Display| format!("{}: {e}", file.display());
    let value: Value = serde_json::from_slice(&fs::read(&file).map_err(|e| fail(&e))?).map_err(|e| fail(&e))?;
    if value.get("format").and_then(Value::as_u64) != Some(1) {
        return Err(fail(&"format 应为 1"));
    }
    let backend = |model: Model| {
        value
            .get(model.name())
            .and_then(Value::as_str)
            .and_then(Backend::named)
            .ok_or_else(|| fail(&format!("{} 应为 cpu、coreml 或 directml", model.name())))
    };
    Ok(Some([backend(Model::Wd)?, backend(Model::Ccip)?]))
}
/// 本进程绑定的运行库（session::initialise）的 RUNTIME_BACKENDS，绑定后读一次；
/// 绑定之前（还没有运行库）使用平台默认值，不缓存。
fn runtime_backends() -> std::result::Result<Option<[Backend; 2]>, String> {
    static DECLARED: std::sync::OnceLock<std::result::Result<Option<[Backend; 2]>, String>> =
        std::sync::OnceLock::new();
    let runtime = super::session::runtime();
    if runtime.as_os_str().is_empty() {
        return Ok(None);
    }
    DECLARED.get_or_init(|| declared(&runtime)).clone()
}

/// 一个 worker 的选择：开关（设置和 CPU_ONLY_ENV）以及共享文件夹的位置。
#[derive(Clone, Debug)]
pub struct Acceleration {
    enabled: bool,
    shared: Option<PathBuf>,
}
impl Acceleration {
    pub fn cpu() -> Self {
        Self { enabled: false, shared: None }
    }
    /// 用于 worker：默认开启，除非设置了 CPU_ONLY_ENV，或 preferences.json 里关闭了「使用 GPU 加速识别」。
    pub fn from_env(shared: Option<&Path>) -> Self {
        let forced = std::env::var_os(CPU_ONLY_ENV).is_some_and(|v| !v.is_empty() && v != "0");
        Self { enabled: AVAILABLE && !forced && crate::app_preferences::acceleration(), shared: shared.map(Into::into) }
    }
    /// 用绑定的运行库声明的后端（RUNTIME_BACKENDS），否则用平台默认值（#213）：macOS 上只有 CCIP 用 CoreML
    /// （WD 用 CoreML 并不更快，编译约要十分钟，缓存占 2.1 GB）；Windows 上两个都用 DirectML（#271）。
    pub fn backend(&self, model: Model) -> Backend {
        if !self.enabled {
            return Backend::Cpu;
        }
        let default = match model {
            Model::Ccip if cfg!(target_os = "macos") => Backend::CoreMl,
            _ if cfg!(windows) => Backend::DirectMl,
            _ => Backend::Cpu,
        };
        match runtime_backends() {
            Ok(Some([wd, ccip])) => {
                if model == Model::Wd {
                    wd
                } else {
                    ccip
                }
            }
            Ok(None) => default,
            // 声明读不了：用 CPU 而不是猜，并在设置页显示的位置写明原因。
            Err(reason) => {
                eprintln!("识别加速：{} 改用 CPU：{reason}", model.name());
                self.record(model, default, Backend::Cpu, Some(&reason));
                Backend::Cpu
            }
        }
    }
    /// 会话里属于加速器的部分：provider、固定的形状、缓存。CPU 会话从不走到这里。
    pub(super) fn configure(
        &self,
        builder: SessionBuilder,
        backend: Backend,
        model: Model,
        path: &Path,
        runtime: &Path,
    ) -> Result<SessionBuilder> {
        let failed = |e: ort::Error| Error::invalid(format!("{} {}: {e}", backend.name(), model.name()));
        match backend {
            Backend::CoreMl => {
                // 不固定 batch 时，CoreML 只接管 CCIP 1486 个节点中的 27 个；固定后整张图是一个分区。
                let builder = builder.with_dimension_override(model.batch(), 1).map_err(failed)?;
                let mut provider = CoreMLExecutionProvider::default()
                    .with_model_format(CoreMLModelFormat::MLProgram)
                    .with_compute_units(CoreMLComputeUnits::All)
                    .with_static_input_shapes(true);
                if let Some(cache) = self.coreml_cache(model, path, runtime) {
                    provider = provider.with_model_cache_dir(cache.display());
                }
                builder.with_execution_providers([provider.build().error_on_failure()]).map_err(failed)
            }
            // batch 固定后，DirectML 能在建会话时规划整张图。
            Backend::DirectMl if cfg!(windows) => {
                builder.with_dimension_override(model.batch(), 1).and_then(super::directml::configure).map_err(failed)
            }
            Backend::DirectMl | Backend::Cpu => Err(Error::invalid(format!("{} 不在本平台", backend.name()))),
        }
    }
    /// Shared/coreml-cache/<model>/<key>。只淘汰本模型的旧键；升级前单层布局的缓存重新编译一次。
    fn coreml_cache(&self, model: Model, path: &Path, runtime: &Path) -> Option<PathBuf> {
        let root = self.shared.as_ref()?.join(COREML_CACHE);
        let key = cache_key(path, runtime).inspect_err(|e| eprintln!("识别加速：缓存键 {e}")).ok()?;
        let owned = root.join(model.name());
        fs::create_dir_all(&owned).inspect_err(|e| eprintln!("识别加速：缓存目录 {e}")).ok()?;
        prune_cache_keys(&root, None);
        prune_cache_keys(&owned, Some(&key));
        Some(owned.join(key))
    }
    /// 加速器建不起来或运行中失败：删掉它的缓存（坏的可能就是缓存），并记下原因。
    pub(super) fn fell_back(&self, model: Model, backend: Backend, path: &Path, runtime: &Path, reason: &str) {
        eprintln!("识别加速：{} 改用 CPU（{}）：{reason}", model.name(), backend.name());
        if backend == Backend::CoreMl {
            let _lock = self.compile_lock(backend);
            if let Some(cache) = self.coreml_cache(model, path, runtime) {
                let _ = fs::remove_dir_all(cache);
            }
        }
        self.record(model, backend, Backend::Cpu, Some(reason));
    }
    /// 什么都没能加载时移除本模型的空目录。锁文件保留，避免等待者与后来的进程锁住不同文件。
    pub(super) fn abandon(&self, model: Model, backend: Backend) {
        let Some(root) = self
            .shared
            .as_ref()
            .map(|s| s.join(COREML_CACHE).join(model.name()))
            .filter(|_| backend == Backend::CoreMl)
        else {
            return;
        };
        let _lock = self.compile_lock(backend);
        for entry in fs::read_dir(&root).into_iter().flatten().flatten() {
            if entry.path().is_dir() && fs::read_dir(entry.path()).is_ok_and(|mut d| d.next().is_none()) {
                let _ = fs::remove_dir(entry.path());
            }
        }
        let _ = fs::remove_dir(&root);
    }
    pub(super) fn record(&self, model: Model, requested: Backend, used: Backend, reason: Option<&str>) {
        let Some(shared) = &self.shared else { return };
        let file = shared.join(STATUS_FILE);
        let mut fields = read_status(&file);
        let at = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        fields.insert(
            model.name().into(),
            json!({"requested": requested.name(), "used": used.name(), "reason": reason, "at": at}),
        );
        let temporary = file.with_extension(format!("{}.tmp", std::process::id()));
        if fs::write(&temporary, Value::Object(fields).to_string()).is_ok() && fs::rename(&temporary, &file).is_err() {
            let _ = fs::remove_file(&temporary);
        }
    }
    /// 让 worker 之间的首次编译串行（识别和索引可能同时启动）：一个编译进缓存，另一个随后加载它。
    /// 最多等一分钟，之后不拿锁直接继续。
    pub(super) fn compile_lock(&self, backend: Backend) -> Option<fs::File> {
        if backend != Backend::CoreMl {
            return None;
        }
        let root = self.shared.as_ref()?.join(COREML_CACHE);
        fs::create_dir_all(&root).ok()?;
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(root.join("compile.lock"))
            .ok()?;
        let started = Instant::now();
        while crate::app_file_lock::acquire(&file).is_err() {
            if started.elapsed() > Duration::from_secs(60) {
                return None;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Some(file)
    }
}

/// 只删除本程序生成的 32 位十六进制缓存键目录；模型目录和编译锁不属于缓存键。
fn prune_cache_keys(root: &Path, current: Option<&str>) {
    for entry in fs::read_dir(root).into_iter().flatten().flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.len() == 32
            && name.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
            && Some(name.as_ref()) != current
            && entry.file_type().is_ok_and(|t| t.is_dir())
        {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

/// GET /api/models：开关的状态、这个构建会用的加速器，以及上一个 worker 用了什么。
pub fn info(shared: Option<&Path>) -> Value {
    let last = shared.map(|s| read_status(&s.join(STATUS_FILE))).unwrap_or_default();
    let backend = if !AVAILABLE {
        None
    } else if cfg!(target_os = "macos") {
        Some(Backend::CoreMl.name())
    } else {
        Some(Backend::DirectMl.name())
    };
    json!({"available": AVAILABLE, "backend": backend, "on": crate::app_preferences::acceleration(), "last": last})
}

fn read_status(file: &Path) -> Map<String, Value> {
    fs::read(file)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .and_then(|value| if let Value::Object(fields) = value { Some(fields) } else { None })
        .unwrap_or_default()
}

/// 模型、运行库和系统版本的 SHA-256：编译好的模型恰好只对这三者有效。
fn cache_key(model: &Path, runtime: &Path) -> Result<String> {
    let mut digest = Sha256::new();
    digest.update(b"illustash-coreml/1 mlprogram all batch=1\n");
    for path in [model, runtime, Path::new("/System/Library/CoreServices/SystemVersion.plist")] {
        let mut file = fs::File::open(path).map_err(|e| Error::invalid(format!("{}: {e}", path.display())))?;
        let mut buffer = vec![0; 1 << 20];
        loop {
            let n = file.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            digest.update(&buffer[..n]);
        }
        digest.update(b"\n");
    }
    Ok(format!("{:x}", digest.finalize())[..32].to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn temp(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("illustash-backend-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }
    #[test]
    fn off_means_cpu_for_every_model() {
        let off = Acceleration::cpu();
        assert_eq!((off.backend(Model::Wd), off.backend(Model::Ccip)), (Backend::Cpu, Backend::Cpu));
        let on = Acceleration { enabled: true, shared: None };
        let (wd, ccip) = if cfg!(target_os = "macos") {
            (Backend::Cpu, Backend::CoreMl)
        } else if cfg!(windows) {
            (Backend::DirectMl, Backend::DirectMl)
        } else {
            (Backend::Cpu, Backend::Cpu)
        };
        assert_eq!((on.backend(Model::Wd), on.backend(Model::Ccip)), (wd, ccip));
    }
    #[test]
    fn the_runtime_declares_its_backends() {
        let root = temp("declared");
        let runtime = root.join("onnxruntime.dll");
        assert_eq!(declared(&runtime), Ok(None), "no file: the platform default");
        let file = root.join(RUNTIME_BACKENDS);
        fs::write(&file, r#"{"format": 1, "wd": "cpu", "ccip": "directml"}"#).unwrap();
        assert_eq!(declared(&runtime), Ok(Some([Backend::Cpu, Backend::DirectMl])));
        for bad in [
            r#"{"format": 2, "wd": "cpu", "ccip": "cpu"}"#,
            r#"{"format": 1, "wd": "cuda", "ccip": "cpu"}"#,
            r#"{"format": 1, "wd": "cpu"}"#,
            "not json",
        ] {
            fs::write(&file, bad).unwrap();
            assert!(declared(&runtime).is_err(), "{bad}");
        }
        fs::remove_dir_all(root).unwrap();
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn cache_is_keyed_by_content_and_stale_keys_are_removed() {
        let root = temp("cache");
        let (model, runtime) = (root.join("model.onnx"), root.join("runtime.dylib"));
        fs::write(&model, b"model one").unwrap();
        fs::write(&runtime, b"runtime").unwrap();
        let shared = root.join("shared");
        let acceleration = Acceleration { enabled: true, shared: Some(shared.clone()) };
        let first = acceleration.coreml_cache(Model::Ccip, &model, &runtime).unwrap();
        assert_eq!(first.parent().unwrap(), shared.join(COREML_CACHE).join("ccip"));
        fs::create_dir_all(first.join("compiled")).unwrap();
        assert_eq!(acceleration.coreml_cache(Model::Ccip, &model, &runtime).unwrap(), first, "相同内容复用缓存");
        assert!(first.join("compiled").exists());
        let lock = acceleration.compile_lock(Backend::CoreMl).unwrap();
        fs::write(&model, b"model two").unwrap();
        let second = acceleration.coreml_cache(Model::Ccip, &model, &runtime).unwrap();
        assert_ne!(second, first);
        assert!(!first.exists(), "the old model's compiled copy is removed");
        assert!(shared.join(COREML_CACHE).join("compile.lock").exists(), "the lock file stays");
        drop(lock);
        fs::remove_dir_all(root).unwrap();
    }
    #[cfg(target_os = "macos")]
    fn cache_fixture(name: &str) -> (PathBuf, Acceleration, [PathBuf; 2], PathBuf) {
        let root = temp(name);
        let models = [root.join("wd.onnx"), root.join("ccip.onnx")];
        fs::write(&models[0], b"wd").unwrap();
        fs::write(&models[1], b"ccip").unwrap();
        let runtime = root.join("runtime.dylib");
        fs::write(&runtime, b"runtime").unwrap();
        let acceleration = Acceleration { enabled: true, shared: Some(root.join("shared")) };
        (root, acceleration, models, runtime)
    }
    #[cfg(target_os = "macos")]
    fn fill_cache(path: &Path) {
        fs::create_dir_all(path).unwrap();
        fs::write(path.join("compiled"), b"compiled model").unwrap();
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn models_keep_independent_caches_across_model_and_runtime_changes() {
        let (root, acceleration, models, runtime) = cache_fixture("cache-owners");
        let cache_root = root.join("shared").join(COREML_CACHE);
        let legacy = cache_root.join("0".repeat(32));
        fill_cache(&legacy);
        let kinds = [Model::Wd, Model::Ccip];
        let mut caches = kinds.map(|kind| {
            let model = &models[usize::from(kind == Model::Ccip)];
            let cache = acceleration.coreml_cache(kind, model, &runtime).unwrap();
            fill_cache(&cache);
            cache
        });
        assert!(!legacy.exists(), "升级时淘汰旧的单层缓存");
        for i in 0..2 {
            assert_eq!(acceleration.coreml_cache(kinds[i], &models[i], &runtime).unwrap(), caches[i]);
            assert_eq!(fs::read(caches[i].join("compiled")).unwrap(), b"compiled model");
            let previous = caches[i].clone();
            fs::write(&models[i], format!("{} updated", kinds[i].name())).unwrap();
            caches[i] = acceleration.coreml_cache(kinds[i], &models[i], &runtime).unwrap();
            assert_ne!(caches[i], previous);
            assert!(!previous.exists(), "只淘汰变更模型的旧缓存");
            assert!(caches[1 - i].join("compiled").exists(), "另一模型仍可命中缓存");
            fill_cache(&caches[i]);
        }
        fs::write(&runtime, b"runtime updated").unwrap();
        for i in 0..2 {
            let previous = caches[i].clone();
            caches[i] = acceleration.coreml_cache(kinds[i], &models[i], &runtime).unwrap();
            assert_ne!(caches[i], previous);
            assert!(!previous.exists());
            assert!(caches[1 - i].join("compiled").exists());
            fill_cache(&caches[i]);
        }
        fs::remove_dir_all(root).unwrap();
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn fallback_only_removes_the_failed_models_cache() {
        let (root, acceleration, models, runtime) = cache_fixture("cache-fallback");
        let kinds = [Model::Wd, Model::Ccip];
        let caches = kinds.map(|kind| {
            let cache = acceleration.coreml_cache(kind, &models[usize::from(kind == Model::Ccip)], &runtime).unwrap();
            fill_cache(&cache);
            cache
        });
        for i in 0..2 {
            acceleration.fell_back(kinds[i], Backend::CoreMl, &models[i], &runtime, "损坏的编译缓存");
            assert!(!caches[i].exists());
            assert!(caches[1 - i].join("compiled").exists(), "回退不删除另一模型的缓存");
            let status = read_status(&root.join("shared").join(STATUS_FILE));
            assert_eq!(status[kinds[i].name()]["used"], "cpu");
            assert_eq!(status[kinds[i].name()]["reason"], "损坏的编译缓存");
            assert!(models[i].exists(), "原始模型不能被清理");
            fill_cache(&caches[i]);
        }
        fs::remove_dir_all(root).unwrap();
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn abandon_only_removes_owned_empty_directories_and_preserves_the_lock_file() {
        use std::os::unix::fs::MetadataExt;
        let (root, acceleration, models, runtime) = cache_fixture("cache-abandon");
        let wd = acceleration.coreml_cache(Model::Wd, &models[0], &runtime).unwrap();
        fs::create_dir_all(&wd).unwrap();
        let ccip = acceleration.coreml_cache(Model::Ccip, &models[1], &runtime).unwrap();
        fill_cache(&ccip);
        let lock = acceleration.compile_lock(Backend::CoreMl).unwrap();
        let inode = lock.metadata().unwrap().ino();
        drop(lock);
        acceleration.abandon(Model::Wd, Backend::CoreMl);
        assert!(!wd.exists());
        assert!(!wd.parent().unwrap().exists());
        assert!(ccip.join("compiled").exists());
        let lock = acceleration.compile_lock(Backend::CoreMl).unwrap();
        assert_eq!(lock.metadata().unwrap().ino(), inode, "不能通过删除重建锁文件破坏进程间互斥");
        drop(lock);
        acceleration.abandon(Model::Ccip, Backend::CoreMl);
        assert!(ccip.join("compiled").exists(), "保留非空编译缓存");
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn fallback_is_recorded_per_model_and_without_shared_nothing_is_written() {
        let root = temp("status");
        let acceleration = Acceleration { enabled: true, shared: Some(root.clone()) };
        acceleration.record(Model::Ccip, Backend::CoreMl, Backend::CoreMl, None);
        acceleration.record(Model::Wd, Backend::DirectMl, Backend::Cpu, Some("no device"));
        let status = read_status(&root.join(STATUS_FILE));
        assert_eq!((status["ccip"]["used"].as_str(), status["ccip"]["reason"].as_str()), (Some("coreml"), None));
        assert_eq!((status["wd"]["used"].as_str(), status["wd"]["reason"].as_str()), (Some("cpu"), Some("no device")));
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1, "no temporary file left");
        Acceleration::cpu().record(Model::Ccip, Backend::Cpu, Backend::Cpu, None);
        fs::remove_dir_all(root).unwrap();
    }
}
