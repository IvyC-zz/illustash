//! CPU ONNX sessions. Models/runtime are explicit inputs, never fetched or discovered in user data.
use crate::{Error, Result};
use image::RgbImage;
use ort::{execution_providers::CPUExecutionProvider, logging::LogLevel, session::Session, value::Tensor};
use std::{path::Path, sync::OnceLock};

fn failure(context: &str, error: impl std::fmt::Display) -> Error {
    Error::Invalid(format!("原生识别 {context}: {error}"))
}
static RUNTIME: OnceLock<std::result::Result<std::path::PathBuf, String>> = OnceLock::new();
/// Call before constructing sessions; one subprocess binds one installed runtime.
pub fn initialise(runtime: &Path) -> Result<()> {
    let requested = runtime.canonicalize().map_err(|e| failure(&format!("运行库 {}", runtime.display()), e))?;
    let loaded = RUNTIME.get_or_init(|| {
        // The binding's loader panics on invalid ABI/symbols. Contain it at the worker boundary
        // and retain the explicit path in the diagnostic rather than falling back to PATH.
        std::panic::catch_unwind(|| ort::init_from(requested.display()).commit())
            .map_err(|_| format!("无法加载 {}（需要 ONNX Runtime >=1.22，含 OrtGetApiBase）", requested.display()))
            .and_then(|result| result.map(|_| requested.clone()).map_err(|e| e.to_string()))
    });
    match loaded {
        Ok(loaded) if loaded == &requested => Ok(()),
        Ok(loaded) => {
            Err(failure("运行库", format!("进程已绑定 {}，拒绝切换 {}", loaded.display(), requested.display())))
        }
        Err(error) => Err(failure("运行库", error)),
    }
}
/// Reusable CPU CCIP session for #150; loading it does not require a WD model.
pub struct CcipSession {
    session: Session,
}
fn load_session(path: &Path, threads: Option<usize>) -> Result<Session> {
    let mut builder = Session::builder()
        .map_err(|e| failure("会话", e))?
        .with_execution_providers([CPUExecutionProvider::default().build()])
        .map_err(|e| failure("CPU", e))?
        .with_log_level(LogLevel::Error)
        .map_err(|e| failure("日志", e))?;
    if let Some(threads) = threads {
        builder = builder.with_intra_threads(threads).map_err(|e| failure("线程", e))?;
    }
    builder.commit_from_file(path).map_err(|e| failure(&path.display().to_string(), e))
}
impl CcipSession {
    pub fn load(model: &Path, runtime: &Path) -> Result<Self> {
        initialise(runtime)?;
        let session = load_session(model, None)?;
        if session.inputs.len() != 1 || session.outputs.is_empty() {
            return Err(failure("CCIP 模型", "输入/输出数量不匹配"));
        }
        Ok(Self { session })
    }
    /// Return 768 raw f32 values; the consumer owns any Features normalization/cache.
    pub fn features(&mut self, image: &RgbImage) -> Result<Vec<f32>> {
        let input = Tensor::from_array(([1, 3, 384, 384], super::preprocess::ccip_input(image, 384)))
            .map_err(|e| failure("CCIP 输入", e))?;
        let output = self.session.run(ort::inputs![input]).map_err(|e| failure("CCIP", e))?;
        let (shape, values) = output[0].try_extract_tensor::<f32>().map_err(|e| failure("CCIP 输出", e))?;
        if shape[..] != [1, 768] {
            return Err(failure("CCIP 输出", "应为 (1,768)"));
        }
        validate_vector(values, 768)?;
        Ok(values.to_vec())
    }
}
pub struct ModelSessions {
    wd: Session,
    ccip: CcipSession,
}
pub struct ModelOutput {
    pub probabilities: Vec<f32>,
    pub wd: Vec<f32>,
    pub ccip: Vec<f32>,
}
impl ModelSessions {
    pub fn load(models: &Path, runtime: &Path) -> Result<Self> {
        initialise(runtime)?;
        let wd = load_session(&models.join("wd_swinv2_v3_emb.onnx"), Some(4))?;
        if wd.inputs.len() != 1 || wd.outputs.len() != 2 {
            return Err(failure("WD 模型", "输入/输出数量不匹配"));
        }
        let ccip = CcipSession::load(&models.join("ccip/model_feat.onnx"), runtime)?;
        Ok(Self { wd, ccip })
    }
    pub fn ccip_features(&mut self, image: &RgbImage) -> Result<Vec<f32>> {
        self.ccip.features(image)
    }
    pub fn infer(&mut self, image: &RgbImage, tags: usize) -> Result<ModelOutput> {
        let input = Tensor::from_array(([1, 448, 448, 3], super::preprocess::wd_input(image, 448)))
            .map_err(|e| failure("WD 输入", e))?;
        let output = self.wd.run(ort::inputs![input]).map_err(|e| failure("WD", e))?;
        let (shape, probabilities) = output[0].try_extract_tensor::<f32>().map_err(|e| failure("WD 概率", e))?;
        if shape[..] != [1, tags as i64] || probabilities.iter().any(|v| !v.is_finite() || !(0.0..=1.0).contains(v)) {
            return Err(failure("WD 概率", "应为 (1,标签数)，有限概率"));
        }
        let (shape, wd) = output[1].try_extract_tensor::<f32>().map_err(|e| failure("WD 向量", e))?;
        if shape[..] != [1, 1024] {
            return Err(failure("WD 向量", "应为 (1,1024)"));
        }
        validate_vector(wd, 1024)?;
        let (probabilities, wd) = (probabilities.to_vec(), wd.to_vec());
        drop(output);
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
