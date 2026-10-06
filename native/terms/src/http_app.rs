//! Local application reads. Session metadata comes from the existing private startup state.
use crate::{
    http::LibraryContext,
    http_json::{parse_python, Json},
    http_read::{display, rows, strings, VISIBLE},
    Error, Result,
};
use rusqlite::Connection;
use std::collections::{HashMap, HashSet};

pub(crate) fn rating(alias: &str, nsfw: &str) -> Result<String> {
    if !["", "hide", "only"].contains(&nsfw) {
        return Err(Error::Invalid("未知内容筛选".into()));
    }
    Ok(if nsfw.is_empty() {
        String::new()
    } else {
        format!(" AND {}coalesce({alias}.uuid IN (SELECT uuid FROM asset_rating WHERE rating IN ('questionable', 'explicit')),0)", if nsfw=="hide" { "NOT " } else { "" })
    })
}
fn entity_name(c: &Connection, id: &str) -> Result<Option<String>> {
    let value = display(c, Json::object([("id", id.into())]))?;
    Ok(value.get("name").map(|v| v.text().to_owned()))
}
fn digits(s: &str) -> (&str, &str) {
    let n = s
        .char_indices()
        .take_while(|(_, c)| crate::compat_unicode::decimal(*c).is_some())
        .last()
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(0);
    s.split_at(n)
}
pub(crate) fn action_label(c: &Connection, r: &Json) -> Result<String> {
    let label = r.get("label").expect("http_app internal invariant").text();
    let before = parse_python(r.get("before_json").expect("http_app internal invariant").text())?;
    let before = if let Json::Array(v) = &before {
        if v.len() == 1 && matches!(&v[0], Json::Object(fields) if fields.len()==1) {
            v[0].get("terms")
                .filter(|t| t.get("v").is_some_and(|v| v.integer() == 2))
                .and_then(|t| t.get("before"))
                .unwrap_or(&before)
        } else {
            &before
        }
    } else {
        &before
    };
    let Json::Array(before) = before else { return Err(Error::Invalid("历史快照应为列表".into())) };
    let merge = before.iter().find_map(|b| b.get("merge"));
    if let Some(merge) = merge.filter(|v| match v {
        Json::Object(v) => !v.is_empty(),
        Json::Array(v) => !v.is_empty(),
        Json::Scalar(v) => crate::character_commands::truthy(v),
        Json::Integer(v) => v != "0",
        Json::Float(v) => *v != 0.0,
    }) {
        let field = |key| {
            merge
                .get(key)
                .ok_or_else(|| Error::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, "历史合并快照缺少字段")))
        };
        if let (Some(left), Some(right)) =
            (entity_name(c, field("source")?.text())?, entity_name(c, field("target")?.text())?)
        {
            let moved = match field("moved")? {
                Json::Array(v) => v.len(),
                Json::Object(v) => v.len(),
                Json::Scalar(serde_json::Value::String(v)) => v.chars().count(),
                _ => return Err(Error::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, "历史移动列表无效"))),
            };
            return Ok(format!("已将 {left} 合并到 {right}（{moved} 条人工结论）"));
        }
    }
    let mut seen = HashSet::new();
    let mut ids: Vec<String> = before
        .iter()
        .filter_map(|b| b.get("entity"))
        .map(|v| v.text().to_owned())
        .filter(|v| !v.is_empty() && seen.insert(v.clone()))
        .collect();
    let swap = label.strip_prefix("已将 ").and_then(|s| {
        let (n, rest) = digits(s);
        (!n.is_empty() && rest.starts_with(" 张从 ") && rest.contains(" 换成 ")).then_some(n)
    });
    if let Some(n) = swap.filter(|_| ids.len() == 2) {
        if let (Some(left), Some(right)) = (entity_name(c, &ids[0])?, entity_name(c, &ids[1])?) {
            return Ok(format!("已将 {n} 张从 {left} 换成 {right}"));
        }
    }
    let sweep = label
        .strip_prefix("已将 ")
        .and_then(|s| s.rsplit_once(" 标为已检查（"))
        .and_then(|(_, s)| s.strip_suffix(" 张）"))
        .filter(|s| !s.is_empty() && digits(s).1.is_empty());
    if sweep.is_some() && ids.is_empty() {
        ids = strings(
            c,
            "SELECT entity FROM reviews WHERE action_id=?",
            &[&r.get("id").expect("http_app internal invariant").integer().to_string()],
        )?;
    }
    if ids.len() == 1 {
        if let Some(name) = entity_name(c, &ids[0])? {
            if let Some(n) = sweep {
                return Ok(format!("已将 {name} 标为已检查（{n} 张）"));
            }
            for (start, ends) in
                [("已将 ", &[" 张设为 ", " 张移出 "][..]), ("已给 ", &[" 张加上 "][..]), ("已从 ", &[" 张去掉 "][..])]
            {
                if let Some(s) = label.strip_prefix(start) {
                    let (n, rest) = digits(s);
                    if !n.is_empty() {
                        for end in ends {
                            if rest.starts_with(end) {
                                return Ok(format!("{start}{n}{end}{name}"));
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(label.into())
}
pub fn read(c: &Connection, context: &LibraryContext, q: &HashMap<String, String>) -> Result<(Json, u16)> {
    let nsfw = q.get("nsfw").map(String::as_str).unwrap_or("");
    let rated = rating("a", nsfw)?;
    let visible = VISIBLE.replace("m.uuid", "a.uuid");
    let total: i64 = c.query_row(&format!("SELECT count(*) FROM assets a WHERE {visible}{rated}"), [], |r| r.get(0))?;
    let hybrid: bool =
        c.query_row("SELECT EXISTS(SELECT 1 FROM meta WHERE key='identity_policy' AND value='hybrid')", [], |r| {
            r.get(0)
        })?;
    let confirmed = if hybrid {
        "SELECT uuid FROM family_confirmed"
    } else {
        "SELECT uuid FROM assertions WHERE status='confirmed'"
    };
    let confirmed: i64 = c.query_row(
        &format!("SELECT count(DISTINCT uuid) FROM ({confirmed}) a JOIN assets USING(uuid) WHERE {visible}"),
        [],
        |r| r.get(0),
    )?;
    let indexed: String = c.query_row("SELECT value FROM meta WHERE key='indexed'", [], |r| r.get(0))?;
    let undo = if let Some(r) =
        rows(c, "SELECT id,label,ts,before_json FROM actions WHERE undone=0 ORDER BY id DESC LIMIT 1", &[])?
            .into_iter()
            .next()
    {
        Json::object([
            ("id", r.get("id").expect("http_app internal invariant").clone()),
            ("label", action_label(c, &r)?.into()),
            ("ts", r.get("ts").expect("http_app internal invariant").clone()),
        ])
    } else {
        None::<String>.into()
    };
    let read_only = if let Some(reason) = &context.read_only {
        Some(reason.clone())
    } else {
        let switched:bool=c.query_row("SELECT count(*)=4 FROM meta WHERE key IN ('terms_artists','terms_labels','terms_characters','terms_names') AND value='core'",[],|r|r.get(0))?;
        if switched {
            None
        } else {
            Some("图库正在切换到词条核心，完成前只读".into())
        }
    };
    Ok((
        Json::object([
            ("total", total.into()),
            ("confirmed", confirmed.into()),
            ("indexed", indexed.into()),
            ("undo", undo),
            ("token", context.token.clone().into()),
            ("read_only", read_only.into()),
        ]),
        200,
    ))
}
