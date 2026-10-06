use super::*;
use crate::recognition_compute::anchors::{self, Metadata};
use std::collections::{HashMap, HashSet};
pub(super) fn scan(
    review: &Path,
    vectors: &[Vec<f32>],
    ids: &[String],
    meta: &HashMap<String, Metadata>,
    patched: &HashSet<String>,
) -> Result<Vec<(String, String, f64)>> {
    let fp = review.join("full-features.npy");
    let manifest = review.join("full-manifest.csv");
    if !fp.try_exists()? {
        save(&fp, &[])?;
        fs::write(&manifest, "uuid\n")?;
    }
    let mut features = matrix(&fp)?;
    let mut rdr = csv::Reader::from_path(&manifest).map_err(|e| invalid(e.to_string()))?;
    let uuid = rdr
        .headers()
        .map_err(|e| invalid(e.to_string()))?
        .iter()
        .position(|s| s == "uuid")
        .ok_or_else(|| invalid("manifest uuid缺失"))?;
    let mut assets = rdr
        .records()
        .map(|r| {
            let r = r.map_err(|e| invalid(e.to_string()))?;
            Ok(r.get(uuid).ok_or_else(|| invalid("manifest uuid缺失"))?.to_owned())
        })
        .collect::<Result<Vec<_>>>()?;
    if assets.len() != features.len() {
        return Err(invalid("索引特征与manifest长度不匹配"));
    }
    let have: HashSet<_> = assets.iter().map(|s| s.trim().to_uppercase()).collect();
    let db = crate::sqlite::open_with_flags(review.join("pi.db"), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let exists: bool =
        db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='embeddings')", [], |r| {
            r.get(0)
        })?;
    if exists {
        let mut stmt = db.prepare("SELECT uuid,vec FROM embeddings WHERE model='ccip'")?;
        let extra = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?)))?;
        for row in extra {
            let (id, blob) = row?;
            if have.contains(&id.to_uppercase()) {
                continue;
            }
            if blob.len() != DIM * 4 {
                return Err(invalid("索引CCIP向量维数不正确"));
            }
            let values: Vec<_> =
                blob.chunks_exact(4).map(|b| f32::from_le_bytes(b.try_into().expect("four bytes"))).collect();
            if values.iter().any(|v| !v.is_finite()) {
                return Err(invalid("索引CCIP向量必须有限"));
            }
            features.push(values);
            assets.push(id);
        }
    }
    let mut csv = csv::WriterBuilder::new()
        .terminator(csv::Terminator::CRLF)
        .from_path(review.join("char-scan.csv"))
        .map_err(|e| invalid(e.to_string()))?;
    csv.write_record(["uuid", "char_id", "entity", "dist", "thresh", "tier", "rank", "within"])
        .map_err(|e| invalid(e.to_string()))?;
    if vectors.is_empty() || features.is_empty() {
        csv.flush()?;
        return Ok(vec![]);
    }
    let index = anchors::anchor_index_for_scan(vectors, ids, meta)?;
    let limit = anchors::claim_thresholds(ids, &index, patched)?;
    let mut labels = vec![];
    let mut positions = HashMap::new();
    // Keep the existing 4000-column scan shape: BLAS shape can alter boundary decisions.
    for start in (0..features.len()).step_by(4000) {
        let d = anchors::cross(vectors, &features[start..(start + 4000).min(features.len())])?;
        for j in 0..features[start..(start + 4000).min(features.len())].len() {
            let col: Vec<_> = d.iter().map(|r| r[j]).collect();
            let asset = assets[start + j].trim();
            let mut seen = HashSet::new();
            let mut rank = 0;
            for a in numeric::nearest_nine(&col) {
                let entity = &index.entities[a];
                if !seen.insert(entity) {
                    continue;
                }
                let distance = col[a];
                let ok = limit[a] > 0. && distance <= limit[a];
                let tier = if !ok {
                    "-"
                } else if distance <= index.p50[a] {
                    "p50"
                } else if distance <= index.p75[a] {
                    "p75"
                } else {
                    "p90"
                };
                let rounded = format!("{:.4}", f64::from(distance));
                csv.write_record([
                    asset,
                    &ids[a],
                    entity,
                    &rounded,
                    &format!("{:.4}", f64::from(limit[a])),
                    tier,
                    &rank.to_string(),
                    if ok { "1" } else { "0" },
                ])
                .map_err(|e| invalid(e.to_string()))?;
                rank += 1;
                if ok {
                    let confidence = 1. - rounded.parse::<f64>().map_err(|e| invalid(e.to_string()))?;
                    let row = (asset.to_owned(), entity.clone(), confidence);
                    let key = (asset.to_owned(), entity.clone());
                    if let Some(&i) = positions.get(&key) {
                        labels[i] = row;
                    } else {
                        positions.insert(key, labels.len());
                        labels.push(row);
                    }
                }
            }
        }
    }
    csv.flush()?;
    Ok(labels)
}
