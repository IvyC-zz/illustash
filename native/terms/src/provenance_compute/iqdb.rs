//! IQDB 签名队列：挑出要查询的图片并排好顺序，再运行本地签名程序 `illustash-iqdb --hash-only`。
//! 离开这个 worker 的只有签名；经授权的 Danbooru IQDB 查询由 App 执行。
use super::{check_owner, groups, pixel_batch, write, Request};
use crate::{Error, Result};
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
// 固定的命中率顺序（provenance/sources/iqdb.py 的 PRIORITY）；未列出的组合排 5.5。
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

/// core 里已有的分级，只读；没有分级的图库保持原来的顺序。
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

/// 队列的一个重排：四个按命中率优先的位置，接一个按原顺序的位置。
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

/// 按保存时间分段的图片，同一段内相邻两次保存间隔不超过 SAVE_GAP，最早的在前。
fn save_runs(pictures: &[Picture]) -> Result<Vec<Vec<Picture>>> {
    let mut dated: Vec<_> = pictures.iter().filter(|p| p.added_at.as_deref().is_some_and(|a| !a.is_empty())).collect();
    // 沿用旧实现：先对存储的字符串做稳定排序，再转换成时间戳。
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

/// 要查询的图片。给了 `only`（按需查找）时：只取其中仍没有画师的，按图库顺序，即使未命中的重查时间还没到。
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
    // 原创角色只属于一位画师：第一个答案就确定了整个角色。
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
    // 安装版适配器把这个程序放在自己旁边。
    let sibling = std::env::current_exe()
        .ok()?
        .with_file_name(illustash_platform::platform_process::executable_name("illustash-iqdb"));
    sibling.is_file().then_some(sibling)
}

pub(super) fn run(request: &Request, sequence: &mut u64) -> Result<Value> {
    let c = super::staged_cache(&request.cache)?;
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
        // 缺失的路径照旧是文本 "None"（旧实现是 str(original or src_path)）。
        let path = match original {
            Some(path) => path.to_string_lossy().into_owned(),
            None => p.src_path.clone().unwrap_or_else(|| "None".into()),
        };
        lines.push_str(&json!({"asset": p.asset, "path": path}).to_string());
        lines.push('\n');
    }
    // 用输入文件，避免 stdout 写满时管道死锁；流式输出让进度持续更新。
    let input = request.mailbox.join("hash-input.jsonl");
    fs::write(&input, lines)?;
    check_owner(request.owner_pid)?;
    let mut child = Command::new(&program)
        .args(["--hash-only", "1"])
        .stdin(fs::File::open(&input)?)
        .stdout(Stdio::piped())
        // 告警经溯源 worker 的 stderr 交给宿主做脱敏诊断。
        .stderr(Stdio::inherit())
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

