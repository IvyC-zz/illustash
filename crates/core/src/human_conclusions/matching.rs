//! 只按完整内容核验恢复目标，不修改图片身份、不展开复制品。
use crate::{error::invalid, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// 一种输入形态的完整字节摘要；宿主负责确认文件在读取期间及提交前没有变化。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Content {
    /// 全文件 SHA-256，64 位小写十六进制。
    pub sha256: String,
    /// 计算摘要时读取的实际字节数。
    pub byte_length: u64,
    /// 原图或平台生成的替代输入，不能混为同一种内容。
    pub representation: String,
}

impl Content {
    /// 检查外部摘要的格式；格式合法不表示宿主已经核验了文件。
    pub fn validate(&self) -> Result<()> {
        if self.sha256.len() != 64 || !self.sha256.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)) {
            return invalid("完整内容摘要格式不正确");
        }
        if !matches!(self.representation.as_str(), "original" | "derived") {
            return invalid("不支持的图片输入形态");
        }
        Ok(())
    }
}

/// 源快照保存的哈希状态；历史摘要必须仍绑定到快照的历史内容版本。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum SourceHash {
    /// 导出时在线读取完整字节且版本未变。
    Verified { content: Content, version: String },
    /// 已保存的完整历史摘要；当前离线，但尚未观察到对应版本失效。
    LastKnown { content: Content, version: String },
    /// 没有足以支持认回的摘要或版本绑定。
    Missing,
}

impl SourceHash {
    /// 可用于匹配的快照历史内容；没有版本绑定的旧值一律拒绝。
    pub fn content(&self) -> Result<Option<&Content>> {
        match self {
            Self::Verified { content, version } | Self::LastKnown { content, version } => {
                content.validate()?;
                if version.is_empty() || version.len() > 512 {
                    return invalid("源内容缺少有效版本关联");
                }
                Ok(Some(content))
            }
            Self::Missing => Ok(None),
        }
    }
}

/// 源快照中的一个资产；只携带稳定引用和摘要，无文件路径。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceAsset {
    /// 源库里的资产键。
    pub key: String,
    /// 源快照的内容证明。
    pub hash: SourceHash,
}

/// 宿主在本次匹配中核实的目标文件状态，不接受从便携文件反序列化。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TargetHash {
    /// 本次在线核验成功，提交前宿主还需检查同一个文件版本。
    Verified(Content),
    /// 未完成内容核验；只有本次可确认的长度、输入形态才可用于排除候选。
    /// 离线文件的旧缓存长度不属于本次确认，必须填 None。
    Unverified { byte_length: Option<u64>, representation: Option<String> },
}

/// 目标库中允许接受人工结论的资产；宿主必须提供完整候选集。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TargetAsset {
    /// 目标库资产键。
    pub key: String,
    /// 当前内容核验结果。
    pub hash: TargetHash,
}

/// 核验成功时使用的身份依据。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Method {
    /// 同一库及资产 ID，且完整内容相同；不宣称全库内容唯一。
    IdAndSha256,
    /// 完整候选核验后只有一个目标匹配。
    ContentUnique,
}

/// 每个源资产恰有一个结果，不能把未完成核验算作未匹配。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Match {
    /// 找到一个经过完整内容核验的目标。
    Matched { target: String, method: Method },
    /// 候选核验完整，且没有相同内容。
    Unmatched,
    /// 同库原资产仍存在，但其当前字节与源快照不同；不能改投其他副本。
    ContentChanged,
    /// 有多个相同内容目标，或多个源资产将写入同一目标。
    Ambiguous { reason: String },
    /// 缺源摘要，或完整候选核验尚未完成。
    Unverified { reason: String },
}

