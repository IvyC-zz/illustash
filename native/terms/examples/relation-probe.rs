//! 仅供测试的显式传输通道，供独立的 Python 关系与图像参考实现调用。
use illustash_terms::research_compute::{
    relation_image::{self, Plane},
    relations,
};
use serde_json::{json, Value};
use std::{fs, path::Path};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 2 {
        return Err("request.json response.json required".into());
    }
    let request: Value = serde_json::from_slice(&fs::read(&args[0])?)?;
    let field = |key: &str| request[key].as_str().ok_or("missing path").map(Path::new);
    let plane = |key: &str| -> Result<Plane, Box<dyn std::error::Error>> {
        let r = &request[key];
        let p = r["pixels"]
            .as_array()
            .ok_or("pixels required")?
            .iter()
            .map(|x| x.as_f64().map(|x| x as f32).ok_or("numeric pixels required"))
            .collect::<Result<_, _>>()?;
        Ok(Plane::new(
            r["width"].as_u64().ok_or("width required")? as usize,
            r["height"].as_u64().ok_or("height required")? as usize,
            r["channels"].as_u64().ok_or("channels required")? as usize,
            p,
        )?)
    };
    let output = match request["op"].as_str() {
        Some("signature") => {
            let (ratio, grid) = relation_image::signature(field("path")?)?;
            json!({"ratio":ratio,"grid":grid})
        }
        Some("signature_bytes") => {
            let bytes = fs::read(field("path")?)?;
            let (ratio, grid) = relation_image::signature_bytes(&bytes)?;
            json!({"ratio":ratio,"grid":grid})
        }
        Some("colour") => json!(relation_image::reference_colour(field("path")?)?),
        Some("colour_bytes") => json!(relation_image::reference_colour_bytes(&fs::read(field("path")?)?)?),
        Some("load") => {
            let loaded = relations::load(field("path")?)?;
            json!({"width":loaded.image.width,"height":loaded.image.height,"size":loaded.size,"bits":loaded.image.pixels.iter().map(|v|v.to_bits()).collect::<Vec<_>>()})
        }
        Some("resize") => {
            let p = plane("image")?;
            let p = p.resize(
                request["width"].as_u64().ok_or("width required")? as usize,
                request["height"].as_u64().ok_or("height required")? as usize,
            );
            json!({"bits":p.pixels.iter().map(|v|v.to_bits()).collect::<Vec<_>>()})
        }
        Some("relate") => relations::relate(&relations::load(field("a")?)?, &relations::load(field("b")?)?)?,
        Some("locate") => json!(relations::locate(
            &relations::load(field("a")?)?.image,
            &relations::load(field("b")?)?.image,
            request["least"].as_f64().unwrap_or(0.1)
        )?),
        _ => return Err("unsupported image probe".into()),
    };
    fs::write(&args[1], serde_json::to_vec(&output)?)?;
    Ok(())
}