#[cfg(test)]
mod tests {
    use super::*;
    fn pic(asset: &str, preset: &str) -> Picture {
        Picture { asset: asset.into(), preset: preset.into(), source_id: "s1".into(), src_path: None, added_at: None }
    }
    fn ids(rows: Vec<Picture>) -> Vec<String> {
        rows.into_iter().map(|p| p.asset).collect()
    }
    struct TestRoot(PathBuf);
    impl TestRoot {
        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }
    impl Drop for TestRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn fixture() -> (TestRoot, Request, Connection) {
        let root = TestRoot(
            std::env::temp_dir().join(format!("illustash-iqdb-contract-{}", crate::store::new_key().unwrap())),
        );
        fs::create_dir(&root.0).unwrap();
        let request = Request {
            format: "illustash-provenance/1".into(),
            identity: json!({"synthetic":true}),
            owner_pid: std::process::id(),
            library: root.path().join("library"),
            cache: root.path().join("cache"),
            mailbox: root.path().join("mailbox"),
            step: "iqdb".into(),
            only: vec![],
            artists: vec![],
            network: true,
        };
        for path in [&request.library, &request.cache, &request.mailbox] {
            fs::create_dir(path).unwrap();
        }
        let source = Connection::open(request.library.join("source.sqlite")).unwrap();
        source.execute_batch("CREATE TABLE assets(uuid,preset,source_id,src_path,in_scope,trashed,added_at);").unwrap();
        let core = Connection::open(request.library.join("library.sqlite")).unwrap();
        core.execute_batch("CREATE TABLE duplicates(grp,uuid); CREATE TABLE similar_groups(gid,uuid); CREATE TABLE entities(id,original,label_only); CREATE TABLE memberships(entity,uuid); CREATE TABLE clusters(cid,uuid);").unwrap();
        let cache = crate::app_provenance::cache::connect(&request.cache.join("provenance.sqlite")).unwrap();
        (root, request, cache)
    }
    fn insert(request: &Request, asset: &str, preset: &str, added: &str) {
        let source = Connection::open(request.library.join("source.sqlite")).unwrap();
        source.execute("INSERT INTO assets VALUES (?,?, 's1',NULL,1,0,?)", params![asset, preset, added]).unwrap();
    }
    #[test]
    fn priority_reserves_every_fifth_original_slot() {
        let low = (0..5).map(|i| pic(&format!("low{i}"), "pixiv"));
        let high = (0..10).map(|i| pic(&format!("high{i}"), "x"));
        let pictures: Vec<_> = low.chain(high).collect();
        let ratings = pictures
            .iter()
            .map(|p| (p.asset.clone(), Some(if p.preset == "pixiv" { "explicit" } else { "sensitive" }.into())))
            .collect();
        assert_eq!(
            ids(prioritize(pictures.clone(), &ratings)),
            [
                "high0", "high1", "high2", "high3", "low0", "high4", "high5", "high6", "high7", "low1", "high8",
                "high9", "low2", "low3", "low4"
            ]
        );
        assert_eq!(pictures[0].asset, "low0");
    }
    #[test]
    fn stable_unknown_ratings_never_drop_assets() {
        for count in 0..21 {
            let pictures: Vec<_> = (0..count).map(|i| pic(&i.to_string(), "x")).collect();
            let expected = ids(pictures.clone());
            assert_eq!(ids(prioritize(pictures.clone(), &BTreeMap::new())), expected);
            let ratings = pictures.iter().map(|p| (p.asset.clone(), Some("general".into()))).collect();
            assert_eq!(ids(prioritize(pictures, &ratings)), expected);
        }
        assert_eq!(
            ids(prioritize(
                vec![pic("low", "pixiv"), pic("unknown", "other"), pic("high", "x")],
                &BTreeMap::from([("low".into(), Some("explicit".into())), ("high".into(), Some("sensitive".into()))])
            )),
            ["high", "unknown", "low"]
        );
    }
    #[test]
    fn ratings_are_optional_and_read_only() {
        let (_root, request, _cache) = fixture();
        assert!(ratings(&request).is_empty());
        let core = Connection::open(request.library.join("library.sqlite")).unwrap();
        core.execute_batch(
            "CREATE TABLE asset_rating(uuid PRIMARY KEY,rating); INSERT INTO asset_rating VALUES ('a','general');",
        )
        .unwrap();
        let before = core.total_changes();
        assert_eq!(ratings(&request), BTreeMap::from([("a".into(), Some("general".into()))]));
        assert_eq!(core.total_changes(), before);
    }
    #[test]
    fn save_run_representatives_and_x_alternate() {
        let (_root, request, cache) = fixture();
        for i in 0..8 {
            insert(&request, &format!("p{i}"), "pixiv", &format!("2025-01-01T10:{i:02}:00"));
        }
        insert(&request, "p-alone", "pixiv", "2025-02-01T10:00:00");
        insert(&request, "x1", "x", "2025-01-01T09:00:00");
        let got = ids(order(&request, &cache).unwrap());
        assert_eq!(&got[..5], ["p0", "x1", "p2", "p4", "p-alone"]);
        let mut rest = got[5..].to_vec();
        rest.sort();
        assert_eq!(rest, ["p1", "p3", "p5", "p6", "p7"]);
    }
    #[test]
    fn manual_lookup_reasks_cached_but_skips_solved() {
        let (_root, mut request, cache) = fixture();
        for asset in ["picked", "other", "solved"] {
            insert(&request, asset, "x", "2025-01-01T10:00:00");
        }
        cache.execute_batch("INSERT INTO lookups(target,backend,last_result,next_due_at) VALUES ('picked','iqdb','miss',datetime('now','+90 days')); INSERT INTO artists(id) VALUES ('A'); INSERT INTO image_artists(asset,artist_id,method,status) VALUES ('solved','A','test','auto');").unwrap();
        request.only = vec!["picked".into(), "solved".into()];
        assert_eq!(ids(order(&request, &cache).unwrap()), ["picked"]);
    }
    #[test]
    fn automatic_order_ranks_only_eligible_assets() {
        let (_root, request, cache) = fixture();
        for asset in ["p-low", "x-high", "x-solved", "x-cached"] {
            insert(&request, asset, if asset.starts_with('p') { "pixiv" } else { "x" }, "2025-01-01T10:00:00");
        }
        cache.execute_batch("INSERT INTO artists(id) VALUES ('A'); INSERT INTO image_artists(asset,artist_id,method,status) VALUES ('x-solved','A','test','auto'); INSERT INTO lookups(target,backend,last_result,next_due_at) VALUES ('x-cached','iqdb','miss',datetime('now','+90 days'));").unwrap();
        let core = Connection::open(request.library.join("library.sqlite")).unwrap();
        core.execute_batch("CREATE TABLE asset_rating(uuid PRIMARY KEY,rating); INSERT INTO asset_rating VALUES ('p-low','explicit'),('x-high','sensitive');").unwrap();
        assert_eq!(ids(order(&request, &cache).unwrap()), ["x-high", "p-low"]);
    }
    #[test]
    fn manual_selection_keeps_order_without_priority() {
        let (_root, mut request, cache) = fixture();
        insert(&request, "low", "pixiv", "2025-01-01T10:00:00");
        insert(&request, "high", "x", "2025-01-01T10:00:00");
        let core = Connection::open(request.library.join("library.sqlite")).unwrap();
        core.execute_batch("CREATE TABLE asset_rating(uuid PRIMARY KEY,rating); INSERT INTO asset_rating VALUES ('low','explicit'),('high','sensitive');").unwrap();
        request.only = vec!["low".into(), "high".into()];
        assert_eq!(ids(order(&request, &cache).unwrap()), ["low", "high"]);
    }
}
