//! 跑在 CPU 或加速器（backend.rs）上的 ONNX 会话。模型和运行库都是显式输入，从不下载，也不在用户数据里查找。
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
    Error::invalid(format!("原生识别 {context}: {error}"))
}
static RUNTIME: OnceLock<std::result::Result<std::path::PathBuf, String>> = OnceLock::new();
/// 在构建会话之前调用；一个子进程只绑定一个已安装的运行库。
/// 本进程没设 `ORT_DISABLE_TELEMETRY=1` 时拒绝加载（#381，见 telemetry.rs）。
pub fn initialise(runtime: &Path) -> Result<()> {
    super::telemetry::require()?;
    let requested =
        runtime.canonicalize().map_err(|e| failure(&format!("运行库 {path}", path = runtime.display()), e))?;
    let loaded = RUNTIME.get_or_init(|| {
        // 绑定库的加载器在 ABI 或符号无效时会 panic。在 worker 边界上兜住它，
        // 诊断信息里保留显式给出的路径，不回退到 PATH。`with_telemetry(false)` 调 DisableTelemetryEvents：
        // macOS 上环境变量已经让上传器不存在，这一句是第二道；Windows 的 ETW 只认这一句。
        std::panic::catch_unwind(|| ort::init_from(requested.display()).with_telemetry(false).commit())
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
/// 一个模型的会话，跑在 `Acceleration` 为它选的后端上（backend.rs）；加速器建不起来或运行中失败时在 CPU 上重建一次。
/// CPU 会话的构建方式与 #270 之前完全相同。
struct ModelSession {
    session: Session,
    backend: Backend,
    model: Model,
    path: PathBuf,
    execution: Option<PathBuf>,
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
/// 设置页显示的内容：运行库自己的原话，去掉错误种类，最多 300 个字符。
fn reason(error: &Error) -> String {
    let text = match error {
        Error::Invalid(text) => text.clone(),
        other => other.to_string(),
    };
    text.trim().chars().take(300).collect()
}
/// 本进程在 `initialise` 里绑定的运行库（之前为空）。
pub(super) fn runtime() -> PathBuf {
    RUNTIME.get().and_then(|r| r.as_ref().ok()).cloned().unwrap_or_default()
}
impl ModelSession {
    fn load(path: &Path, model: Model, threads: Option<usize>, acceleration: &Acceleration) -> Result<Self> {
        let requested = acceleration.backend(model);
        let mut execution = None;
        let accelerated = (requested != Backend::Cpu).then(|| {
            let path = super::wd_coreml::execution_path(path, model, requested)?;
            execution = Some(path.clone());
            let _lock = acceleration.compile_lock(requested);
            Session::builder()
                .map_err(|e| failure("会话", e))
                .and_then(|b| acceleration.configure(b, requested, model, &path, &runtime()))
                .and_then(|b| build(b, &path, threads))
        });
        let (session, backend) = match accelerated {
            Some(Ok(session)) => {
                acceleration.record(model, requested, requested, None);
                (session, requested)
            }
            // CPU 也加载不了的模型或运行库不怪加速器：照旧报它的错误。
            Some(Err(e)) => match cpu_session(path, threads) {
                Ok(session) => {
                    acceleration.fell_back(model, requested, execution.as_deref(), &runtime(), &reason(&e));
                    (session, Backend::Cpu)
                }
                Err(cpu) => {
                    acceleration.abandon(model, requested);
                    return Err(cpu);
                }
            },
            None => (cpu_session(path, threads)?, Backend::Cpu),
        };
        Ok(Self {
            session,
            backend,
            model,
            path: path.to_owned(),
            execution,
            threads,
            acceleration: acceleration.clone(),
        })
    }
    /// `run` 从输出里读出它需要的全部内容。加速会话失败时（出错，或输出校验不通过），这张图片在一个 CPU 会话上
    /// 重跑，这个 CPU 会话负责这个 worker 余下的工作。
    fn run<T>(&mut self, mut run: impl FnMut(&mut Session) -> Result<T>) -> Result<T> {
        match run(&mut self.session) {
            Err(e) if self.backend != Backend::Cpu => {
                self.acceleration.fell_back(
                    self.model,
                    self.backend,
                    self.execution.as_deref(),
                    &runtime(),
                    &reason(&e),
                );
                self.session = cpu_session(&self.path, self.threads)?;
                self.backend = Backend::Cpu;
                run(&mut self.session)
            }
            result => result,
        }
    }
}
/// #150 用的可复用 CCIP 会话；加载它不需要 WD 模型。
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
    /// 会话当前运行的后端。
    pub fn backend(&self) -> Backend {
        self.session.backend
    }
    /// 返回 768 个原始 f32 值；Features 的归一化和缓存由使用方负责。
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
/// WD 的 intra-op 线程数（#213）：Windows 上用 ONNX Runtime 的默认值（每个物理核一个），其他平台 4 个；
/// 在 M2 上线程再多就会用到能效核，WD 反而变慢。线程数不改变 CPU 输出，逐位一致。
pub const WD_THREADS: Option<usize> = if cfg!(windows) { None } else { Some(4) };
/// 跑在 `Acceleration` 为它选的后端上的 WD；`ModelSessions` 和基准测试的分阶段计时
/// （examples/recognition_compute/bench.rs）都用它，所以计时的就是生产实际运行的代码。
pub struct WdSession {
    session: ModelSession,
}
impl WdSession {
    pub fn load(model: &Path, runtime: &Path, acceleration: &Acceleration) -> Result<Self> {
        initialise(runtime)?;
        let session = ModelSession::load(model, Model::Wd, WD_THREADS, acceleration)?;
        if session.session.inputs.len() != 1 || session.session.outputs.len() != 2 {
            return Err(failure("WD 模型", "输入/输出数量不匹配"));
        }
        Ok(Self { session })
    }
    /// 会话当前运行的后端。
    pub fn backend(&self) -> Backend {
        self.session.backend
    }
    /// 对一个预处理好的 448×448 输入（preprocess::wd_input）给出标签概率和 1024 个原始向量值。
    pub fn run(&mut self, input: &[f32], tags: usize) -> Result<(Vec<f32>, Vec<f32>)> {
        self.session.run(|session| {
            let input = Tensor::from_array(([1, 448, 448, 3], input.to_vec())).map_err(|e| failure("WD 输入", e))?;
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
        })
    }
}
pub struct ModelSessions {
    wd: WdSession,
    ccip: CcipSession,
}
pub struct ModelOutput {
    pub probabilities: Vec<f32>,
    pub wd: Vec<f32>,
    pub ccip: Vec<f32>,
}
impl ModelSessions {
    pub fn load(models: &Path, runtime: &Path, acceleration: &Acceleration) -> Result<Self> {
        let wd = WdSession::load(&models.join("wd_swinv2_v3_emb.onnx"), runtime, acceleration)?;
        let ccip = CcipSession::load(&models.join("ccip/model_feat.onnx"), runtime, acceleration)?;
        Ok(Self { wd, ccip })
    }
    /// WD 和 CCIP 当前运行的后端。
    pub fn backends(&self) -> (Backend, Backend) {
        (self.wd.backend(), self.ccip.backend())
    }
    pub fn ccip_features(&mut self, image: &RgbImage) -> Result<Vec<f32>> {
        self.ccip.features(image)
    }
    pub fn infer(&mut self, image: &RgbImage, tags: usize) -> Result<ModelOutput> {
        let (probabilities, wd) = self.wd.run(&super::preprocess::wd_input(image, 448), tags)?;
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

#[cfg(test)]
mod platform_tests {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    use super::*;

    /// 与实际模型验收同样需要显式资源和隔离的 HOME；不在普通测试中加载大型模型。
    #[test]
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[ignore = "需要显式的 ILLUSTASH_RECOGNITION_MODELS 与 ILLUSTASH_ORT_DYLIB，单独验收"]
    fn wd_runtime_failure_rebuilds_the_original_cpu_model() {
        let models = PathBuf::from(std::env::var_os("ILLUSTASH_RECOGNITION_MODELS").expect("显式模型目录"));
        let library = PathBuf::from(std::env::var_os("ILLUSTASH_ORT_DYLIB").expect("显式运行库"));
        initialise(&library).unwrap();
        let original = models.join("wd_swinv2_v3_emb.onnx");
        let shared = std::env::temp_dir().join(format!("illustash-wd-run-fallback-{}", std::process::id()));
        std::fs::create_dir_all(&shared).unwrap();
        let input: Vec<f32> = (0..448 * 448 * 3).map(|i| (i % 251) as f32).collect();
        let infer = |session: &mut Session| -> Result<Vec<Vec<f32>>> {
            let tensor = Tensor::from_array(([1, 448, 448, 3], input.clone())).map_err(|e| failure("输入", e))?;
            let output = session.run(ort::inputs![tensor]).map_err(|e| failure("运行", e))?;
            (0..2)
                .map(|i| output[i].try_extract_tensor::<f32>().map(|(_, v)| v.to_vec()).map_err(|e| failure("输出", e)))
                .collect()
        };
        let expected = infer(&mut cpu_session(&original, WD_THREADS).unwrap()).unwrap();
        let acceleration = Acceleration::from_env(Some(&shared));
        let mut loaded = ModelSession::load(&original, Model::Wd, WD_THREADS, &acceleration).unwrap();
        assert_eq!(loaded.backend, Backend::CoreMl);
        assert_ne!(loaded.execution.as_deref(), Some(original.as_path()));
        let mut attempts = 0;
        let actual = loaded
            .run(|session| {
                attempts += 1;
                if attempts == 1 {
                    return Err(failure("输出校验", "模拟运行中的设备/输出故障"));
                }
                infer(session)
            })
            .unwrap();
        assert_eq!(attempts, 2, "同一图片只回退重跑一次");
        assert_eq!(loaded.backend, Backend::Cpu);
        assert_eq!(actual, expected, "必须回到原始模型，不能在 CPU 上运行派生图");
        assert_eq!(loaded.run(infer).unwrap(), expected, "该 worker 后续继续使用原始 CPU 模型");
        let status: serde_json::Value =
            serde_json::from_slice(&std::fs::read(shared.join("acceleration.json")).unwrap()).unwrap();
        assert_eq!(status["wd"]["used"], "cpu");
        assert!(status["wd"]["reason"].as_str().unwrap().contains("模拟运行"));
        drop(loaded);
        std::fs::remove_dir_all(shared).unwrap();
    }
}