/// 对完整目标候选集进行确定性匹配；返回顺序与源资产一致。
/// 同库 ID 仍须核验字节；内容认回必须排除所有可能匹配的未知候选。
pub fn match_assets(same_library: bool, source: &[SourceAsset], target: &[TargetAsset]) -> Result<Vec<Match>> {
    let mut source_keys = BTreeSet::new();
    for asset in source {
        if asset.key.is_empty() || !source_keys.insert(&asset.key) {
            return invalid("源资产键缺失或重复");
        }
        asset.hash.content()?;
    }
    let mut targets = BTreeMap::new();
    let mut contents: BTreeMap<(&str, u64, &str), Vec<&str>> = BTreeMap::new();
    let mut unknown = BTreeSet::new();
    for asset in target {
        if asset.key.is_empty() || targets.insert(asset.key.as_str(), &asset.hash).is_some() {
            return invalid("目标候选资产键缺失或重复");
        }
        match &asset.hash {
            TargetHash::Verified(content) => {
                content.validate()?;
                contents
                    .entry((&content.sha256, content.byte_length, &content.representation))
                    .or_default()
                    .push(&asset.key);
            }
            TargetHash::Unverified { byte_length, representation } => {
                unknown.insert((*byte_length, representation.as_deref()));
            }
        }
    }
    let mut out = Vec::with_capacity(source.len());
    let mut claimed: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for asset in source {
        let result = if let Some(content) = asset.hash.content()? {
            if let Some(target) = targets.get(asset.key.as_str()).filter(|_| same_library) {
                match target {
                    TargetHash::Verified(found) if found == content => {
                        Match::Matched { target: asset.key.clone(), method: Method::IdAndSha256 }
                    }
                    TargetHash::Verified(_) => Match::ContentChanged,
                    TargetHash::Unverified { .. } => Match::Unverified { reason: "same_id_unverified".into() },
                }
            } else {
                let matches =
                    contents.get(&(content.sha256.as_str(), content.byte_length, content.representation.as_str()));
                let incomplete = [None, Some(content.byte_length)].into_iter().any(|length| {
                    [None, Some(content.representation.as_str())]
                        .into_iter()
                        .any(|representation| unknown.contains(&(length, representation)))
                });
                match matches.map(Vec::as_slice).unwrap_or_default() {
                    [_, _, ..] => Match::Ambiguous { reason: "duplicate_content".into() },
                    _ if incomplete => Match::Unverified { reason: "candidate_set_incomplete".into() },
                    [only] => Match::Matched { target: (*only).to_owned(), method: Method::ContentUnique },
                    [] => Match::Unmatched,
                }
            }
        } else {
            Match::Unverified { reason: "source_hash_missing".into() }
        };
        if let Match::Matched { target, .. } = &result {
            claimed.entry(target.clone()).or_default().push(out.len());
        }
        out.push(result);
    }
    for indices in claimed.values().filter(|indices| indices.len() > 1) {
        for &index in indices {
            out[index] = Match::Ambiguous { reason: "multiple_source_assets".into() };
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn content(value: u8) -> Content {
        Content { sha256: format!("{value:064x}"), byte_length: 10, representation: "original".into() }
    }
    fn source(key: &str) -> SourceAsset {
        SourceAsset {
            key: key.into(),
            hash: SourceHash::LastKnown { content: content(1), version: "known-version".into() },
        }
    }
    fn target(key: &str, value: u8) -> TargetAsset {
        TargetAsset { key: key.into(), hash: TargetHash::Verified(content(value)) }
    }
    fn offline(key: &str, size: Option<u64>) -> TargetAsset {
        TargetAsset { key: key.into(), hash: TargetHash::Unverified { byte_length: size, representation: None } }
    }
    #[test]
    fn same_id_requires_bytes_and_does_not_claim_content_uniqueness() {
        assert_eq!(match_assets(true, &[source("a")], &[target("a", 2)]).unwrap(), [Match::ContentChanged]);
        assert_eq!(
            match_assets(true, &[source("a")], &[target("a", 1), target("copy", 1), offline("missing", None)]).unwrap(),
            [Match::Matched { target: "a".into(), method: Method::IdAndSha256 }]
        );
    }
    #[test]
    fn present_same_library_id_never_falls_back_to_another_copy() {
        assert_eq!(
            match_assets(true, &[source("a")], &[target("a", 2), target("copy", 1)]).unwrap(),
            [Match::ContentChanged]
        );
        assert_eq!(
            match_assets(true, &[source("a")], &[offline("a", None), target("copy", 1)]).unwrap(),
            [Match::Unverified { reason: "same_id_unverified".into() }]
        );
        assert_eq!(
            match_assets(true, &[source("a")], &[target("copy", 1)]).unwrap(),
            [Match::Matched { target: "copy".into(), method: Method::ContentUnique }]
        );
        assert_eq!(
            match_assets(false, &[source("a")], &[target("a", 2), target("copy", 1)]).unwrap(),
            [Match::Matched { target: "copy".into(), method: Method::ContentUnique }]
        );
    }
    #[test]
    fn unknown_candidates_block_uniqueness_until_all_possible_matches_are_verified() {
        for size in [None, Some(10)] {
            assert_eq!(
                match_assets(false, &[source("a")], &[target("b", 1), offline("unknown", size)]).unwrap(),
                [Match::Unverified { reason: "candidate_set_incomplete".into() }]
            );
            assert_eq!(
                match_assets(false, &[source("a")], &[offline("unknown", size)]).unwrap(),
                [Match::Unverified { reason: "candidate_set_incomplete".into() }]
            );
        }
        assert_eq!(
            match_assets(false, &[source("a")], &[target("b", 1), offline("different-length", Some(11))]).unwrap(),
            [Match::Matched { target: "b".into(), method: Method::ContentUnique }]
        );
    }
    #[test]
    fn duplicates_and_multiple_sources_are_never_expanded_or_arbitrarily_chosen() {
        assert_eq!(
            match_assets(false, &[source("a")], &[target("b", 1), target("c", 1)]).unwrap(),
            [Match::Ambiguous { reason: "duplicate_content".into() }]
        );
        assert_eq!(
            match_assets(true, &[source("a"), source("b")], &[target("a", 1)]).unwrap(),
            [
                Match::Ambiguous { reason: "multiple_source_assets".into() },
                Match::Ambiguous { reason: "multiple_source_assets".into() }
            ]
        );
    }
    #[test]
    fn cached_target_is_never_a_match_and_missing_source_is_not_unmatched() {
        assert_eq!(
            match_assets(true, &[source("a")], &[offline("a", Some(10))]).unwrap(),
            [Match::Unverified { reason: "same_id_unverified".into() }]
        );
        let source = SourceAsset { key: "a".into(), hash: SourceHash::Missing };
        assert_eq!(
            match_assets(true, &[source], &[target("a", 1)]).unwrap(),
            [Match::Unverified { reason: "source_hash_missing".into() }]
        );
    }
}
