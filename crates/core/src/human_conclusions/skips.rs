//! 身份对跳过只改本次新增的键；含分隔符的旧不透明键保留在文件中并报告依赖无法认回。
use crate::{Error, Result};
use rusqlite::{Connection, OptionalExtension};

pub(crate) fn pair(value: &str) -> Option<(&str, &str)> {
    let (a, b) = value.split_once('|')?;
    (!a.is_empty() && !b.is_empty() && a != b && !b.contains('|')).then_some((a, b))
}

pub(crate) fn stored(c: &Connection) -> Result<Option<String>> {
    Ok(c.query_row("SELECT value FROM prefs WHERE key='identity_skipped'", [], |r| r.get(0)).optional()?)
}

fn decode(value: Option<&str>) -> Result<Vec<String>> {
    let value = value.filter(|v| !v.is_empty()).unwrap_or("[]");
    serde_json::from_value(crate::strict_json::parse(value).map_err(Error::invalid)?)
        .map_err(|_| Error::invalid("身份跳过记录不是字符串数组"))
}

pub(crate) fn add(c: &Connection, pair: &str) -> Result<()> {
    let mut values = decode(stored(c)?.as_deref())?;
    if !values.iter().any(|v| v == pair) {
        values.push(pair.into());
    }
    c.execute(
        "INSERT OR REPLACE INTO prefs(key,value) VALUES ('identity_skipped',?1)",
        [serde_json::to_string(&values).expect("字符串数组可序列化")],
    )?;
    Ok(())
}

pub(crate) fn undo(c: &Connection, pairs: &[&str], before: Option<&str>) -> Result<()> {
    if pairs.is_empty() {
        return Ok(());
    }
    let mut values = decode(stored(c)?.as_deref())?;
    if pairs.iter().any(|p| !values.iter().any(|v| v == p)) {
        return Err(Error::Conflict("导入的身份跳过记录已改变".into()));
    }
    values.retain(|v| !pairs.contains(&v.as_str()));
    if values == decode(before)? {
        if let Some(before) = before {
            c.execute("UPDATE prefs SET value=?1 WHERE key='identity_skipped'", [before])?;
        } else {
            c.execute("DELETE FROM prefs WHERE key='identity_skipped'", [])?;
        }
    } else {
        c.execute(
            "UPDATE prefs SET value=?1 WHERE key='identity_skipped'",
            [serde_json::to_string(&values).expect("字符串数组可序列化")],
        )?;
    }
    Ok(())
}
