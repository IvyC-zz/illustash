//! JSONL v1 的封闭字段、完整性摘要与资源上限；解析成功前不接触目标数据库。
use super::matching::SourceHash;
use crate::{error::invalid, strict_json, Error, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io::{BufRead, Read, Write};

/// 单个快照最多 256 MiB，含头尾记录。
pub const MAX_BYTES: u64 = 256 * 1024 * 1024;
/// 单行最多 4 MiB，含换行符。
pub const MAX_LINE: u64 = 4 * 1024 * 1024;
/// 头尾以外最多二百万条记录。
pub const MAX_RECORDS: usize = 2_000_000;

/// 源图库使用的身份解释策略。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Policy {
    /// 普通父子结构及合并指向。
    Default,
    /// 含关系基线、等价组和判断范围的身份策略。
    Hybrid,
}

/// 快照头；随机编号和导出时间不参与语义摘要。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Header {
    record: String,
    format: String,
    version: u32,
    scope: String,
    /// 快照唯一编号。
    pub snapshot_id: String,
    /// 源图库编号，不能单独作为图片相同的证明。
    pub library_id: String,
    /// UTC 导出时间。
    pub exported_at: String,
    /// 源图库的身份解释策略。
    pub policy: Policy,
}

impl Header {
    /// 创建 JSONL v1 的固定格式头。
    pub fn new(snapshot_id: String, library_id: String, exported_at: String, policy: Policy) -> Self {
        Self {
            record: "header".into(),
            format: "illustash-human".into(),
            version: 1,
            scope: "effective_state".into(),
            snapshot_id,
            library_id,
            exported_at,
            policy,
        }
    }

