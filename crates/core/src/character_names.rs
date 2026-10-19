//! 角色、皮肤、作品的名字（`term_names`，按词条）。核心第 8 版（#201）起这里直接写 `term_names`，语义逐行照
//! tests/reference/web/names.py（#9 已删除） `put_name` 与 tests/reference/web/store.py（#9 已删除） `Store.alias`。
//!
//! 身份（`entities.id`）就是词条的 `terms.key`；新建的身份在建出来时就镜像成词条（`legacy_characters::mirror_state`），
//! 所以写名字时总能找到它的词条。`seq` 是名字的先后（旧表 `names` 的 rowid）：新行取当前最大值加一，与 SQLite 分配
//! rowid 的规则相同；`ON CONFLICT DO UPDATE` 保留原值。
use crate::{Error, Result};
use rusqlite::{params, Connection, OptionalExtension};

/// tests/reference/web/names.py（#9 已删除） PRIORITY 的来源。
const SOURCES: [&str; 7] = ["user", "bangumi", "index", "danbooru", "wiki", "builtin", "model"];

/// 一个身份的词条 id：`term_id = (SELECT id FROM terms WHERE key = ?)`。
pub const TERM_OF: &str = "(SELECT id FROM terms WHERE key = ?)";

/// 新一行名字的先后：当前最大值加一。
pub const NEXT_SEQ: &str = "(SELECT coalesce(max(seq), 0) + 1 FROM term_names)";

/// Python `str.strip()` 去掉的空白（比 Rust 的 White_Space 多 \x1c–\x1f）。
pub fn strip(value: &str) -> &str {
    value.trim_matches(|ch: char| ch.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&ch))
}

#[allow(clippy::too_many_arguments)]
pub fn put_name(
    c: &Connection,
    entity: &str,
    lang: &str,
    source: &str,
    value: &str,
    role: &str,
    position: i64,
    preferred: i64,
) -> Result<()> {
    if !matches!(lang, "" | "zh" | "ja" | "en")
        || (role == "display" && (lang.is_empty() || !SOURCES.contains(&source)))
    {
        return Err(Error::Invalid("名称来源或语言不正确".into()));
    }
    if source == "danbooru" && role == "display" {
        return Ok(());
    }
    let value = strip(value);
    if value.is_empty() {
        return Ok(());
    }
    if preferred != 0 && role == "display" {
        c.prepare_cached(&format!(
            "UPDATE term_names SET preferred = 0
             WHERE term_id = {TERM_OF} AND lang = ? AND role = 'display'"
        ))?
        .execute([entity, lang])?;
    }
    c.prepare_cached(&format!(
        "INSERT INTO term_names(term_id, lang, value, source, role, preferred, position, seq)
         VALUES ({TERM_OF}, ?, ?, ?, ?, ?, ?, {NEXT_SEQ})
         ON CONFLICT(term_id, role, lang, source, value) DO UPDATE SET preferred = max(preferred, excluded.preferred)"
    ))?
    .execute(params![entity, lang, value, source, role, preferred, position])?;
    Ok(())
}

/// `Store.alias`：只供搜索的名字；这个身份已有同值的别名就不再加。返回是否加了一行。
pub fn alias(c: &Connection, entity: &str, value: &str, source: &str, lang: &str) -> Result<bool> {
    let exists = c
        .prepare_cached(&format!(
            "SELECT 1 FROM term_names WHERE term_id = {TERM_OF} AND role = 'alias' AND value = ?"
        ))?
        .query_row([entity, strip(value)], |_| Ok(()))
        .optional()?
        .is_some();
    if exists || strip(value).is_empty() {
        return Ok(false);
    }
    put_name(c, entity, lang, source, value, "alias", 0, 0)?;
    Ok(true)
}
