//! One-shot research calculator shared by learning (#151) and image relations (#152).
//! App Research keeps task identity, cancellation, fingerprint checks and formal publication.
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
pub mod features;
mod groups;
mod learn;
mod numeric;
mod relation_fft;
pub mod relation_image;
pub mod relations;
pub(crate) mod similar;
// Existing NumPy f32 contiguous reduction shared with native anchor inference.
pub(crate) use numeric::{sum32 as contiguous_sum32, sum64 as contiguous_sum64};
pub mod snapshot;
use crate::{Error, Result};
use serde_json::{json, Value};
use std::{fs, path::Path};

fn field<'a>(request: &'a Value, key: &str) -> Result<&'a str> {
    request[key].as_str().ok_or_else(|| Error::Invalid(format!("research request missing {key}")))
}
/// Installed readiness: on Windows the fixed OpenBLAS backend must load and match
/// its registered identity; macOS uses the system Accelerate framework.
pub fn check() -> Result<()> {
    #[cfg(target_os = "windows")]
    illustash_platform::platform_research_math::windows_backend_identity().map_err(Error::Invalid)?;
    Ok(())
}

/// Installed macOS/Windows routing uses one calculator for learning and image
/// relations, including a mixed startup batch. App still owns formal publication.
pub fn handles(request: &Value) -> bool {
    matches!(request["kind"].as_str(), Some("regroup" | "oc" | "similar" | "relate" | "events"))
}
fn progress(status: &str) -> Result<()> {
    progress_value(json!({"status":status}))
}
fn progress_value(value: Value) -> Result<()> {
    if let Some(path) = std::env::var_os("ILLUSTASH_RESEARCH_PROGRESS") {
        let path = Path::new(&path);
        let temporary = path.with_extension("tmp");
        fs::write(&temporary, serde_json::to_vec(&value).map_err(|e| Error::Invalid(e.to_string()))?)?;
        fs::rename(temporary, path)?;
    }
    Ok(())
}
/// The owner accepts similarity caches as change lists (#31); an owner that does not say so gets full tables.
fn delta_tables(request: &Value) -> bool {
    request["delta_tables"] == true
}
pub fn run(request: &Value) -> Result<Value> {
    if request["format"] != "illustash-research/1" || !handles(request) {
        return Err(Error::Invalid("unsupported native research request".into()));
    }
    // Require the fixed backend even for empty/no-op requests; missing installation
    // resources must fail in the existing task status, never select portable arithmetic.
    #[cfg(target_os = "windows")]
    illustash_platform::platform_research_math::windows_backend_identity().map_err(Error::Invalid)?;
    let snapshot = snapshot::Snapshot::open(Path::new(field(request, "snapshot")?))?;
    // Pixel comparison never reads CCIP features; an unrelated bad vector must not block it (#30).
    if request["kind"] == "relate" {
        let c = snapshot.read()?;
        let ids = request["args"]["ids"].as_array().ok_or_else(|| Error::Invalid("missing relation ids".into()))?;
        let mut seen = std::collections::HashSet::new();
        let mut images = vec![];
        let mut missing = vec![];
        for id in ids {
            let id = id.as_str().ok_or_else(|| Error::Invalid("invalid relation id".into()))?;
            if !seen.insert(id) {
                continue;
            }
            use rusqlite::OptionalExtension;
            let path: Option<Option<String>> =
                c.query_row("SELECT src_path FROM assets WHERE uuid=?", [id], |r| r.get(0)).optional()?;
            let image = match path.flatten().filter(|p| !p.is_empty()) {
                Some(path) => match relations::load(Path::new(&path)) {
                    Ok(image) => Some(image),
                    // Pillow thumbnail's I;16 reducing_gap raises ValueError;
                    // the old worker catches only decode/IO failures as missing.
                    Err(error) if relation_image::is_mode_error(&error) => return Err(error),
                    Err(_) => None,
                },
                None => None,
            };
            if let Some(image) = image {
                images.push((id.to_owned(), image));
            } else {
                missing.push(id);
            }
        }
        if images.len() < 2 {
            return Err(Error::Invalid("能读到的图不足两张：原图可能还在 iCloud".into()));
        }
        let mut result = relations::relate_all(&images)?;
        result["missing"] = json!(missing);
        return Ok(
            json!({"format":"illustash-research/1","identity":request["identity"],"version":request["version"],"result":result}),
        );
    }
    // Every remaining kind consumes CCIP features and still rejects any invalid vector.
    let features = &features::load(&snapshot.source, Path::new(field(request, "features")?))?;
    let mut out = json!({"format":"illustash-research/1","identity":request["identity"],"version":request["version"]});
    if request["kind"] == "oc" {
        out["result"] = groups::oc(&snapshot, features, field(&request["args"], "entity")?)?;
        return Ok(out);
    }
    if request["kind"] == "similar" {
        let (result, tables) = similar::scan(&snapshot, features, delta_tables(request))?;
        out["result"] = result;
        out["tables"] = tables;
        return Ok(out);
    }
    progress("learning")?;
    let anchors = learn::patch_anchors(&snapshot, Path::new(field(request, "anchors")?))?;
    let (mut result, learning) =
        learn::learn(&snapshot, features, &request["learning_token"], request["regroup"] == true, &anchors)?;
    if let Some(batch) = learning {
        out["learning"] = batch;
    }
    let mut tables = json!({});
    if request["regroup"] == true {
        if result["status"] == "stale" {
            return Err(Error::Conflict("学习输入已改变".into()));
        }
        progress("grouping")?;
        let (count, cache) = groups::cluster(&snapshot, features)?;
        result["clusters"] = json!(count);
        tables = cache;
    }
    if request["kind"] == "events" && request["regroup"] == true && request["args"]["similar"] == true {
        let (state, cache) = similar::scan(&snapshot, features, delta_tables(request))?;
        result
            .as_object_mut()
            .expect("learning result object")
            .extend(state.as_object().expect("scan result object").clone());
        tables.as_object_mut().expect("cache object").extend(cache.as_object().expect("similar cache object").clone());
    }
    out["result"] = result;
    out["tables"] = tables;
    Ok(out)
}
/// The installed adapter only owns transport; it calls this dispatcher once for one kind.
pub fn run_files(input: &Path, output: &Path) -> Result<()> {
    let text = fs::read_to_string(input)?;
    let request = crate::strict_json::parse(&text).map_err(|e| Error::Invalid(e.to_string()))?;
    let response = run(&request)?;
    let bytes = if request["kind"] == "relate" {
        let result = relations::wire_result(&response["result"], &request["args"]["ids"])?;
        let mut ordered = crate::http_json::Json::from(response);
        ordered.set("result", result);
        ordered.dump().into_bytes()
    } else {
        response_bytes(request["kind"].as_str(), response)?
    };
    let temporary = output.with_extension("tmp");
    fs::write(&temporary, bytes)?;
    fs::rename(temporary, output)?;
    Ok(())
}
fn response_bytes(kind: Option<&str>, response: Value) -> Result<Vec<u8>> {
    if kind == Some("oc") {
        // App forwards this result to HTTP with insertion order preserved. Keep the
        // legacy {total, groups} order and existing HTTP formatter; no stored hash changes.
        let result = crate::http_json::Json::object([
            ("total", response["result"]["total"].clone().into()),
            ("groups", response["result"]["groups"].clone().into()),
        ]);
        let mut ordered = crate::http_json::Json::from(response);
        ordered.set("result", result);
        Ok(ordered.dump().into_bytes())
    } else {
        serde_json::to_vec(&response).map_err(|e| Error::Invalid(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn oc_wire_result_keeps_legacy_http_order() {
        let bytes = response_bytes(Some("oc"), json!({"result":{"total":3,"groups":[["a","b"]]}})).unwrap();
        let ordered = crate::http_json::parse_python(std::str::from_utf8(&bytes).unwrap()).unwrap();
        assert_eq!(ordered.get("result").unwrap().dump(), r#"{"total": 3, "groups": [["a", "b"]]}"#);
    }
    #[test]
    fn relate_ignores_unrelated_bad_ccip_vector_but_feature_kinds_reject_it() {
        // #30: synthetic images and features only; relate is a pixel comparison.
        let root = std::env::temp_dir().join(format!("illustash-relate-test-{}", crate::store::new_key().unwrap()));
        let features = root.join("features");
        fs::create_dir_all(&features).unwrap();
        let library = crate::sqlite::open(root.join("library.sqlite")).unwrap();
        library.execute_batch("CREATE TABLE assets(uuid TEXT PRIMARY KEY, src_path TEXT);").unwrap();
        for (id, edited) in [("A", false), ("B", true)] {
            let path = root.join(format!("{id}.png"));
            image::RgbImage::from_fn(96, 64, |x, y| {
                let mark = edited && (30..50).contains(&x) && (20..40).contains(&y);
                image::Rgb(if mark { [250, 20, 20] } else { [(x * 2) as u8, (y * 3) as u8, ((x + y) % 256) as u8] })
            })
            .save(&path)
            .unwrap();
            library.execute("INSERT INTO assets VALUES (?,?)", [id, path.to_str().unwrap()]).unwrap();
        }
        let source = crate::sqlite::open(root.join("source.sqlite")).unwrap();
        source.execute_batch("CREATE TABLE embeddings(uuid,model,vec,PRIMARY KEY(uuid,model));").unwrap();
        let request = |kind: &str| {
            json!({"format":"illustash-research/1","identity":{"synthetic":true},"version":"v","kind":kind,
                "args":{"ids":["A","B","C"]},"snapshot":root.to_str().unwrap(),"features":features.to_str().unwrap()})
        };
        let before = run(&request("relate")).unwrap();
        assert_eq!(before["result"]["missing"], json!(["C"]));
        assert_eq!(before["result"]["sizes"]["A"], json!({"width":96,"height":64}));
        // Feature kinds load CCIP first; this minimal snapshot has no full library schema.
        assert!(!run(&request("similar")).unwrap_err().to_string().contains("CCIP"));
        // An unrelated stored CCIP vector is invalid (NaN), as is a finite one whose norm overflows.
        for bad in [features::bytes(&[f32::NAN; 768]), features::bytes(&[1e30; 768])] {
            source.execute("INSERT OR REPLACE INTO embeddings VALUES ('UNRELATED','ccip',?)", [bad]).unwrap();
            assert_eq!(run(&request("relate")).unwrap(), before);
            for kind in ["similar", "oc"] {
                let error = run(&request(kind)).unwrap_err().to_string();
                assert!(error.contains("Invalid CCIP feature vector"), "{kind}: {error}");
            }
        }
        drop((library, source));
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn routing_never_runs_both_calculators_for_an_events_task() {
        for regroup in [false, true] {
            for similar in [false, true] {
                assert!(handles(&json!({"kind":"events","regroup":regroup,"args":{"similar":similar}})));
            }
        }
        for kind in ["regroup", "oc", "similar", "relate"] {
            assert!(handles(&json!({"kind":kind})));
        }
        assert!(!handles(&json!({"kind":"unknown"})));
    }
}
