//! Explicit synthetic driver; production adapter calls run_files with installed resources.
use illustash_terms::index_compute::{self, numeric, Resources};
use serde_json::{json, Value};
use std::{fs, path::Path};
fn main() -> Result<(), Box<dyn std::error::Error>> {
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
        index_compute::run_files(
            input,
            output,
            &Resources { tags: Path::new(&tags), model: Path::new(&model), runtime: Path::new(&runtime) },
        )?;
    }
    Ok(())
}
