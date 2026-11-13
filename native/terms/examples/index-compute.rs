//! 显式调用的合成驱动；生产适配器用已安装的资源调用 run_files。
use illustash_terms::{
    index_compute::{self, numeric, Resources},
    recognition_compute::backend::Acceleration,
};
use serde_json::{json, Value};
use std::{fs, path::Path};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 单线程时关掉 ONNX Runtime 的遥测（#381）；session::initialise 没看到它会拒绝加载。
    illustash_terms::recognition_compute::telemetry::at_startup();
    let args: Vec<_> = std::env::args().collect();
    let input = Path::new(args.get(1).ok_or("request missing")?);
    let output = Path::new(args.get(2).ok_or("response missing")?);
    let req: Value = serde_json::from_slice(&fs::read(input)?)?;
    if req["mode"] == "numeric" {
        let rows: Vec<Vec<f32>> = serde_json::from_value(req["rows"].clone())?;
        let anchor = numeric::merge(&rows)?;
        fs::write(
            output,
            serde_json::to_vec(
                &json!({"anchor":anchor,"percentiles":numeric::percentiles(&rows,&anchor)?,"core":numeric::core(&rows)?,"nearest":numeric::nearest_nine(&serde_json::from_value::<Vec<f32>>(req["distances"].clone())?)}),
            )?,
        )?;
    } else {
        let tags = std::env::var("ILLUSTASH_INDEX_TAGS")?;
        let model = std::env::var("ILLUSTASH_INDEX_MODEL").unwrap_or_default();
        let runtime = std::env::var("ILLUSTASH_INDEX_RUNTIME").unwrap_or_default();
        // 与已安装的 worker 相同：除非设了 ILLUSTASH_CPU_ONLY（逐位验收会设），否则启用加速。
        let shared = std::env::var_os("ILLUSTASH_SHARED").map(std::path::PathBuf::from);
        let acceleration = Acceleration::from_env(shared.as_deref());
        index_compute::run_files(
            input,
            output,
            &Resources {
                tags: Path::new(&tags),
                model: Path::new(&model),
                runtime: Path::new(&runtime),
                acceleration: &acceleration,
            },
        )?;
    }
    Ok(())
}
