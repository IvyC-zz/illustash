//! IQDB signature queue: picks and orders the pictures to ask about, then runs the
//! local `illustash-iqdb --hash-only` signature program. Only signatures leave
//! this worker; the App performs the authorized Danbooru IQDB queries.
use super::{check_owner, groups, pixel_batch, write, Request};
use crate::{app_provenance::cache, Error, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashSet},
    fs,
    io::{BufRead, BufReader},
    path::PathBuf,
    process::{Command, Stdio},
};

const BACKEND: &str = "iqdb";
const PER_RUN: usize = 10_000;
const PER_SAVE_RUN: usize = 3;
const SAVE_GAP: f64 = 300.;
// Frozen yield order (provenance/sources/iqdb.py PRIORITY); unknown pairs rank 5.5.
const PRIORITY: [(&str, &str); 8] = [
    ("x", "questionable"),
    ("x", "sensitive"),
    ("pixiv", "general"),
    ("x", "explicit"),
    ("pixiv", "sensitive"),
    ("x", "general"),
    ("pixiv", "questionable"),
    ("pixiv", "explicit"),
];

#[derive(Clone)]
struct Picture {
    asset: String,
    preset: String,
    source_id: String,
    src_path: Option<String>,
    added_at: Option<String>,
}

fn pictures(request: &Request, presets: &[&str]) -> Result<Vec<Picture>> {
    let source = crate::app_library::read_connection(&request.library.join("source.sqlite"))?;
    let marks = vec!["?"; presets.len()].join(",");
    let rows = source
        .prepare(&format!(
            "SELECT uuid,preset,source_id,src_path FROM assets WHERE in_scope=1 AND coalesce(trashed,0)=0 AND preset IN ({marks})"
        ))?
        .query_map(rusqlite::params_from_iter(presets), |r| {
            Ok(Picture {
                asset: r.get(0)?,
                preset: r.get(1)?,
                source_id: r.get(2)?,
                src_path: r.get(3)?,
                added_at: None,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

fn owned(c: &Connection) -> Result<HashSet<String>> {
    Ok(c.prepare("SELECT DISTINCT asset FROM image_artists WHERE status IN ('auto','confirmed')")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

fn due(c: &Connection, asset: &str) -> Result<bool> {
    let row: Option<bool> = c
        .query_row(
            "SELECT next_due_at IS NULL OR next_due_at<=datetime('now') FROM lookups WHERE target=? AND backend=?",
            params![asset, BACKEND],
            |r| r.get(0),
        )
        .optional()?;
    Ok(row.unwrap_or(true))
}

/// The core's existing ratings, read-only; a library without them keeps the old order.
fn ratings(request: &Request) -> BTreeMap<String, Option<String>> {
    let path = request.library.join("library.sqlite");
    if !path.is_file() {
        return BTreeMap::new();
    }
    let read = || -> Result<BTreeMap<String, Option<String>>> {
        let c = crate::app_library::read_connection(&path)?;
        let rows = c
            .prepare("SELECT uuid,rating FROM asset_rating")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    };
    read().unwrap_or_default()
}

/// A permutation of the queue: four yield-prioritised slots, then one original-order slot.
fn prioritize(pictures: Vec<Picture>, ratings: &BTreeMap<String, Option<String>>) -> Vec<Picture> {
    let rank = |p: &Picture| {
        let rating = ratings.get(&p.asset).cloned().flatten();
        PRIORITY
            .iter()
            .position(|(preset, r)| *preset == p.preset && Some(*r) == rating.as_deref())
            .map_or(5.5, |i| i as f64)
    };
    let mut ranked = pictures.clone();
    ranked.sort_by(|a, b| rank(a).total_cmp(&rank(b)));
    let (mut at_ranked, mut at_original) = (0, 0);
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(pictures.len());
    for index in 0..pictures.len() {
        let (queue, at) = if index % 5 == 4 { (&pictures, &mut at_original) } else { (&ranked, &mut at_ranked) };
        while seen.contains(&queue[*at].asset) {
            *at += 1;
        }
        seen.insert(queue[*at].asset.clone());
        out.push(queue[*at].clone());
        *at += 1;
    }
    out
}

/// Pictures in runs of saves no more than SAVE_GAP apart, oldest first.
fn save_runs(pictures: &[Picture]) -> Result<Vec<Vec<Picture>>> {
    let mut dated: Vec<_> = pictures.iter().filter(|p| p.added_at.as_deref().is_some_and(|a| !a.is_empty())).collect();
    // Python sorts the stored strings, stably, before converting them to timestamps.
    dated.sort_by(|a, b| a.added_at.cmp(&b.added_at));
    let (mut runs, mut last): (Vec<Vec<Picture>>, Option<f64>) = (Vec::new(), None);
    for picture in dated {
        let time = groups::timestamp(picture.added_at.as_deref().unwrap_or_default())?;
        if last.is_none_or(|previous| time - previous > SAVE_GAP) {
            runs.push(Vec::new());
        }
        if let Some(run) = runs.last_mut() {
            run.push(picture.clone());
        }
        last = Some(time);
    }
    Ok(runs)
}

/// Pictures to ask about. With `only` (按需查找): just those still without an artist,
/// in library order, even when a miss is not due yet.
fn order(request: &Request, c: &Connection) -> Result<Vec<Picture>> {
    let done = owned(c)?;
    if !request.only.is_empty() {
        let only: HashSet<_> = request.only.iter().collect();
        return Ok(pictures(request, &["x", "pixiv", "other"])?
            .into_iter()
            .filter(|p| only.contains(&p.asset) && !done.contains(&p.asset))
            .collect());
    }
    let mut todo = Vec::new();
    for picture in pictures(request, &["x", "pixiv"])? {
        if !done.contains(&picture.asset) && due(c, &picture.asset)? {
            todo.push(picture);
        }
    }
    let source = crate::app_library::read_connection(&request.library.join("source.sqlite"))?;
    let added: BTreeMap<String, Option<String>> = source
        .prepare("SELECT uuid,added_at FROM assets")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for picture in &mut todo {
        picture.added_at = added.get(&picture.asset).cloned().flatten();
    }
    // An original character is one artist's: the first answer names the whole character.
    let mut originals = HashSet::new();
    for (_, members) in &groups::library_groups(request)?[2] {
        if !members.iter().any(|u| done.contains(u)) {
            originals.extend(members.iter().take(3).cloned());
        }
    }
    let (lead, todo): (Vec<_>, Vec<_>) = todo.into_iter().partition(|p| originals.contains(&p.asset));
    let pixiv: Vec<_> = todo.iter().filter(|p| p.preset == "pixiv").cloned().collect();
    let mut first = Vec::new();
    for run in save_runs(&pixiv)? {
        let step = (run.len() / PER_SAVE_RUN).max(1);
        first.extend(run.into_iter().step_by(step).take(PER_SAVE_RUN));
    }
    let chosen: HashSet<_> = first.iter().map(|p| p.asset.clone()).collect();
    let x: Vec<_> = todo.iter().filter(|p| p.preset == "x").cloned().collect();
    let mut queue = lead;
    for (a, b) in first.iter().zip(&x) {
        queue.push(a.clone());
        queue.push(b.clone());
    }
    queue.extend(first.iter().skip(x.len()).cloned());
    queue.extend(x.iter().skip(first.len()).cloned());
    queue.extend(pixiv.into_iter().filter(|p| !chosen.contains(&p.asset)));
    Ok(prioritize(queue, &ratings(request)))
}

pub(super) fn signature_program() -> Option<PathBuf> {
    for name in ["ILLUSTASH_IQDB_BINARY", "ILLUSTASH_IQDB_BIN"] {
        if let Some(value) = std::env::var_os(name).filter(|v| !v.is_empty()) {
            return Some(PathBuf::from(value));
        }
    }
    // The installed adapter ships the program beside itself.
    let sibling = std::env::current_exe()
        .ok()?
        .with_file_name(illustash_platform::platform_process::executable_name("illustash-iqdb"));
    sibling.is_file().then_some(sibling)
}

pub(super) fn run(request: &Request, sequence: &mut u64) -> Result<Value> {
    let c = cache::connect(&request.cache.join("provenance.sqlite"))?;
    let mut todo = order(request, &c)?;
    todo.truncate(PER_RUN);
    drop(c);
    let Some(program) = signature_program() else {
        return Ok(json!({"failed": 1, "stopped": "未配置本地 IQDB 签名计算程序"}));
    };
    let config = crate::strict_json::parse(&fs::read_to_string(request.library.join("library.json"))?)
        .map_err(Error::Invalid)?;
    let mut lines = String::new();
    for p in &todo {
        let original = pixel_batch::original(&config, &p.asset, &p.source_id, p.src_path.as_deref())?;
        // Python: str(original or src_path), so a missing path is the text "None".
        let path = match original {
            Some(path) => path.to_string_lossy().into_owned(),
            None => p.src_path.clone().unwrap_or_else(|| "None".into()),
        };
        lines.push_str(&json!({"asset": p.asset, "path": path}).to_string());
        lines.push('\n');
    }
    // An input file avoids a pipe deadlock while stdout fills; streamed output keeps progress alive.
    let input = request.mailbox.join("hash-input.jsonl");
    fs::write(&input, lines)?;
    check_owner(request.owner_pid)?;
    let mut child = Command::new(&program)
        .args(["--hash-only", "1"])
        .stdin(fs::File::open(&input)?)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| Error::Invalid(format!("可选 IQDB 程序不可用：{e}")))?;
    let stdout = child.stdout.take().ok_or_else(|| Error::Invalid("IQDB 签名输出不可用".into()))?;
    let mut rows = Vec::new();
    for line in BufReader::new(stdout).lines() {
        let line = line?;
        rows.push(crate::strict_json::parse(&line).map_err(Error::Invalid)?);
        *sequence += 1;
        write(
            &request.mailbox.join("progress.json"),
            &json!({"progress":{"done":rows.len(),"total":todo.len()},"sequence":*sequence}),
        )?;
    }
    if !child.wait()?.success() {
        return Err(Error::Invalid("IQDB signature calculation failed".into()));
    }
    check_owner(request.owner_pid)?;
    write(&request.mailbox.join("hashes.json"), &json!(rows))?;
    Ok(json!({"hashes":"hashes.json"}))
}
