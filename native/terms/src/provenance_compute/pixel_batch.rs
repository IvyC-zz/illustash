//! Legacy pixel-hash worker queue. Only hashes leave this local candidate.
use super::{check_owner, pixel_hash, write, Request};
use crate::{Error, Result};
use rusqlite::params;
use serde_json::{json, Value};
use std::{collections::BTreeSet, fs, path::PathBuf};

pub(super) fn run(request: &Request, sequence: &mut u64) -> Result<Value> {
    let c = super::staged_cache(&request.cache)?;
    let done = c
        .prepare("SELECT target FROM lookups WHERE backend='pixel_hash' AND last_result IN ('hit','miss')")?
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<BTreeSet<_>>>()?;
    let source = crate::app_library::read_connection(&request.library.join("source.sqlite"))?;
    let config = crate::strict_json::parse(&fs::read_to_string(request.library.join("library.json"))?)
        .map_err(Error::Invalid)?;
    let rows = source
        .prepare(
            "SELECT uuid,source_id,src_path FROM assets WHERE in_scope=1 AND coalesce(trashed,0)=0 AND preset IN (?,?)",
        )?
        .query_map(params!["x", "pixiv"], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<String>>(2)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let todo: Vec<_> = rows
        .into_iter()
        .filter(|(asset, _, _)| !done.contains(asset) && (request.only.is_empty() || request.only.contains(asset)))
        .collect();
    let mut hashes = Vec::new();
    for (asset, source_id, path) in &todo {
        check_owner(request.owner_pid)?;
        let original = original(&config, asset, source_id, path.as_deref())?;
        // A missing file yields null, but Pillow Image.open(None) raises uncaught
        // AttributeError in the old adapter: absence of an original aborts the batch.
        let original = original.ok_or_else(|| Error::Invalid("像素哈希缺少原始文件路径".into()))?;
        let digest = pixel_hash::candidate_path(&original).ok();
        hashes.push(json!({"asset":asset,"hash":digest}));
        *sequence += 1;
        write(
            &request.mailbox.join("progress.json"),
            &json!({"progress":{"done":hashes.len(),"total":todo.len()},"sequence":*sequence}),
        )?;
    }
    check_owner(request.owner_pid)?;
    write(&request.mailbox.join("hashes.json"), &json!(hashes))?;
    Ok(json!({"hashes":"hashes.json"}))
}

pub(super) fn original(config: &Value, asset: &str, source_id: &str, path: Option<&str>) -> Result<Option<PathBuf>> {
    let sources = config["sources"].as_array().ok_or_else(|| Error::Invalid("图库 sources 无效".into()))?;
    let source = sources.iter().find(|s| s["id"].as_str() == Some(source_id));
    if let Some(source) = source.filter(|s| s["kind"] == "photos") {
        let root = source["path"].as_str().ok_or_else(|| Error::Invalid("Photos 原始目录无效".into()))?;
        let root = if root == "~" || root.starts_with("~/") {
            let home = std::env::var_os("HOME").ok_or_else(|| Error::Invalid("无法展开原始目录".into()))?;
            PathBuf::from(home).join(root.strip_prefix("~/").unwrap_or(""))
        } else {
            PathBuf::from(root)
        };
        let first = asset.chars().next().ok_or_else(|| Error::Invalid("资产身份为空".into()))?;
        let folder = root.join("originals").join(first.to_string());
        let mut matches = Vec::new();
        match fs::read_dir(folder) {
            Ok(entries) => {
                for entry in entries {
                    let entry = entry?;
                    if entry.file_name().to_string_lossy().starts_with(&format!("{asset}.")) {
                        matches.push(entry.path());
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        }
        matches.sort();
        Ok(matches.into_iter().next())
    } else {
        Ok(path.filter(|p| !p.is_empty()).map(PathBuf::from))
    }
}
