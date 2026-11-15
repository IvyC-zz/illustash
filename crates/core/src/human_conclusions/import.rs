//! 人工快照的事务写入与独立撤销所有者；图片候选只接受宿主本次核验的完整清单。
use super::{
    export,
    format::{Entity, Header, Policy, Record, Snapshot},
    matching::{self, Match, SourceAsset, TargetAsset},
    state::{self, Domain, Mark},
};
use crate::{error::invalid, schema, store, Error, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

/// 一条源逻辑记录的唯一处理结果；只返回最多 500 条明细。
#[derive(Clone, Debug, Serialize)]
pub struct Detail {
    /// 源记录类别。
    pub kind: String,
    /// 源文件内的稳定记录引用。
    pub record_key: String,
    /// applied、already_present 或拒绝原因。
    pub result: String,
}

/// 单次导入结果；定义数和人工逻辑记录数分开计数。
#[derive(Clone, Debug, Default, Serialize)]
pub struct Report {
    /// 没有有效写入时为空，不创建空历史。
    pub action_id: Option<i64>,
    /// 人工逻辑记录按唯一结果分类的数量。
    pub records: BTreeMap<String, usize>,
    /// 依赖定义按结果分类的数量。
    pub definitions: BTreeMap<String, usize>,
    /// 源资产按匹配结果分类的数量。
    pub assets: BTreeMap<String, usize>,
    /// 有界明细；计数始终包含全部记录。
    pub details: Vec<Detail>,
    /// 超出明细上限的条数。
    pub details_omitted: usize,
    /// 需要宿主在同一事务刷新收录投影的目标资产。
    #[serde(skip)]
    pub touched: Vec<String>,
}

impl Report {
    fn record(&mut self, record: &Record, result: &str) {
        *self.records.entry(result.into()).or_default() += 1;
        if self.details.len() < 500 {
            self.details.push(Detail {
                kind: record.identity().0.into(),
                record_key: record.identity().1.into(),
                result: result.into(),
            });
        } else {
            self.details_omitted += 1;
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct History {
    v: u32,
    source_library: String,
    snapshot_id: String,
    // 当前阶段只补从未决定的逻辑键；before 均为无贡献，after 用于拒绝独立改动后的撤销。
    after: Vec<Record>,
}

fn key(record: &Record) -> Vec<String> {
    record.logical_key().into_iter().map(str::to_owned).collect()
}

fn current(c: &Connection) -> Result<Snapshot> {
    let policy = if crate::character_projection::policy(c)? { Policy::Hybrid } else { Policy::Default };
    export::capture(c, Header::new("current".into(), "current".into(), "2000-01-01T00:00:00Z".into(), policy))
}

fn equivalent(a: &Record, b: &Record) -> Result<bool> {
    let normalize = |record: &Record| -> Result<Value> {
        let mut value = serde_json::to_value(record).map_err(|e| Error::invalid(e.to_string()))?;
        let object = value.as_object_mut().expect("记录序列化为对象");
        object.remove("record_key");
        // 操作时间是审计信息，不让同一判断因创建时刻不同被重复导入。
        object.remove("created_at");
        if let Some(decision) = object.get_mut("decision").and_then(Value::as_object_mut) {
            decision.remove("created_at");
        }
        Ok(value)
    };
    Ok(normalize(a)? == normalize(b)?)
}

fn resolve_existing(c: &Connection, same_library: bool, source: &str, definition: &Entity) -> Result<Option<String>> {
    let mut candidates = BTreeSet::new();
    if same_library && store::term_by_key(c, source)?.is_some() {
        candidates.insert(source.to_owned());
    }
    for link in &definition.links {
        if let Some(target) = c
            .query_row(
                "SELECT t.key FROM term_links l JOIN terms t ON t.id=l.term_id WHERE l.system=?1 AND l.key=?2",
                [&link.system, &link.key],
                |r| r.get::<_, String>(0),
            )
            .optional()?
        {
            candidates.insert(target);
        }
    }
    let Some(target) = candidates.pop_first() else { return Ok(None) };
    if !candidates.is_empty() {
        return Ok(None);
    }
    let kind: String = c.query_row("SELECT kind FROM terms WHERE key=?1", [&target], |r| r.get(0))?;
    // 结构与关系的贡献撤销尚未接入时，不把它们降格为普通平面词条写入。
    let structured: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM term_tree r JOIN terms t ON t.id=r.term_id WHERE t.key=?1)",
        [&target],
        |r| r.get(0),
    )?;
    Ok((kind == definition.kind && definition.tree.is_none() && definition.baseline.is_none() && !structured)
        .then_some(target))
}

fn match_reason(result: &Match) -> &'static str {
    match result {
        Match::Matched { .. } => "matched",
        Match::Unmatched => "unmatched_asset",
        Match::Ambiguous { .. } => "ambiguous_asset",
        Match::Unverified { .. } => "unverified_asset",
    }
}

/// 在宿主持有的 BEGIN IMMEDIATE 和核心写钥匙中应用快照。
/// 宿主须提供全部当前资产的本次内容核验结果，并在提交前复核文件版本、会话及取消信号。
/// 返回错误时必须回滚整个事务。未实现的依赖或记录明确报告，不算成功导入。
pub fn apply_in(
    c: &Connection,
    snapshot: &Snapshot,
    target_library: &str,
    candidates: &[TargetAsset],
    check: &mut impl FnMut() -> Result<()>,
) -> Result<Report> {
    if c.is_autocommit() {
        return invalid("导入人工结论需要写事务");
    }
    schema::require_current(c)?;
    snapshot.validate()?;
    check()?;
    let live: BTreeSet<String> =
        c.prepare("SELECT uuid FROM assets")?.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
    if live != candidates.iter().map(|a| a.key.clone()).collect() || live.len() != candidates.len() {
        return Err(Error::Conflict("目标资产清单已经变化，须重新核验图片内容".into()));
    }
    let same_library = snapshot.header.library_id == target_library;
    let source: Vec<SourceAsset> = snapshot
        .records
        .iter()
        .filter_map(|r| match r {
            Record::Asset { key, hash } => Some(SourceAsset { key: key.clone(), hash: hash.clone() }),
            _ => None,
        })
        .collect();
    let matched = matching::match_assets(same_library, &source, candidates)?;
    let assets: BTreeMap<_, _> = source.iter().map(|a| a.key.as_str()).zip(&matched).collect();
    let mut report = Report::default();
    for result in &matched {
        *report.assets.entry(match_reason(result).into()).or_default() += 1;
    }
    let mut terms = BTreeMap::new();
    for record in &snapshot.records {
        if let Record::Entity { key, definition } = record {
            let target = resolve_existing(c, same_library, key, definition)?;
            *report
                .definitions
                .entry(if target.is_some() { "matched_dependency" } else { "blocked_dependency" }.into())
                .or_default() += 1;
            if let Some(target) = target {
                terms.insert(key.clone(), target);
            }
        }
    }
    // 两个源定义同时认回一个目标会让各自的结论占用同一个逻辑键；禁止靠记录顺序覆盖。
    let mut claims = BTreeMap::<String, usize>::new();
    for target in terms.values() {
        *claims.entry(target.clone()).or_default() += 1;
    }
    terms.retain(|_, target| claims[target] == 1);
    let existing = current(c)?;
    let existing: BTreeMap<_, _> = existing.records.iter().map(|r| (key(r), r)).collect();
    let mut planned = Vec::new();
    for source in &snapshot.records {
        check()?;
        if matches!(source, Record::Asset { .. } | Record::Entity { .. }) {
            continue;
        }
        if let Some(reason) = source.assets().into_iter().find_map(|a| match assets[a] {
            Match::Matched { .. } => None,
            result => Some(match_reason(result)),
        }) {
            report.record(source, reason);
            continue;
        }
        if source.terms().iter().any(|t| !terms.contains_key(*t)) {
            report.record(source, "blocked_dependency");
            continue;
        }
        let mut target = source.clone();
        let asset_key = |asset: &str| match assets[asset] {
            Match::Matched { target, .. } => target.clone(),
            _ => unreachable!("所有依赖资产已经匹配"),
        };
        match &mut target {
            Record::ImageVerdict { asset, term, .. } => {
                *asset = asset_key(asset);
                *term = terms[term].clone();
            }
            Record::Curation { asset, .. } | Record::Deferred { asset, .. } => *asset = asset_key(asset),
            Record::Uncertain { asset, options, .. } => {
                *asset = asset_key(asset);
                *options = options.iter().map(|t| terms[t].clone()).collect();
            }
            _ => {
                report.record(source, "unsupported_record");
                continue;
            }
        }
        if let Some(before) = existing.get(&key(&target)) {
            report.record(source, if equivalent(before, &target)? { "already_present" } else { "conflict" });
        } else {
            report.record(source, "applied");
            planned.push(target);
        }
    }
    if planned.is_empty() {
        return Ok(report);
    }
    c.execute("INSERT INTO actions(label,before_json) VALUES ('已导入人工结论','[]')", [])?;
    let action = c.last_insert_rowid();
    let mut touched = BTreeSet::new();
    for record in &planned {
        check()?;
        write(c, action, &snapshot.header.snapshot_id, record)?;
        touched.extend(record.assets().into_iter().map(str::to_owned));
    }
    project(c, &touched)?;
    let applied: BTreeSet<_> = planned.iter().map(key).collect();
    let after = current(c)?.records.into_iter().filter(|r| applied.contains(&key(r))).collect::<Vec<_>>();
    if after.len() != planned.len() {
        return Err(Error::Conflict("导入后的有效记录与本次计划不一致".into()));
    }
    let history = History {
        v: 1,
        source_library: snapshot.header.library_id.clone(),
        snapshot_id: snapshot.header.snapshot_id.clone(),
        after,
    };
    c.execute(
        "UPDATE actions SET before_json=?1 WHERE id=?2",
        params![json!([{"human_conclusions":history}]).to_string(), action],
    )?;
    report.action_id = Some(action);
    report.touched = touched.into_iter().collect();
    check()?;
    Ok(report)
}

fn write(c: &Connection, action: i64, snapshot: &str, record: &Record) -> Result<()> {
    let absent = |domain, asset: &str, origin: &str, at: &str| {
        state::set(
            c,
            domain,
            asset,
            action,
            Some(Mark {
                owner_action_id: action,
                origin: origin.into(),
                source_ref: Some(format!("{snapshot}:{}", record.identity().1)),
                created_at: at.into(),
            }),
        )
    };
    match record {
        Record::ImageVerdict { asset, term, decision, .. } => {
            let asset = store::library_asset(c, asset)?.ok_or_else(|| Error::Conflict("目标图片已移除".into()))?;
            let (term, _) = store::term_by_key(c, term)?.ok_or_else(|| Error::Conflict("目标词条已移除".into()))?;
            c.execute(
                "INSERT INTO term_verdicts(action_id,anchor,asset_id,term_id,verdict,origin,via,reason,created_at)
                 VALUES (?1,?1,?2,?3,?4,?5,?6,?7,?8)",
                params![
                    action,
                    asset,
                    term,
                    decision.verdict,
                    decision.origin,
                    decision.via,
                    decision.reason,
                    decision.created_at
                ],
            )?;
        }
        Record::Curation { asset, state: value, origin, created_at, .. } => {
            if value == "reset" {
                absent(Domain::Curation, asset, origin, created_at)?;
            } else {
                c.execute(
                    "INSERT INTO curation(uuid,status,origin,created_at) VALUES (?1,?2,?3,?4)",
                    params![asset, value, origin, created_at],
                )?;
            }
        }
        Record::Deferred { asset, state: value, origin, created_at, .. } => {
            if value == "resumed" {
                absent(Domain::Deferred, asset, origin, created_at.as_deref().expect("格式已检查恢复时间"))?;
            } else {
                c.execute("INSERT INTO deferred(uuid) VALUES (?1)", [asset])?;
            }
        }
        Record::Uncertain { asset, state: value, options, reason, origin, created_at, .. } => {
            if value == "cleared" {
                absent(Domain::Uncertain, asset, origin, created_at)?;
            } else {
                c.execute(
                    "INSERT INTO identity_uncertain(uuid,options_json,reason,created_at) VALUES (?1,?2,?3,?4)",
                    params![asset, json!(options).to_string(), reason, created_at],
                )?;
            }
        }
        _ => return invalid("尚未接入此类人工记录的事务写入"),
    }
    Ok(())
}

fn project(c: &Connection, touched: &BTreeSet<String>) -> Result<()> {
    let mut assets = Vec::new();
    for uuid in touched {
        if let Some(asset) = store::library_asset(c, uuid)? {
            assets.push(asset);
        }
    }
    store::project(c, &assets)
}

fn history(value: &Value) -> Result<Option<History>> {
    let Some(items) = value.as_array() else { return Ok(None) };
    if !items.iter().any(|v| v.get("human_conclusions").is_some()) {
        return Ok(None);
    }
    if items.len() != 1 || items[0].as_object().is_none_or(|o| o.len() != 1) {
        return Err(Error::Conflict("人工结论撤销信封不完整".into()));
    }
    let decoded: History = serde_json::from_value(items[0]["human_conclusions"].clone())
        .map_err(|_| Error::Conflict("人工结论撤销信封不完整".into()))?;
    if decoded.v != 1 || decoded.after.is_empty() {
        return Err(Error::Conflict("人工结论撤销版本不受支持".into()));
    }
    Ok(Some(decoded))
}

/// 识别专用历史，混合或未知版本返回错误，不落入普通应用撤销。
pub fn owns(value: &Value) -> Result<bool> {
    Ok(history(value)?.is_some())
}

/// 撤销最后一个有效导入；检查全部 after 后再写入，独立变更导致冲突。
/// 调用方持有写事务和写钥匙，并在同一事务刷新返回资产的收录投影。
pub fn undo_in(c: &Connection, action: i64) -> Result<Vec<String>> {
    if c.is_autocommit() {
        return invalid("撤销人工结论需要写事务");
    }
    let last: Option<i64> = c.query_row("SELECT max(id) FROM actions WHERE undone=0", [], |r| r.get(0))?;
    if last != Some(action) {
        return Err(Error::Conflict(store::HISTORY_CHANGED.into()));
    }
    let encoded: String = c.query_row("SELECT before_json FROM actions WHERE id=?1", [action], |r| r.get(0))?;
    let value = crate::strict_json::parse(&encoded).map_err(Error::invalid)?;
    let history = history(&value)?.ok_or_else(|| Error::Conflict("这一步不属于人工结论导入".into()))?;
    let now = current(c)?;
    let now: BTreeMap<_, _> = now.records.iter().map(|r| (key(r), r)).collect();
    for expected in &history.after {
        if now.get(&key(expected)).copied() != Some(expected) {
            return Err(Error::Conflict("导入后的人工状态已被独立修改，无法安全撤销".into()));
        }
    }
    let mut touched = BTreeSet::new();
    for record in &history.after {
        let table = match record {
            Record::ImageVerdict { .. } => None,
            Record::Curation { .. } => Some("curation"),
            Record::Deferred { .. } => Some("deferred"),
            Record::Uncertain { .. } => Some("identity_uncertain"),
            _ => return Err(Error::Conflict("人工结论撤销包含不支持的记录".into())),
        };
        for asset in record.assets() {
            if let Some(table) = table {
                c.execute(&format!("DELETE FROM {table} WHERE uuid=?1"), [asset])?;
            }
            touched.insert(asset.to_owned());
        }
    }
    c.execute("UPDATE actions SET undone=1 WHERE id=?1", [action])?;
    state::undo(c, action)?;
    project(c, &touched)?;
    Ok(touched.into_iter().collect())
}
