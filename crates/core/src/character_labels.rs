//! 历史文案由核心生成（docs/TERMS_NAMES.md 方案 N5）：角色命令的文案逐字照 tests/reference/web/store.py（#9 已删除），用到的三样东西在这里。
//! - `Shown::name`：SQL 函数 `entity_name(id)`，即 `View.entity(id)['name']`，身份不存在时空串。名字已在核心时读显示
//!   投影（这个事务改过名字就先在事务里重建投影，`legacy_names::refresh_projection`）；名字还没切过来时用移植的 `View`
//!   （names_view，与旧代码全量逐字相等）在同一事务的数据上现算。
//! - `works`：`Store._works`，按复制品组去重的张数。
//! - `lang_of`：`names.lang_of`，打字的名字按假名 → ja、汉字 → zh、其余 → en 归语言。
use crate::names_view::View;
use crate::{legacy_names, Result};
use rusqlite::{Connection, OptionalExtension};
use std::collections::{HashMap, HashSet};

/// 一次命令里的名字读取。在写完名字之后再建（旧代码读的是改完之后的名字）。
#[derive(Default)]
pub struct Shown {
    view: Option<View>,
    /// 名字在核心时：已按这个 `names_version` 重建过投影。
    projected: Option<String>,
}

impl Shown {
    pub fn new() -> Shown {
        Shown::default()
    }

    /// `entity_name(id)`。
    pub fn name(&mut self, c: &Connection, id: &str) -> Result<String> {
        if legacy_names::names_in_core(c)? {
            legacy_names::refresh_projection(c, &mut self.projected)?;
            return Ok(c
                .prepare_cached("SELECT name FROM term_name_display WHERE entity=? AND skin_only=0")?
                .query_row([id], |r| r.get(0))
                .optional()?
                .unwrap_or_default());
        }
        if self.view.is_none() {
            self.view = Some(View::load(c)?);
        }
        let view = self.view.as_ref().expect("loaded");
        Ok(view.entity(id, false)?.map(|(choice, _)| choice.name).unwrap_or_default())
    }
}

/// `Store._works`：复制品只算一次（同组的按 `duplicates.grp` 算一张）。
pub fn works(c: &Connection, ids: &[String]) -> Result<usize> {
    let mut groups: HashMap<String, String> = HashMap::new();
    let mut stmt = c.prepare_cached("SELECT grp FROM duplicates WHERE uuid=?")?;
    for id in ids {
        if let Some(grp) = stmt.query_row([id], |r| r.get::<_, String>(0)).optional()? {
            groups.insert(id.clone(), grp);
        }
    }
    Ok(ids.iter().map(|i| groups.get(i).unwrap_or(i)).collect::<HashSet<_>>().len())
}

/// `names.lang_of`。
pub fn lang_of(value: &str) -> &'static str {
    if value.chars().any(|ch| ('\u{3040}'..='\u{30ff}').contains(&ch)) {
        "ja"
    } else if value.chars().any(|ch| ('\u{3400}'..='\u{9fff}').contains(&ch)) {
        "zh"
    } else {
        "en"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lang_of_follows_python() {
        assert_eq!(lang_of("アリス"), "ja");
        assert_eq!(lang_of("爱丽丝ス"), "ja");
        assert_eq!(lang_of("爱丽丝"), "zh");
        assert_eq!(lang_of("Alice"), "en");
        assert_eq!(lang_of("앨리스"), "en");
    }
}
