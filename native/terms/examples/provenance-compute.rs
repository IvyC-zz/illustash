//! Explicit synthetic probe; the installed adapter dispatches through provenance_compute::run_files.
use illustash_terms::provenance_compute::{self, pixel_hash};
use serde_json::json;
use std::{fs, path::Path};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    let input = Path::new(args.get(1).ok_or("request missing")?);
    let output = Path::new(args.get(2).ok_or("response missing")?);
    let request = illustash_terms::strict_json::parse(&fs::read_to_string(input)?)?;
    let result = match request["op"].as_str() {
        Some("lcms_version") => json!(pixel_hash::lcms_version()?),
        Some("rgba") => {
            let pixels: Vec<u8> = serde_json::from_value(request["pixels"].clone())?;
            json!(pixel_hash::rgba_hash(
                serde_json::from_value(request["width"].clone())?,
                serde_json::from_value(request["height"].clone())?,
                &pixels
            )?)
        }
        Some("hash") | Some("hash_bytes") => {
            let path = Path::new(request["path"].as_str().ok_or("path missing")?);
            json!(if request["op"] == "hash" {
                pixel_hash::candidate_path(path)?
            } else {
                pixel_hash::candidate_bytes(&fs::read(path)?)?
            })
        }
        _ => {
            provenance_compute::run_files(input, output)?;
            return Ok(());
        }
    };
    provenance_compute::write(output, &result)?;
    Ok(())
}
