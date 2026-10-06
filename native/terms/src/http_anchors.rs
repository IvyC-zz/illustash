//! Read-only identity bank validation for stored anchor previews, without NumPy or a worker.
use crate::{http_json::Json, http_read::rows, Result};
use rusqlite::{Connection, OptionalExtension};
use sha2::{Digest, Sha256};
use std::{collections::HashMap, path::Path};
fn hash(bytes: impl AsRef<[u8]>) -> String {
    format!("{:x}", Sha256::digest(bytes.as_ref()))
}
fn invalid() -> crate::Error {
    crate::Error::Invalid("锚点数据不合法".into())
}
pub(crate) fn anchors(c: &Connection) -> Result<HashMap<String, Vec<Json>>> {
    let path: Option<String> =
        c.query_row("SELECT value FROM meta WHERE key='identity_bank'", [], |r| r.get(0)).optional()?;
    let Some(path) = path else {
        return Ok(HashMap::new());
    };
    // Like Store.identity_pairs: an unreadable/stale/invalid bank omits previews.
    match load(c, Path::new(&path)) {
        Ok(result) => Ok(result),
        Err(crate::Error::Io(_) | crate::Error::Invalid(_)) => Ok(HashMap::new()),
        Err(e) => Err(e),
    }
}
fn load(c: &Connection, path: &Path) -> Result<HashMap<String, Vec<Json>>> {
    let raw = std::fs::read_to_string(path.join("bank.json"))?;
    let meta = crate::http_json::parse_python(&raw)?;
    let field = |k: &str| meta.get(k).ok_or_else(invalid);
    let mut context = Vec::new();
    for sql in [
        "SELECT id,parent_id,merged_into FROM entities WHERE kind<>'work' ORDER BY id",
        "SELECT key,entity_id FROM links WHERE system='danbooru' ORDER BY key,entity_id",
        "SELECT entity,reference FROM identity_anchor_decisions WHERE decision='exclude' ORDER BY entity,reference",
    ] {
        context.push(Json::Array(
            rows(c, sql, &[])?
                .into_iter()
                .map(|r| match r {
                    Json::Object(fields) => Json::Array(fields.into_iter().map(|(_, v)| v).collect()),
                    _ => unreachable!(),
                })
                .collect(),
        ));
    }
    let context = hash(crate::character_json::dump(
        &serde_json::from_str(&Json::Array(context).dump()).map_err(|_| invalid())?,
        true,
    ));
    if field("context")?.text() != context {
        return Err(invalid());
    }
    let bytes = std::fs::read(path.join("vectors.npy"))?;
    if hash(&bytes) != field("vectors_sha256")?.text() {
        return Err(invalid());
    }
    let Some(Json::Array(prototypes)) = meta.get("prototypes") else {
        return Err(invalid());
    };
    let sorted: serde_json::Value = serde_json::from_str(&field("prototypes")?.dump()).map_err(|_| invalid())?;
    let expected = hash(format!(
        "{}{}{}{}",
        field("vectors_sha256")?.text(),
        field("reference_vectors_sha256")?.text(),
        context,
        crate::character_json::dump(&sorted, true)
    ));
    if expected != field("version")?.text() || !valid_vectors(&bytes, prototypes.len()) {
        return Err(invalid());
    }
    let canonical: HashMap<String, String> = c
        .prepare("SELECT id,canonical_id FROM identity_nodes")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut out: HashMap<String, Vec<Json>> = HashMap::new();
    for p in prototypes {
        let id = p.get("entity").ok_or_else(invalid)?.text();
        let owner = canonical.get(id).ok_or_else(invalid)?;
        out.entry(owner.clone()).or_default().push(p.clone());
    }
    Ok(out)
}
fn valid_vectors(bytes: &[u8], count: usize) -> bool {
    if bytes.get(..6) != Some(b"\x93NUMPY") {
        return false;
    }
    let (start, n) = match bytes.get(6..8) {
        Some([1, 0]) if bytes.len() >= 10 => (10, u16::from_le_bytes([bytes[8], bytes[9]]) as usize),
        Some([2 | 3, 0]) if bytes.len() >= 12 => {
            (12, u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]) as usize)
        }
        _ => return false,
    };
    let Some(header) = bytes.get(start..start + n).and_then(|v| std::str::from_utf8(v).ok()) else {
        return false;
    };
    // NumPy writes a Python literal dict. Accept its standard quoted dtype and shape keys.
    let shape = header
        .split("shape")
        .nth(1)
        .and_then(|s| s.split_once('('))
        .and_then(|(_, s)| s.split_once(')'))
        .map(|(s, _)| s.split(',').filter_map(|n| n.trim().parse::<usize>().ok()).collect::<Vec<_>>());
    if shape.as_deref() != Some(&[count, 768]) {
        return false;
    }
    let fortran = header
        .split("fortran_order")
        .nth(1)
        .and_then(|s| s.split_once(':'))
        .is_some_and(|(_, s)| s.trim_start().starts_with("True"));
    let (width, big) = if header.contains("<f4") || header.contains("=f4") || header.contains("'f4'") {
        (4, false)
    } else if header.contains(">f4") {
        (4, true)
    } else if header.contains("<f8") || header.contains("=f8") || header.contains("'f8'") {
        (8, false)
    } else if header.contains(">f8") {
        (8, true)
    } else {
        return false;
    };
    let Some(data) = bytes.get(start + n..) else {
        return false;
    };
    if count.checked_mul(768).and_then(|n| n.checked_mul(width)) != Some(data.len()) {
        return false;
    }
    let values: Vec<f64> = data
        .chunks_exact(width)
        .map(|b| {
            if width == 4 {
                let a = b.try_into().expect("http_anchors internal invariant");
                (if big { f32::from_be_bytes(a) } else { f32::from_le_bytes(a) }) as f64
            } else {
                let a = b.try_into().expect("http_anchors internal invariant");
                if big {
                    f64::from_be_bytes(a)
                } else {
                    f64::from_le_bytes(a)
                }
            }
        })
        .collect();
    if values.iter().any(|v| !v.is_finite()) {
        return false;
    }
    (0..count).all(|row| {
        (0..768).map(|col| values[if fortran { col * count + row } else { row * 768 + col }].powi(2)).sum::<f64>()
            > 1e-24
    })
}
