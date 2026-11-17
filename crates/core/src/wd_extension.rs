//! WD 扩展头的证据入口。
use crate::character_extension::{self, Extension};
use crate::Result;
use rusqlite::Connection;

/// 线性扩展头的证据来源。
pub const SOURCE: &str = "wd_extension";
/// 线性扩展头的证据方法。
pub const METHOD: &str = "linear_head";

/// 某张图库图片上，一条越过包内阈值的扩展头命中。
pub struct Hit {
    /// 图库内部图片 ID。
    pub uuid: String,
    /// 冻结词表中的 Danbooru 角色键。
    pub tag: String,
    /// 线性头的 sigmoid 概率。
    pub probability: f64,
}

/// 以给定冻结包的命中更新整个图库的 WD 扩展候选。调用方持有核心写事务。
pub fn apply(c: &Connection, package: &str, hits: &[Hit]) -> Result<usize> {
    publish(c, package, None, hits)
}

/// 只替换本次扫描范围内的 WD 扩展候选。调用方持有核心写事务。
pub fn apply_scoped(c: &Connection, package: &str, scope: &[String], hits: &[Hit]) -> Result<usize> {
    publish(c, package, Some(scope), hits)
}

fn publish(c: &Connection, package: &str, scope: Option<&[String]>, hits: &[Hit]) -> Result<usize> {
    character_extension::apply(
        c,
        Extension::Wd,
        package,
        scope,
        hits.iter().map(|h| (h.uuid.as_str(), h.tag.as_str(), h.probability)),
    )
}

/// 撤回 WD 扩展候选。调用方持有核心写事务，历史证据仍可回读。
pub fn withdraw(c: &Connection) -> Result<usize> {
    character_extension::withdraw(c, Extension::Wd)
}

/// 图片移除或 WD 向量作废时撤回这些图的扩展候选。调用方持有核心写事务。
pub fn invalidate(c: &Connection, uuids: &[String]) -> Result<()> {
    character_extension::invalidate(c, Extension::Wd, uuids)
}
