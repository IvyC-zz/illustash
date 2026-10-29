//! 按显式路径运行的合成比较驱动；从不自行选择模型或用户图库。
use illustash_terms::recognition_compute::{
    anchor_snapshot::Snapshot, anchors, backend, claims::Claims, preprocess, producer, session,
};
use serde_json::{json, Value};
use std::{fs, path::Path};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let request: Value = serde_json::from_slice(&fs::read(args.get(1).ok_or("request missing")?)?)?;
    let output = Path::new(args.get(2).ok_or("output missing")?);
    let mode = request["mode"].as_str().ok_or("mode missing")?;
    let result = match mode {
        "preprocess" => {
            let image = producer::decode(Path::new(request["image"].as_str().ok_or("image missing")?))?;
            let size = request["size"].as_u64().unwrap_or(16) as u32;
            json!({"wd":preprocess::wd_input(&image,size),"ccip":preprocess::ccip_input(&image,size)})
        }
        "infer" => {
            // 与已安装的 worker 相同：除非设了 ILLUSTASH_CPU_ONLY（逐位验收会设），否则启用加速。
            let shared = request["shared"].as_str().map(Path::new);
            let mut sessions = session::ModelSessions::load(
                Path::new(request["models"].as_str().ok_or("models missing")?),
                Path::new(request["runtime"].as_str().ok_or("runtime missing")?),
                &backend::Acceleration::from_env(shared),
            )?;
            let tags = producer::Tags::load(Path::new(request["tags"].as_str().ok_or("tags missing")?))?;
            let mut rows = Vec::new();
            for path in request["images"].as_array().ok_or("images missing")? {
                let path = path.as_str().ok_or("path invalid")?;
                let image = producer::decode(Path::new(path))?;
                let value = sessions.infer(&image, tags.len())?;
                rows.push(json!({"probs":value.probabilities,"wd":value.wd,"ccip":value.ccip,"labels":tags.labels("synthetic",&value.probabilities)?}));
            }
            json!(rows)
        }
        "produce" => producer::run(
            serde_json::from_value(request["request"].clone())?,
            Path::new(request["resources"].as_str().ok_or("resources missing")?),
            Path::new(request["models"].as_str().ok_or("models missing")?),
            Path::new(request["runtime"].as_str().ok_or("runtime missing")?),
            Path::new(request["shared"].as_str().ok_or("shared missing")?),
        )?,
        "claims" => {
            let started = std::time::Instant::now();
            let snapshot = Snapshot::load(
                Path::new(request["shared"].as_str().ok_or("shared missing")?),
                Path::new(request["source"].as_str().ok_or("source missing")?),
            )?;
            let snapshot_ms = started.elapsed().as_secs_f64() * 1000.;
            let assets: Vec<String> = serde_json::from_value(request["assets"].clone())?;
            let features: Vec<Vec<f32>> = serde_json::from_value(request["features"].clone())?;
            if features.len() != assets.len() {
                return Err("feature/asset mismatch".into());
            }
            let mut claims = vec![];
            for (asset, feature) in assets.iter().zip(&features) {
                claims.extend(snapshot.claims(asset, feature)?);
            }
            let identity = serde_json::from_value(request["identity"].clone())?;
            let models = json!({"synthetic":true});
            let mut sidecar =
                Claims::new(identity, assets.clone(), models.clone(), snapshot.fingerprint.clone(), claims);
            let bytes: Vec<u8> = features.iter().flatten().flat_map(|v| v.to_le_bytes()).collect();
            sidecar.validate(&sidecar.identity, &assets, &models, &snapshot, &bytes, &|| Ok(()))?;
            let accepted = serde_json::to_value(&sidecar.claims)?;
            let target = request["tamper_asset"].as_str().ok_or("tamper_asset missing")?;
            let claim = sidecar.claims.iter_mut().find(|c| c.asset == target).ok_or("tamper claim missing")?;
            claim.character = request["tamper_character"].as_str().ok_or("tamper_character missing")?.into();
            let tampered_rejected =
                sidecar.validate(&sidecar.identity, &assets, &models, &snapshot, &bytes, &|| Ok(())).is_err();
            json!({"entities":snapshot.index.entities,"claims":accepted,"tampered_rejected":tampered_rejected,"snapshot_ms":snapshot_ms})
        }
        "anchors" => {
            let vectors: Vec<Vec<f32>> = serde_json::from_value(request["vectors"].clone())?;
            let features: Vec<Vec<f32>> = serde_json::from_value(request["features"].clone())?;
            let distances = anchors::cross(&vectors, &features)?;
            let orders = features
                .iter()
                .map(|f| {
                    let d = anchors::cross(&vectors, std::slice::from_ref(f))?;
                    Ok(anchors::nearest_order(&d.into_iter().map(|r| r[0]).collect::<Vec<_>>()))
                })
                .collect::<illustash_terms::Result<Vec<_>>>()?;
            json!({"distances": distances,"orders":orders})
        }
        "bench-generate" => bench::generate(&request)?,
        "bench-stages" => bench::stages(&request)?,
        _ => return Err("unsupported mode".into()),
    };
    fs::write(output, serde_json::to_vec(&result)?)?;
    Ok(())
}

#[path = "recognition_compute/bench.rs"]
mod bench;
