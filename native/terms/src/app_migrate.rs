//! WD rating and topic labels from source.sqlite delivered to the core as evidence packages (folder sync and
//! recognition imports). The Python-era library migrations that lived here went with core v8 (#201): libraries from
//! before the switch to the core are upgraded by the `pre-v8` version.
use crate::{Error, Result};
use rusqlite::Connection;
use serde_json::{json, Value};
use std::{collections::BTreeSet, path::Path};
fn columns(c: &Connection, table: &str) -> Result<BTreeSet<String>> {
    Ok(c.prepare(&format!("PRAGMA table_info({table})"))?
        .query_map([], |r| r.get(1))?
        .collect::<rusqlite::Result<_>>()?)
}
pub(crate) fn deliver_wd(
    c: &mut Connection,
    source: &Path,
    id: &str,
    assets: Option<&[String]>,
    new_assets: bool,
) -> Result<()> {
    let all = if let Some(assets) = assets {
        assets.to_vec()
    } else {
        c.prepare("SELECT uuid FROM assets ORDER BY uuid")?
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    let all = if crate::store::labels_in_core(c)? {
        crate::wd_pending::pending(c, source, &all, &json!(id))?["pending"]
            .as_array()
            .expect("validated internal JSON array")
            .iter()
            .map(|v| v.as_str().expect("validated internal text field").to_owned())
            .collect()
    } else {
        all
    };
    let src = crate::app_library::read_connection(source)?;
    let have = columns(&src, "labels")?;
    let method = if have.contains("source") { "source" } else { "'wd'" };
    let version = if have.contains("model_ver") { "model_ver" } else { "NULL" };
    let ts = if have.contains("ts") { "ts" } else { "NULL" };
    let now: String = src.query_row("SELECT strftime('%Y-%m-%dT%H:%M:%SZ','now')", [], |r| r.get(0))?;
    for chunk in all.chunks(1000) {
        let mut q=src.prepare(&format!("SELECT uuid,dim,value,conf,{method} AS source,{version},{ts} FROM labels WHERE uuid IN (SELECT value FROM json_each(?)) AND (+dim='rating' OR (+dim='general' AND +conf>=0.2)) ORDER BY uuid,dim,value,source"))?;
        let mut rows = q.query([serde_json::to_string(chunk).map_err(|e| Error::Invalid(e.to_string()))?])?;
        let mut evidence = String::new();
        let mut assets = Vec::new();
        let mut last = String::new();
        while let Some(r) = rows.next()? {
            let Some(conf) = r.get::<_, Option<f64>>(3)? else { continue };
            let (uuid, dim, value, method): (String, String, String, String) =
                (r.get(0)?, r.get(1)?, r.get(2)?, r.get(4)?);
            let version = r.get::<_, Option<String>>(5)?.filter(|v| !v.is_empty()).unwrap_or_else(|| "unknown".into());
            let ts = r.get::<_, Option<String>>(6)?;
            let observed = ts
                .and_then(|t| {
                    src.query_row("SELECT strftime('%Y-%m-%dT%H:%M:%SZ',?)", [t], |r| r.get::<_, Option<String>>(0))
                        .ok()
                        .flatten()
                })
                .unwrap_or_else(|| now.clone());
            let (kind, system) = if dim == "general" { ("topic", "wd") } else { ("rating", "wd_rating") };
            let j = |v: &str| serde_json::to_string(v).expect("JSON text");
            let score = crate::character_json::dump(&Value::from(conf), false);
            evidence.push_str(&format!("{{\"id\":{},\"asset\":{{\"namespace\":\"library\",\"id\":{}}},\"term\":{{\"kind\":{},\"link\":{{\"system\":{},\"key\":{}}}}},\"source\":\"wd\",\"method\":{},\"version\":{},\"score\":{{\"value\":{},\"metric\":\"wd_probability\"}},\"observed_at\":{},\"data\":{{}}}}\n",j(&format!("{uuid}:{dim}:{value}:{method}")),j(&uuid),j(kind),j(system),j(&value),j(&method),j(&version),score,j(&observed)));
            if last != uuid {
                assets.push(json!({"asset":{"namespace":"library","id":uuid},"status":"completed","error":null}));
                last = uuid;
            }
        }
        if assets.is_empty() {
            continue;
        }
        let manifest = json!({"format":"illustash-evidence/1","library_id":id,"producer":{"name":"wd","version":"labels"},"created_at":now,"assets":assets,"files":{"evidence.jsonl":{"bytes":evidence.len(),"sha256":crate::strict_json::sha256_hex(evidence.as_bytes())}}});
        let pkg = crate::package::Package::from_parts(&manifest.to_string(), &evidence)?;
        crate::store::import_package_with(c, &pkg, id, new_assets)?;
    }
    Ok(())
}