    fn validate(&self) -> Result<()> {
        if self.record != "header"
            || self.format != "illustash-human"
            || self.version != 1
            || self.scope != "effective_state"
        {
            return invalid("不支持的人工结论文件格式或版本");
        }
        key(&self.snapshot_id)?;
        key(&self.library_id)?;
        if !self.exported_at.ends_with('Z') || self.exported_at.len() > 40 || !self.exported_at.contains('T') {
            return invalid("快照导出时间须为 UTC");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Name {
    pub lang: String,
    pub value: String,
    pub role: String,
    pub preferred: bool,
    pub position: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Link {
    pub system: String,
    pub key: String,
    pub url: String,
    pub current: bool,
    pub added_at: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Tree {
    pub parent: Option<String>,
    pub merged_into: Option<String>,
    pub work: Option<String>,
    pub work_text: String,
    pub variant: String,
    pub artist: String,
    pub label_only: bool,
    pub mascot: bool,
    pub mascot_origin: Option<String>,
    pub original: bool,
    pub parent_locked: Option<bool>,
    pub review_status: Option<String>,
    pub review_note: Option<String>,
    pub order: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Baseline {
    pub parent: Option<String>,
    pub variant: Option<String>,
    pub kind: Option<String>,
    pub merged_into: Option<String>,
    pub order: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Entity {
    pub kind: String,
    pub origin: String,
    pub provenance: String,
    pub tree: Option<Tree>,
    pub baseline: Option<Baseline>,
    pub names: Vec<Name>,
    pub links: Vec<Link>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Verdict {
    pub verdict: String,
    pub origin: String,
    pub via: String,
    pub reason: String,
    pub created_at: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Suspended {
    pub reason: String,
    pub options: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "record", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Record {
    Asset {
        key: String,
        hash: SourceHash,
    },
    Entity {
        key: String,
        definition: Box<Entity>,
    },
    ImageVerdict {
        key: String,
        asset: String,
        term: String,
        decision: Verdict,
    },
    RelationVerdict {
        key: String,
        a: String,
        b: String,
        kind: String,
        decision: Verdict,
        seen: Vec<String>,
        reviewed: Vec<String>,
    },
    IdentityContext {
        key: String,
        asset: String,
        term: String,
        scope: Vec<String>,
        suspended: Option<Suspended>,
    },
    Curation {
        key: String,
        asset: String,
        state: String,
        origin: String,
        created_at: String,
    },
    Deferred {
        key: String,
        asset: String,
        state: String,
        origin: String,
        created_at: String,
    },
    Uncertain {
        key: String,
        asset: String,
        state: String,
        options: Vec<String>,
        reason: String,
        origin: String,
        created_at: String,
    },
    Review {
        key: String,
        term: String,
        reviewed_at: String,
    },
    IdentitySkip {
        key: String,
        pair: String,
    },
    AnchorDecision {
        key: String,
        term: String,
        reference: String,
        reference_version: String,
        decision: String,
        reason: String,
    },
}

impl Record {
    fn identity(&self) -> (&'static str, &str) {
        match self {
            Self::Asset { key, .. } => ("asset", key),
            Self::Entity { key, .. } => ("entity", key),
            Self::ImageVerdict { key, .. } => ("image_verdict", key),
            Self::RelationVerdict { key, .. } => ("relation_verdict", key),
            Self::IdentityContext { key, .. } => ("identity_context", key),
            Self::Curation { key, .. } => ("curation", key),
            Self::Deferred { key, .. } => ("deferred", key),
            Self::Uncertain { key, .. } => ("uncertain", key),
            Self::Review { key, .. } => ("review", key),
            Self::IdentitySkip { key, .. } => ("identity_skip", key),
            Self::AnchorDecision { key, .. } => ("anchor_decision", key),
        }
    }

    fn assets(&self) -> Vec<&str> {
        match self {
            Self::ImageVerdict { asset, .. }
            | Self::IdentityContext { asset, .. }
            | Self::Curation { asset, .. }
            | Self::Deferred { asset, .. }
            | Self::Uncertain { asset, .. } => vec![asset],
            _ => Vec::new(),
        }
    }

    fn terms(&self) -> Vec<&str> {
        match self {
            Self::Entity { definition, .. } => {
                let mut out = Vec::new();
                if let Some(tree) = &definition.tree {
                    out.extend(
                        [tree.parent.as_deref(), tree.merged_into.as_deref(), tree.work.as_deref()]
                            .into_iter()
                            .flatten(),
                    );
                }
                if let Some(baseline) = &definition.baseline {
                    out.extend([baseline.parent.as_deref(), baseline.merged_into.as_deref()].into_iter().flatten());
                }
                out
            }
            Self::ImageVerdict { term, .. } | Self::Review { term, .. } | Self::AnchorDecision { term, .. } => {
                vec![term]
            }
            Self::RelationVerdict { a, b, .. } => vec![a, b],
            Self::IdentityContext { term, scope, suspended, .. } => {
                let mut out = vec![term.as_str()];
                out.extend(scope.iter().map(String::as_str));
                if let Some(s) = suspended {
                    out.extend(s.options.iter().map(String::as_str));
                }
                out
            }
            Self::Uncertain { options, .. } => options.iter().map(String::as_str).collect(),
            _ => Vec::new(),
        }
    }

    fn logical_key(&self) -> Vec<&str> {
        let (kind, key) = self.identity();
        match self {
            Self::ImageVerdict { asset, term, .. } | Self::IdentityContext { asset, term, .. } => {
                vec![kind, asset, term]
            }
            Self::RelationVerdict { a, b, kind: relation, .. } => vec![kind, a, b, relation],
            Self::IdentitySkip { pair, .. } => vec![kind, pair],
            Self::Curation { asset, .. } | Self::Deferred { asset, .. } | Self::Uncertain { asset, .. } => {
                vec![kind, asset]
            }
            Self::Review { term, .. } => vec![kind, term],
            Self::AnchorDecision { term, reference, .. } => vec![kind, term, reference],
            _ => vec![kind, key],
        }
    }

    fn validate(&self) -> Result<()> {
        key(self.identity().1)?;
        for value in self.terms().into_iter().chain(self.assets()) {
            key(value)?;
        }
        match self {
            Self::Asset { hash, .. } => {
                hash.content()?;
            }
            Self::Entity { definition: e, .. } => {
                if crate::schema::Kind::parse(&e.kind).is_none()
                    || !matches!(e.provenance.as_str(), "user" | "legacy_unknown" | "dependency")
                {
                    return invalid("词条定义类型或来源不正确");
                }
                bounded(&e.origin, 1, 80)?;
                let mut names = BTreeSet::new();
                let mut preferred = BTreeSet::new();
                for n in &e.names {
                    if !matches!(n.lang.as_str(), "" | "zh" | "ja" | "en")
                        || !matches!(n.role.as_str(), "display" | "alias")
                        || !names.insert((&n.lang, &n.role, &n.value))
                        || (n.preferred && !preferred.insert((&n.lang, &n.role)))
                    {
                        return invalid("名字定义重复或不正确");
                    }
                    bounded(&n.value, 1, 200)?;
                }
                let mut links = BTreeSet::new();
                for l in &e.links {
                    bounded(&l.system, 1, 32)?;
                    bounded(&l.key, 1, 256)?;
                    if !links.insert((&l.system, &l.key)) {
                        return invalid("外部链接定义重复");
                    }
                }
            }
            Self::ImageVerdict { decision, .. } | Self::RelationVerdict { decision, .. } => {
                one_of(&decision.verdict, &["yes", "no", "clear"])?;
                one_of(&decision.origin, &["user", "album", "legacy"])?;
                bounded(&decision.via, 0, 40)?;
            }
            Self::Curation { state, origin, .. } => {
                one_of(state, &["included", "excluded", "reset"])?;
                bounded(origin, 1, 80)?;
            }
            Self::Deferred { state, origin, .. } => {
                one_of(state, &["deferred", "resumed"])?;
                bounded(origin, 1, 80)?;
            }
            Self::Uncertain { state, options, reason, origin, .. } => {
                one_of(state, &["uncertain", "cleared"])?;
                bounded(origin, 1, 80)?;
                if (state == "cleared" && !options.is_empty())
                    || (state == "uncertain" && (!(2..=20).contains(&options.len()) || reason.trim().is_empty()))
                {
                    return invalid("未定状态的候选或理由不正确");
                }
                unique(options)?;
            }
            Self::IdentityContext { scope, suspended, .. } => {
                unique(scope)?;
                if let Some(s) = suspended {
                    unique(&s.options)?;
                }
            }
            _ => {}
        }
        if let Self::RelationVerdict { a, b, kind, seen, reviewed, .. } = self {
            one_of(kind, &["contains", "same_as", "different_from", "excludes"])?;
            if a == b || (matches!(kind.as_str(), "different_from" | "excludes") && a > b) {
                return invalid("身份关系端点不正确");
            }
            unique(seen)?;
            unique(reviewed)?;
            let seen: BTreeSet<_> = seen.iter().collect();
            if reviewed.iter().any(|v| !seen.contains(v)) {
                return invalid("已审证据必须属于所见证据");
            }
        }
        if let Self::IdentitySkip { pair, .. } = self {
            // 现行入口允许任意键，且以竖线拼接。保持原串，不把无法唯一解析的旧键捏造成两个身份。
            bounded(pair, 1, MAX_LINE as usize)?;
        }
        Ok(())
    }
}

fn bounded(value: &str, min: usize, max: usize) -> Result<()> {
    if !(min..=max).contains(&value.chars().count()) || value.contains('\0') {
        return invalid("人工结论字段长度或内容不正确");
    }
    Ok(())
}
fn key(value: &str) -> Result<()> {
    bounded(value, 1, 256)
}
fn one_of(value: &str, choices: &[&str]) -> Result<()> {
    if !choices.contains(&value) {
        return invalid("不支持的人工状态值");
    }
    Ok(())
}
fn unique(values: &[String]) -> Result<()> {
    let mut seen = BTreeSet::new();
    for v in values {
        key(v)?;
        if !seen.insert(v) {
            return invalid("人工状态包含重复引用");
        }
    }
    Ok(())
}

/// 哈希覆盖按源资产数计；last_known 保留历史内容证明，不承诺源文件当前在线。
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Coverage {
    /// 快照内的资产总数。
    pub total: u64,
    /// 导出时在线核验成功的资产数。
    pub verified: u64,
    /// 有有效历史摘要的离线资产数。
    pub last_known: u64,
    /// 尚无有效摘要的资产数。
    pub missing: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Footer {
    record: String,
    counts: BTreeMap<String, u64>,
    hash_coverage: Coverage,
    portable_complete: bool,
    sha256: String,
    semantic_sha256: String,
}

/// 已通过字段、引用、计数和完整性校验的人工状态快照。
pub struct Snapshot {
    pub(crate) header: Header,
    pub(crate) records: Vec<Record>,
}

impl Snapshot {
    /// 读取已验证的格式头。
    pub fn header(&self) -> &Header {
        &self.header
    }

    /// 计算资产摘要的覆盖，不受调用方传来的 footer 数字影响。
    pub fn coverage(&self) -> Coverage {
        let mut out = Coverage::default();
        for r in &self.records {
            if let Record::Asset { hash, .. } = r {
                out.total += 1;
                match hash {
                    SourceHash::Verified { .. } => out.verified += 1,
                    SourceHash::LastKnown { .. } => out.last_known += 1,
                    SourceHash::Missing => out.missing += 1,
                }
            }
        }
        out
    }

    /// 稳定语义摘要；忽略文件记录次序、快照随机编号和导出时间，保留人工操作的来源时间。
    pub fn semantic_sha256(&self) -> Result<String> {
        let mut hash = Sha256::new();
        hash.update(encode(&serde_json::json!({"format":"illustash-human","version":1,"scope":"effective_state","library_id":self.header.library_id,"policy":self.header.policy}))?);
        let mut records: Vec<_> = self.records.iter().collect();
        records.sort_by_key(|r| r.identity());
        for record in records {
            hash.update(encode(record)?);
        }
        Ok(strict_json::hex(&hash.finalize()))
    }

    fn counts(&self) -> BTreeMap<String, u64> {
        let mut out = BTreeMap::new();
        for record in &self.records {
            *out.entry(record.identity().0.into()).or_default() += 1;
        }
        out
    }

    pub(crate) fn validate(&self) -> Result<()> {
        self.header.validate()?;
        if self.records.len() > MAX_RECORDS {
            return invalid("人工结论记录过多");
        }
        let mut ids = BTreeSet::new();
        let mut logical = BTreeSet::new();
        let mut assets = BTreeSet::new();
        let mut entities = BTreeMap::new();
        let mut external = BTreeSet::new();
        for record in &self.records {
            record.validate()?;
            if !ids.insert(record.identity()) || !logical.insert(record.logical_key()) {
                return invalid("人工结论记录重复");
            }
            match record {
                Record::Asset { key, .. } => {
                    assets.insert(key.as_str());
                }
                Record::Entity { key, definition } => {
                    entities.insert(key.as_str(), record);
                    for link in &definition.links {
                        if !external.insert((&link.system, &link.key)) {
                            return invalid("多个定义占用了同一外部链接");
                        }
                    }
                    if self.header.policy == Policy::Default && definition.baseline.is_some() {
                        return invalid("默认策略不能携带 hybrid 身份基线");
                    }
                }
                _ => {}
            }
        }
        for record in &self.records {
            if record.assets().iter().any(|k| !assets.contains(k))
                || record.terms().iter().any(|k| !entities.contains_key(k))
            {
                return invalid("人工结论缺少引用的资产或词条定义");
            }
        }
        // 无递归的拓扑检查，长依赖链也不会耗尽调用栈。
        let mut degrees = BTreeMap::new();
        let mut dependents: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for (&key, record) in &entities {
            let refs: BTreeSet<_> = record.terms().into_iter().collect();
            degrees.insert(key, refs.len());
            for dependency in refs {
                dependents.entry(dependency).or_default().push(key);
            }
        }
        let mut queue: VecDeque<_> = degrees.iter().filter(|(_, count)| **count == 0).map(|(&key, _)| key).collect();
        let mut visited = 0;
        while let Some(key) = queue.pop_front() {
            visited += 1;
            for child in dependents.get(key).into_iter().flatten() {
                let count = degrees.get_mut(child).expect("依赖图的端点已经校验");
                *count -= 1;
                if *count == 0 {
                    queue.push_back(child);
                }
            }
        }
        if visited != entities.len() {
            return invalid("人工定义存在依赖环");
        }
        Ok(())
    }

    /// 写出经过自校验的 JSONL；调用方负责私有临时文件、刷盘和原子发布。
    pub fn write(&self, output: &mut impl Write) -> Result<()> {
        self.validate()?;
        let mut digest = Sha256::new();
        let mut bytes = 0;
        let header = encode(&self.header)?;
        write_line(output, &header, &mut bytes)?;
        digest.update(&header);
        for record in &self.records {
            let line = encode(record)?;
            write_line(output, &line, &mut bytes)?;
            digest.update(line);
        }
        let coverage = self.coverage();
        let footer = Footer {
            record: "footer".into(),
            counts: self.counts(),
            portable_complete: coverage.missing == 0,
            hash_coverage: coverage,
            sha256: strict_json::hex(&digest.finalize()),
            semantic_sha256: self.semantic_sha256()?,
        };
        write_line(output, &encode(&footer)?, &mut bytes)
    }
}

fn encode(value: &impl Serialize) -> Result<Vec<u8>> {
    let mut line = serde_json::to_vec(value).map_err(|e| Error::invalid(e.to_string()))?;
    line.push(b'\n');
    Ok(line)
}
fn write_line(out: &mut impl Write, line: &[u8], bytes: &mut u64) -> Result<()> {
    *bytes += line.len() as u64;
    if line.len() as u64 > MAX_LINE || *bytes > MAX_BYTES {
        return invalid("人工结论文件超过大小上限");
    }
    out.write_all(line)?;
    Ok(())
}
fn decode<T: serde::de::DeserializeOwned>(value: serde_json::Value) -> Result<T> {
    serde_json::from_value(value).map_err(|e| Error::invalid(format!("人工结论记录不正确：{e}")))
}

/// 严格读取一份完整快照；限制逐行读取长度，校验原始字节摘要、语义摘要及所有引用。
pub fn read(input: &mut impl BufRead) -> Result<Snapshot> {
    let mut digest = Sha256::new();
    let mut bytes = 0;
    let mut header = None;
    let mut records = Vec::new();
    let mut footer = None;
    loop {
        let mut line = Vec::new();
        let n = (&mut *input).take(MAX_LINE + 1).read_until(b'\n', &mut line)?;
        if n == 0 {
            break;
        }
        bytes += n as u64;
        if n as u64 > MAX_LINE || bytes > MAX_BYTES {
            return invalid("人工结论文件超过大小上限");
        }
        if footer.is_some() {
            return invalid("人工结论文件尾部还有内容");
        }
        if line.last() != Some(&b'\n') {
            return invalid("人工结论文件被截断");
        }
        let text = std::str::from_utf8(&line).map_err(|_| Error::invalid("人工结论文件须为 UTF-8"))?;
        let value = strict_json::parse(text).map_err(Error::invalid)?;
        if header.is_none() {
            let parsed: Header = decode(value)?;
            parsed.validate()?;
            header = Some(parsed);
        } else if value["record"] == "footer" {
            footer = Some(decode::<Footer>(value)?);
            continue;
        } else {
            if records.len() == MAX_RECORDS {
                return invalid("人工结论记录过多");
            }
            records.push(decode::<Record>(value)?);
        }
        digest.update(&line);
    }
    let (Some(header), Some(footer)) = (header, footer) else { return invalid("人工结论文件缺少完整头尾") };
    let snapshot = Snapshot { header, records };
    snapshot.validate()?;
    let coverage = snapshot.coverage();
    if footer.record != "footer"
        || footer.sha256 != strict_json::hex(&digest.finalize())
        || footer.semantic_sha256 != snapshot.semantic_sha256()?
        || footer.counts != snapshot.counts()
        || footer.hash_coverage != coverage
        || footer.portable_complete != (coverage.missing == 0)
    {
        return invalid("人工结论文件摘要、计数或哈希覆盖不一致");
    }
    Ok(snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn sample() -> Snapshot {
        Snapshot {
            header: Header::new(
                "snapshot-a".into(),
                "library-a".into(),
                "2026-11-15T00:00:00Z".into(),
                Policy::Default,
            ),
            records: vec![
                Record::Asset { key: "a".into(), hash: SourceHash::Missing },
                Record::Curation {
                    key: "curation:a".into(),
                    asset: "a".into(),
                    state: "reset".into(),
                    origin: "user".into(),
                    created_at: "2026-01-01 00:00:00".into(),
                },
            ],
        }
    }

    fn bytes(snapshot: &Snapshot) -> Vec<u8> {
        let mut out = Vec::new();
        snapshot.write(&mut out).unwrap();
        out
    }

    #[test]
    fn round_trip_keeps_explicit_reset_and_reports_incomplete_hash_coverage() {
        let original = sample();
        let wire = bytes(&original);
        let got = read(&mut Cursor::new(&wire)).unwrap();
        assert_eq!(got.header, original.header);
        assert_eq!(got.records, original.records);
        assert_eq!(got.coverage(), Coverage { total: 1, missing: 1, ..Coverage::default() });
        assert_eq!(wire, bytes(&got));
    }

    #[test]
    fn semantic_digest_ignores_only_export_identity_time_and_record_order() {
        let mut other = sample();
        let expected = other.semantic_sha256().unwrap();
        other.header.snapshot_id = "snapshot-b".into();
        other.header.exported_at = "2026-11-16T12:00:00Z".into();
        other.records.reverse();
        assert_eq!(other.semantic_sha256().unwrap(), expected);
        if let Record::Curation { created_at, .. } = &mut other.records[0] {
            *created_at = "changed-human-time".into();
        }
        assert_ne!(other.semantic_sha256().unwrap(), expected);
    }

    #[test]
    fn truncation_trailing_data_duplicate_fields_unknown_fields_and_tampered_counts_fail() {
        let wire = String::from_utf8(bytes(&sample())).unwrap();
        let failures = [
            wire[..wire.len() - 1].to_owned(),
            format!("{wire}\n"),
            wire.replace("\"state\":\"reset\"", "\"state\":\"reset\",\"state\":\"included\""),
            wire.replace("\"state\":\"reset\"", "\"state\":\"reset\",\"filename\":\"private.jpg\""),
            wire.replace("\"total\":1", "\"total\":2"),
            wire.replace("\"reset\"", "\"included\""),
            wire.replace("\"version\":1", "\"version\":2"),
        ];
        for invalid in failures {
            assert!(read(&mut Cursor::new(invalid)).is_err());
        }
    }

    #[test]
    fn duplicate_semantics_and_missing_dependencies_fail_even_with_different_record_keys() {
        let mut snapshot = sample();
        let mut duplicate = snapshot.records[1].clone();
        if let Record::Curation { key, .. } = &mut duplicate {
            *key = "other-key".into();
        }
        snapshot.records.push(duplicate);
        assert!(snapshot.validate().is_err());
        snapshot.records.pop();
        snapshot.records.remove(0);
        assert!(snapshot.validate().is_err());
    }

    fn entity(key: &str, parent: Option<&str>) -> Record {
        Record::Entity {
            key: key.into(),
            definition: Box::new(Entity {
                kind: "character".into(),
                origin: "user".into(),
                provenance: "user".into(),
                names: vec![],
                links: vec![],
                baseline: Some(Baseline {
                    parent: parent.map(str::to_owned),
                    variant: None,
                    kind: None,
                    merged_into: None,
                    order: 0,
                }),
                tree: None,
            }),
        }
    }

    #[test]
    fn dependency_cycles_are_rejected_without_recursing() {
        let mut snapshot = sample();
        snapshot.header.policy = Policy::Hybrid;
        snapshot.records.extend([entity("x", Some("y")), entity("y", Some("x"))]);
        assert!(snapshot.validate().is_err());
        snapshot.records.pop();
        snapshot.records.push(entity("y", None));
        snapshot.validate().unwrap();
    }

    #[test]
    fn legacy_skipped_pair_remains_opaque_when_its_keys_no_longer_resolve() {
        let mut snapshot = sample();
        snapshot.records.push(Record::IdentitySkip { key: "skip:a".into(), pair: "old|key|missing".into() });
        let result = read(&mut Cursor::new(bytes(&snapshot))).unwrap();
        assert_eq!(result.records, snapshot.records);
    }

    #[test]
    fn long_unterminated_line_stops_at_the_bounded_read() {
        let wire = vec![b' '; MAX_LINE as usize + 100];
        let mut reader = Cursor::new(wire);
        assert!(read(&mut reader).is_err());
        assert_eq!(reader.position(), MAX_LINE + 1);
    }
}
