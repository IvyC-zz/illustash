//! 角色、皮肤、作品的外部链接（`term_links`）。核心第 8 版（#201）起它是唯一权威：命令直接写它，读者读它；原来的兼容表
//! `links` 在升级时照旧表同步一次、逐行核对后删除（`legacy_characters::retire_links`）。
//!
//! `term_links` 里也有画师等别的词条的链接，`(system, key)` 全局唯一。角色的链接沿用原来的系统名，只有 WD 改名
//! `wd_character`（WD 题材占了 `wd`）。
use crate::{Error, Result};
use rusqlite::Connection;

/// 与原来的 `links` 同形的读法：系统名照旧写（`wd`），`entity_id` 是身份 id（`terms.key`），只含 `term_tree` 里的
/// 词条。另带存储里的系统名 `core_system`：按系统查找时过滤它，能走 `(system, key)` 主键。
pub const LINKS: &str = "(SELECT CASE x.system WHEN 'wd_character' THEN 'wd' ELSE x.system END AS system,
    x.system AS core_system, x.key AS key, t.key AS entity_id, x.current AS current, x.source AS source,
    x.added_at AS added_at
  FROM term_links x
  JOIN term_tree r ON r.term_id = x.term_id
  JOIN terms t ON t.id = x.term_id)";

/// 存储里的系统名：WD 写作 `wd_character`，其余不变。
pub fn core_system(system: &str) -> &str {
    if system == "wd" {
        "wd_character"
    } else {
        system
    }
}

/// 写一个身份的链接之前：这个 (system, key) 已属于别的类型的词条（画师等）就拒绝。原来由命令末尾镜像旧表时检查，
/// 报错相同。
pub fn check_free(c: &Connection, system: &str, key: &str) -> Result<()> {
    let taken: bool = c
        .prepare_cached(
            "SELECT EXISTS(
               SELECT 1 FROM term_links
               WHERE system = ? AND key = ? AND term_id NOT IN (SELECT term_id FROM term_tree))",
        )?
        .query_row([core_system(system), key], |r| r.get(0))?;
    if taken {
        return Err(Error::Conflict(format!("链接 {system}:{key} 已属于别的类型的词条")));
    }
    Ok(())
}
