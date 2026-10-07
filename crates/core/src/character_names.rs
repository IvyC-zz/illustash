//! 名字这一步不收编（方案第 10 节）：`names` 仍是 Python 的表。核心命令需要写用户名字和别名时，逐行照
//! tests/reference/web/names.py（#9 已删除） `put_name` 与 tests/reference/web/store.py（#9 已删除） `Store.alias` 的语义写。
use crate::{Error, Result};
use rusqlite::{params, Connection, OptionalExtension};

/// tests/reference/web/names.py（#9 已删除） PRIORITY 的来源。
const SOURCES: [&str; 7] = ["user", "bangumi", "index", "danbooru", "wiki", "builtin", "model"];

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
        c.prepare_cached("UPDATE names SET preferred=0 WHERE entity_id=? AND lang=? AND role='display'")?
            .execute([entity, lang])?;
    }
    c.prepare_cached(
        "INSERT INTO names(entity_id,lang,value,source,role,preferred,position) VALUES (?,?,?,?,?,?,?)
         ON CONFLICT(entity_id,role,lang,source,value) DO UPDATE SET preferred=max(preferred,excluded.preferred)",
    )?
    .execute(params![entity, lang, value, source, role, preferred, position])?;
    Ok(())
}

/// `Store.alias`：只供搜索的名字；这个身份已有同值的别名就不再加。返回是否加了一行。
pub fn alias(c: &Connection, entity: &str, value: &str, source: &str, lang: &str) -> Result<bool> {
    let exists = c
        .prepare_cached("SELECT 1 FROM names WHERE entity_id=? AND role='alias' AND value=?")?
        .query_row([entity, strip(value)], |_| Ok(()))
        .optional()?
        .is_some();
    if exists || strip(value).is_empty() {
        return Ok(false);
    }
    put_name(c, entity, lang, source, value, "alias", 0, 0)?;
    Ok(true)
}
