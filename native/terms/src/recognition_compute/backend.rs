//! Execution backends for the recognition sessions (#270: CoreML on macOS; #271: DirectML on Windows).
//! The CPU is the reference. An accelerated session that cannot be built, or fails while running, falls back to the
//! CPU for the rest of the worker, and the reason is recorded in Shared/acceleration.json. Accelerated outputs differ
//! from the CPU's in fp32 rounding only (docs/TERMS_PERFORMANCE.md 第 6 节).
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

/// `ILLUSTASH_CPU_ONLY=1` keeps every session on the CPU whatever the setting says: the bit-for-bit acceptance runs
/// and anyone ruling the accelerator out set it. Empty and `0` mean unset.
pub const CPU_ONLY_ENV: &str = "ILLUSTASH_CPU_ONLY";
/// Whether this build has an accelerated backend at all; the setting is offered only where it does.
pub const AVAILABLE: bool = cfg!(all(target_os = "macos", target_arch = "aarch64"));
/// The compiled CoreML models, one folder per key (cache_key), under the shared folder.
const COREML_CACHE: &str = "coreml-cache";
/// What the last worker used, and why it fell back, per model: read by GET /api/models.
pub const STATUS_FILE: &str = "acceleration.json";

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
    /// The dynamic batch dimension; pinned to 1 for accelerators that need static shapes.
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
}

