//! 显式调用的合成数值探针；不改变已安装生产环境的路由。
use illustash_platform::platform_research_math;
use std::{io::Read, path::PathBuf};
pub(super) fn run(input: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let mut text = String::new();
    std::fs::File::open(input)?.read_to_string(&mut text)?;
    let cases: Vec<serde_json::Value> = serde_json::from_str(&text)?;
    let mut results = vec![];
    for case in cases {
        let m = case["m"].as_u64().ok_or("missing rows")? as usize;
        let p = case["p"].as_u64().ok_or("missing columns")? as usize;
        let dim = case["dim"].as_u64().ok_or("missing width")? as usize;
        let same = case["same"].as_bool() == Some(true);
        #[cfg(target_os = "windows")]
        if case["dtype"].as_str() == Some("f64") {
            let a: Vec<f64> = serde_json::from_value(case["a"].clone())?;
            let other: Vec<f64> = if same { vec![] } else { serde_json::from_value(case["b"].clone())? };
            let b = if same { &a } else { &other };
            let result = platform_research_math::cross64(&a, m, b, p, dim)?;
            results.push(serde_json::json!(result.iter().map(|x| x.to_bits()).collect::<Vec<_>>()));
            continue;
        }
        let a: Vec<f32> = serde_json::from_value(case["a"].clone())?;
        let other: Vec<f32> = if same { vec![] } else { serde_json::from_value(case["b"].clone())? };
        let b = if same { &a } else { &other };
        let result = platform_research_math::cross(&a, m, b, p, dim)?;
        results.push(serde_json::json!(result.iter().map(|x| x.to_bits()).collect::<Vec<_>>()));
    }
    #[cfg(target_os = "windows")]
    let identity = platform_research_math::windows_backend_identity()?;
    #[cfg(not(target_os = "windows"))]
    let identity = "existing platform backend";
    println!("{}", serde_json::json!({"identity":identity,"bits":results}));
    Ok(())
}
