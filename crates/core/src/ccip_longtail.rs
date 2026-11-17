//! CCIP 长尾参考包的独立角色证据。
use crate::character_extension::{self, Extension};
use crate::Result;
use rusqlite::{Connection, OptionalExtension};

/// 长尾参考包的证据来源。
pub const SOURCE: &str = "ccip_longtail";
/// 长尾参考包的锚点认领方式。
pub const METHOD: &str = "anchor_p75";

const PACKAGE_KEY: &str = "ccip_longtail_package";

/// 长尾锚点在一张图上的有效命中。
#[derive(Clone)]
pub struct Hit {
    /// 图库内部图片 ID。
    pub uuid: String,
    /// 规范 Danbooru 角色键。
    pub tag: String,
    /// 生产 CCIP 认领分数。
    pub score: f64,
}

/// 本图库启用的冻结包摘要。即使没有命中，启用状态也可回读。
pub fn enabled_package(c: &Connection) -> Result<Option<String>> {
    Ok(c.query_row("SELECT value FROM meta WHERE key=?1", [PACKAGE_KEY], |r| r.get(0)).optional()?)
}

/// 全库启用并发布命中。调用方持有核心写事务。
pub fn apply(c: &Connection, package: &str, hits: &[Hit]) -> Result<usize> {
    let changed = publish(c, package, None, hits)?;
    c.execute("INSERT OR REPLACE INTO meta(key,value) VALUES (?1,?2)", [PACKAGE_KEY, package])?;
    Ok(changed)
}

/// 给刚识别的一批图替换长尾命中。调用方在同一事务中确认包仍启用。
pub fn refresh(c: &Connection, package: &str, uuids: &[String], hits: &[Hit]) -> Result<usize> {
    if enabled_package(c)?.as_deref() != Some(package) {
        return Ok(0);
    }
    publish(c, package, Some(uuids), hits)
}

fn publish(c: &Connection, package: &str, scope: Option<&[String]>, hits: &[Hit]) -> Result<usize> {
    character_extension::apply(
        c,
        Extension::Ccip,
        package,
        scope,
        hits.iter().map(|h| (h.uuid.as_str(), h.tag.as_str(), h.score, false)),
    )
}

/// 撤回当前长尾包并按原证据重算归属。调用方持有核心写事务。
pub fn withdraw(c: &Connection) -> Result<usize> {
    let changed = character_extension::withdraw(c, Extension::Ccip)?;
    c.execute("DELETE FROM meta WHERE key=?1", [PACKAGE_KEY])?;
    Ok(changed)
}

/// 图片移除或 CCIP 向量作废时撤回这些图的长尾候选。
pub fn invalidate(c: &Connection, uuids: &[String]) -> Result<()> {
    character_extension::invalidate(c, Extension::Ccip, uuids)
}