/// One worker's choice: the switch (setting and CPU_ONLY_ENV) and where the shared folder is.
#[derive(Clone, Debug)]
pub struct Acceleration {
    enabled: bool,
    shared: Option<PathBuf>,
}
impl Acceleration {
    pub fn cpu() -> Self {
        Self { enabled: false, shared: None }
    }
    /// For a worker: on unless CPU_ONLY_ENV is set or 「使用 GPU 加速识别」 is off in preferences.json.
    pub fn from_env(shared: Option<&Path>) -> Self {
        let forced = std::env::var_os(CPU_ONLY_ENV).is_some_and(|v| !v.is_empty() && v != "0");
        Self { enabled: AVAILABLE && !forced && crate::app_preferences::acceleration(), shared: shared.map(Into::into) }
    }
    /// The platform policy (#213): on macOS only CCIP goes to CoreML. WD on CoreML is no faster, takes about ten
    /// minutes to compile and caches 2.1 GB.
    pub fn backend(&self, model: Model) -> Backend {
        if !self.enabled {
            return Backend::Cpu;
        }
        match model {
            Model::Ccip if cfg!(target_os = "macos") => Backend::CoreMl,
            _ => Backend::Cpu,
        }
    }
    /// The accelerator's part of the session: providers, pinned shapes, cache. CPU sessions never come here.
    pub(super) fn configure(
        &self,
        builder: SessionBuilder,
        backend: Backend,
        model: Model,
        path: &Path,
        runtime: &Path,
    ) -> Result<SessionBuilder> {
        let failed = |e: ort::Error| Error::Invalid(format!("{} {}: {e}", backend.name(), model.name()));
        match backend {
            Backend::CoreMl => {
                // Without a static batch CoreML takes 27 of CCIP's 1486 nodes; with it the whole graph is one partition.
                let builder = builder.with_dimension_override(model.batch(), 1).map_err(failed)?;
                let mut provider = CoreMLExecutionProvider::default()
                    .with_model_format(CoreMLModelFormat::MLProgram)
                    .with_compute_units(CoreMLComputeUnits::All)
                    .with_static_input_shapes(true);
                if let Some(cache) = self.coreml_cache(path, runtime) {
                    provider = provider.with_model_cache_dir(cache.display());
                }
                builder.with_execution_providers([provider.build().error_on_failure()]).map_err(failed)
            }
            Backend::DirectMl | Backend::Cpu => Err(Error::Invalid(format!("{} 不在本平台", backend.name()))),
        }
    }
    /// Shared/coreml-cache/<key>. A key that changes (a new model, runtime or macOS) leaves the old folders unused,
    /// so they are removed; the first session then compiles again (about 4 s for CCIP, then about 0.65 s per start).
    fn coreml_cache(&self, path: &Path, runtime: &Path) -> Option<PathBuf> {
        let root = self.shared.as_ref()?.join(COREML_CACHE);
        let key = cache_key(path, runtime).inspect_err(|e| eprintln!("识别加速：缓存键 {e}")).ok()?;
        fs::create_dir_all(&root).inspect_err(|e| eprintln!("识别加速：缓存目录 {e}")).ok()?;
        for entry in fs::read_dir(&root).into_iter().flatten().flatten() {
            let name = entry.file_name();
            if name != key.as_str() && !name.to_string_lossy().ends_with(".lock") {
                let _ = fs::remove_dir_all(entry.path());
            }
        }
        Some(root.join(key))
    }
    /// The accelerator could not be built, or failed while running: drop its cache (it may be what broke) and say so.
    pub(super) fn fell_back(&self, model: Model, backend: Backend, path: &Path, runtime: &Path, reason: &str) {
        eprintln!("识别加速：{} 改用 CPU（{}）：{reason}", model.name(), backend.name());
        if backend == Backend::CoreMl {
            if let Some(cache) = self.coreml_cache(path, runtime) {
                let _ = fs::remove_dir_all(cache);
            }
        }
        self.record(model, backend, Backend::Cpu, Some(reason));
    }
    /// Undo coreml_cache and compile_lock when nothing could be loaded at all: no folder is left behind.
    pub(super) fn abandon(&self, backend: Backend) {
        let Some(root) = self.shared.as_ref().map(|s| s.join(COREML_CACHE)).filter(|_| backend == Backend::CoreMl)
        else {
            return;
        };
        for entry in fs::read_dir(&root).into_iter().flatten().flatten() {
            if entry.path().is_dir() && fs::read_dir(entry.path()).is_ok_and(|mut d| d.next().is_none()) {
                let _ = fs::remove_dir(entry.path());
            }
        }
        if fs::read_dir(&root).is_ok_and(|d| d.flatten().all(|e| e.file_name() == "compile.lock")) {
            let _ = fs::remove_file(root.join("compile.lock"));
            let _ = fs::remove_dir(&root);
        }
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
    /// Serialise the first compile between workers (recognition and index may start together): one compiles into
    /// the cache, the other then loads it. Waits at most a minute, then goes ahead without the lock.
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

/// GET /api/models: the switch's state, the accelerator this build would use, and what the last worker used.
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

/// SHA-256 of the model, the runtime and the system version: the compiled model is valid for exactly these.
fn cache_key(model: &Path, runtime: &Path) -> Result<String> {
    let mut digest = Sha256::new();
    digest.update(b"illustash-coreml/1 mlprogram all batch=1\n");
    for path in [model, runtime, Path::new("/System/Library/CoreServices/SystemVersion.plist")] {
        let mut file = fs::File::open(path).map_err(|e| Error::Invalid(format!("{}: {e}", path.display())))?;
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
        assert_eq!(on.backend(Model::Wd), Backend::Cpu, "WD stays on the CPU everywhere for now");
        let ccip = if cfg!(target_os = "macos") { Backend::CoreMl } else { Backend::Cpu };
        assert_eq!(on.backend(Model::Ccip), ccip);
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
        let first = acceleration.coreml_cache(&model, &runtime).unwrap();
        assert_eq!(first.parent().unwrap(), shared.join(COREML_CACHE));
        fs::create_dir_all(first.join("compiled")).unwrap();
        assert_eq!(acceleration.coreml_cache(&model, &runtime).unwrap(), first, "same content, same folder");
        assert!(first.join("compiled").exists());
        let lock = acceleration.compile_lock(Backend::CoreMl).unwrap();
        fs::write(&model, b"model two").unwrap();
        let second = acceleration.coreml_cache(&model, &runtime).unwrap();
        assert_ne!(second, first);
        assert!(!first.exists(), "the old model's compiled copy is removed");
        assert!(shared.join(COREML_CACHE).join("compile.lock").exists(), "the lock file stays");
        drop(lock);
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
