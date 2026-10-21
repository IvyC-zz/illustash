//! ONNX sessions on the CPU or an accelerator (backend.rs). Models/runtime are explicit inputs, never fetched or
//! discovered in user data.
use super::backend::{Acceleration, Backend, Model};
use crate::{Error, Result};
use image::RgbImage;
use ort::{
    execution_providers::CPUExecutionProvider,
    logging::LogLevel,
    session::{builder::SessionBuilder, Session},
    value::Tensor,
};
use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
};

fn failure(context: &str, error: impl std::fmt::Display) -> Error {
    Error::Invalid(format!("原生识别 {context}: {error}"))
}
static RUNTIME: OnceLock<std::result::Result<std::path::PathBuf, String>> = OnceLock::new();
/// Call before constructing sessions; one subprocess binds one installed runtime.
pub fn initialise(runtime: &Path) -> Result<()> {
    let requested =
        runtime.canonicalize().map_err(|e| failure(&format!("运行库 {path}", path = runtime.display()), e))?;
    let loaded = RUNTIME.get_or_init(|| {
        // The binding's loader panics on invalid ABI/symbols. Contain it at the worker boundary
        // and retain the explicit path in the diagnostic rather than falling back to PATH.
        std::panic::catch_unwind(|| ort::init_from(requested.display()).commit())
            .map_err(|_| {
                format!("无法加载 {path}（需要 ONNX Runtime >=1.22，含 OrtGetApiBase）", path = requested.display())
            })
            .and_then(|result| result.map(|_| requested.clone()).map_err(|e| e.to_string()))
    });
    match loaded {
        Ok(loaded) if loaded == &requested => Ok(()),
        Ok(loaded) => Err(failure(
            "运行库",
            format!("进程已绑定 {loaded}，拒绝切换 {path}", loaded = loaded.display(), path = requested.display()),
        )),
        Err(error) => Err(failure("运行库", error)),
    }
}
/// One model's session on the backend `Acceleration` picks for it (backend.rs), rebuilt on the CPU once if the
/// accelerator cannot be built or fails while running. CPU sessions are built exactly as before #270.
struct ModelSession {
    session: Session,
    backend: Backend,
    model: Model,
    path: PathBuf,
    threads: Option<usize>,
    acceleration: Acceleration,
}
fn cpu_session(path: &Path, threads: Option<usize>) -> Result<Session> {
    build(
        Session::builder()
            .map_err(|e| failure("会话", e))?
            .with_execution_providers([CPUExecutionProvider::default().build()])
            .map_err(|e| failure("CPU", e))?,
        path,
        threads,
    )
}
fn build(builder: SessionBuilder, path: &Path, threads: Option<usize>) -> Result<Session> {
    let mut builder = builder.with_log_level(LogLevel::Error).map_err(|e| failure("日志", e))?;
    if let Some(threads) = threads {
        builder = builder.with_intra_threads(threads).map_err(|e| failure("线程", e))?;
    }
    builder.commit_from_file(path).map_err(|e| failure(&path.display().to_string(), e))
}
fn runtime() -> PathBuf {
    RUNTIME.get().and_then(|r| r.as_ref().ok()).cloned().unwrap_or_default()
}
impl ModelSession {
    fn load(path: &Path, model: Model, threads: Option<usize>, acceleration: &Acceleration) -> Result<Self> {
        let requested = acceleration.backend(model);
        let accelerated = (requested != Backend::Cpu).then(|| {
            let _lock = acceleration.compile_lock(requested);
            Session::builder()
                .map_err(|e| failure("会话", e))
                .and_then(|b| acceleration.configure(b, requested, model, path, &runtime()))
                .and_then(|b| build(b, path, threads))
        });
        let (session, backend) = match accelerated {
            Some(Ok(session)) => {
                acceleration.record(model, requested, requested, None);
                (session, requested)
            }
            // A model or runtime the CPU cannot load either is not the accelerator's fault: its error, as before.
            Some(Err(e)) => match cpu_session(path, threads) {
                Ok(session) => {
                    acceleration.fell_back(model, requested, path, &runtime(), &e.to_string());
                    (session, Backend::Cpu)
                }
                Err(cpu) => {
                    acceleration.abandon(requested);
                    return Err(cpu);
                }
            },
            None => (cpu_session(path, threads)?, Backend::Cpu),
        };
        Ok(Self { session, backend, model, path: path.to_owned(), threads, acceleration: acceleration.clone() })
    }
    /// `run` reads everything it needs out of the outputs. If an accelerated session fails (an error, or outputs
    /// that do not validate), the picture is run again on a CPU session that serves the rest of this worker.
    fn run<T>(&mut self, mut run: impl FnMut(&mut Session) -> Result<T>) -> Result<T> {
        match run(&mut self.session) {
            Err(e) if self.backend != Backend::Cpu => {
                self.acceleration.fell_back(self.model, self.backend, &self.path, &runtime(), &e.to_string());
                self.session = cpu_session(&self.path, self.threads)?;
                self.backend = Backend::Cpu;
                run(&mut self.session)
            }
            result => result,
        }
    }
}
/// Reusable CCIP session for #150; loading it does not require a WD model.
pub struct CcipSession {
    session: ModelSession,
}
impl CcipSession {
    pub fn load(model: &Path, runtime: &Path, acceleration: &Acceleration) -> Result<Self> {
        initialise(runtime)?;
        let session = ModelSession::load(model, Model::Ccip, None, acceleration)?;
        if session.session.inputs.len() != 1 || session.session.outputs.is_empty() {
            return Err(failure("CCIP 模型", "输入/输出数量不匹配"));
        }
        Ok(Self { session })
    }
    /// The backend the session runs on now.
    pub fn backend(&self) -> Backend {
        self.session.backend
    }
    /// Return 768 raw f32 values; the consumer owns any Features normalization/cache.
    pub fn features(&mut self, image: &RgbImage) -> Result<Vec<f32>> {
        self.session.run(|session| {
            let input = Tensor::from_array(([1, 3, 384, 384], super::preprocess::ccip_input(image, 384)))
                .map_err(|e| failure("CCIP 输入", e))?;
            let output = session.run(ort::inputs![input]).map_err(|e| failure("CCIP", e))?;
            let (shape, values) = output[0].try_extract_tensor::<f32>().map_err(|e| failure("CCIP 输出", e))?;
            if shape[..] != [1, 768] {
                return Err(failure("CCIP 输出", "应为 (1,768)"));
            }
            validate_vector(values, 768)?;
            Ok(values.to_vec())
        })
    }
}
/// WD intra-op threads (#213): the ONNX Runtime default (one per physical core) on Windows,
/// 4 elsewhere; on the M2 more threads reach the efficiency cores and WD gets slower.
/// Thread count does not change the CPU outputs, bit for bit.
pub const WD_THREADS: Option<usize> = if cfg!(windows) { None } else { Some(4) };
pub struct ModelSessions {
    wd: ModelSession,
    ccip: CcipSession,
}
pub struct ModelOutput {
    pub probabilities: Vec<f32>,
    pub wd: Vec<f32>,
    pub ccip: Vec<f32>,
}
impl ModelSessions {
    pub fn load(models: &Path, runtime: &Path, acceleration: &Acceleration) -> Result<Self> {
        initialise(runtime)?;
        let wd = ModelSession::load(&models.join("wd_swinv2_v3_emb.onnx"), Model::Wd, WD_THREADS, acceleration)?;
        if wd.session.inputs.len() != 1 || wd.session.outputs.len() != 2 {
            return Err(failure("WD 模型", "输入/输出数量不匹配"));
        }
        let ccip = CcipSession::load(&models.join("ccip/model_feat.onnx"), runtime, acceleration)?;
        Ok(Self { wd, ccip })
    }
    /// The backends WD and CCIP run on now.
    pub fn backends(&self) -> (Backend, Backend) {
        (self.wd.backend, self.ccip.backend())
    }
    pub fn ccip_features(&mut self, image: &RgbImage) -> Result<Vec<f32>> {
        self.ccip.features(image)
    }
    pub fn infer(&mut self, image: &RgbImage, tags: usize) -> Result<ModelOutput> {
        let (probabilities, wd) = self.wd.run(|session| {
            let input = Tensor::from_array(([1, 448, 448, 3], super::preprocess::wd_input(image, 448)))
                .map_err(|e| failure("WD 输入", e))?;
            let output = session.run(ort::inputs![input]).map_err(|e| failure("WD", e))?;
            let (shape, probabilities) = output[0].try_extract_tensor::<f32>().map_err(|e| failure("WD 概率", e))?;
            if shape[..] != [1, tags as i64] || probabilities.iter().any(|v| !v.is_finite() || !(0.0..=1.0).contains(v))
            {
                return Err(failure("WD 概率", "应为 (1,标签数)，有限概率"));
            }
            let (shape, wd) = output[1].try_extract_tensor::<f32>().map_err(|e| failure("WD 向量", e))?;
            if shape[..] != [1, 1024] {
                return Err(failure("WD 向量", "应为 (1,1024)"));
            }
            validate_vector(wd, 1024)?;
            Ok((probabilities.to_vec(), wd.to_vec()))
        })?;
        let ccip = self.ccip_features(image)?;
        Ok(ModelOutput { probabilities, wd, ccip })
    }
}
pub(crate) fn validate_vector(values: &[f32], dimension: usize) -> Result<()> {
    if values.len() != dimension
        || values.iter().any(|v| !v.is_finite())
        || values.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() <= 1e-24
    {
        return Err(failure("向量", "维度、有限值或非零范数不合法"));
    }
    Ok(())
}
