//! 相似组的「看过了」（#427）。组号随 `similar_groups` 重建而变，所以按成员记：一次标记记下当时这组里看得到的图。
//! 一组现在看得到的成员全部落在某一次标记里，就算看过；组里进了新图（包括两组连成一组）就不再算，回到列表。
//! 写入和撤销都不算学习输入（`app_research.rs` 的 `UNREAD`），撤销也不触发学习。

use crate::{
    app_commands,
    http_json::Json,
    sql_fragments::{rating, visible},
    Error, Result,
};
use illustash_core::error::invalid;
use rusqlite::{params, Connection, TransactionBehavior};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

/// 全部标记：每次标记的成员，以及每张图出现在哪几次标记里。
pub(crate) struct Seen {
    sets: HashMap<i64, HashSet<String>>,
    by_uuid: HashMap<String, Vec<i64>>,
}

impl Seen {
    pub(crate) fn load(c: &Connection) -> Result<Self> {
        let mut sets: HashMap<i64, HashSet<String>> = HashMap::new();
        let mut by_uuid: HashMap<String, Vec<i64>> = HashMap::new();
        let mut statement = c.prepare("SELECT id,uuid FROM similar_seen ORDER BY id,uuid")?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let (id, uuid): (i64, String) = (row.get(0)?, row.get(1)?);
            by_uuid.entry(uuid.clone()).or_default().push(id);
            sets.entry(id).or_default().insert(uuid);
        }
        Ok(Self { sets, by_uuid })
    }

    /// 包含全部 `members` 的那几次标记。
    pub(crate) fn covering<S: AsRef<str>>(&self, members: &[S]) -> Vec<i64> {
        let Some(first) = members.first() else {
            return Vec::new();
        };
        self.by_uuid
            .get(first.as_ref())
            .into_iter()
            .flatten()
            .copied()
            .filter(|id| self.sets.get(id).is_some_and(|set| members.iter().all(|u| set.contains(u.as_ref()))))
            .collect()
    }
}

/// 这一组在内容筛选 `nsfw` 下看得到的成员，和「相似的图」列表里的张数同一口径。
fn members(c: &Connection, gid: i64, nsfw: &str) -> Result<Vec<String>> {
    let sql = format!(
        "SELECT g.uuid FROM similar_groups g JOIN assets a ON a.uuid=g.uuid
         WHERE g.gid=? AND {}{} ORDER BY g.uuid",
        visible("a"),
        rating("a", nsfw)?
    );
    let members = c.prepare(&sql)?.query_map([gid], |r| r.get(0))?.collect::<rusqlite::Result<Vec<String>>>()?;
    Ok(members)
}

fn integer(json: Option<&Json>) -> Option<i64> {
    match json? {
        Json::Scalar(Value::Number(number)) => number.as_i64(),
        Json::Integer(number) => number.parse().ok(),
        _ => None,
    }
}

/// `POST /api/similar/seen {"gid", "size", "nsfw", "seen": true|false}`：标为看过，或者标为没看过。
/// `size` 是界面显示的张数；和现在看得到的成员数对不上说明组刚变过，拒绝，免得把没看到的新图一起标掉。
pub(crate) fn write(c: &mut Connection, body: &Json) -> Result<Json> {
    app_commands::initialized(c)?;
    let gid = integer(body.get("gid")).ok_or_else(|| Error::invalid("相似组不存在"))?;
    let size = integer(body.get("size")).ok_or_else(|| Error::invalid("参数不正确"))?;
    let nsfw = match body.get("nsfw") {
        None => "",
        Some(Json::Scalar(Value::String(nsfw))) => nsfw,
        Some(_) => return invalid("未知内容筛选"),
    };
    let want = match body.get("seen") {
        Some(Json::Scalar(Value::Bool(seen))) => *seen,
        _ => return invalid("参数不正确"),
    };
    let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let members = members(&tx, gid, nsfw)?;
    if members.len() < 2 {
        return invalid("相似组不存在");
    }
    if i64::try_from(members.len()).ok() != Some(size) {
        return Err(Error::Conflict("这组刚有变化，请重新看一下".into()));
    }
    let covering = Seen::load(&tx)?.covering(&members);
    let (before, label) = if want {
        if !covering.is_empty() {
            return invalid("这组已经标为看过");
        }
        let id: i64 = tx.query_row("SELECT coalesce(max(id),0)+1 FROM similar_seen", [], |r| r.get(0))?;
        let now: String = tx.query_row("SELECT CURRENT_TIMESTAMP", [], |r| r.get(0))?;
        let mut insert = tx.prepare("INSERT INTO similar_seen(id,uuid,seen_at) VALUES (?,?,?)")?;
        for uuid in &members {
            insert.execute(params![id, uuid, now])?;
        }
        (json!({"id": id, "rows": null}), "已标为看过")
    } else {
        if covering.is_empty() {
            return invalid("这组没有标为看过");
        }
        // 只去掉这组的图；同一次标记里的其他图（组后来拆开的另一部分）照旧算看过。
        let mut removed = Vec::new();
        let mut select = tx.prepare("SELECT seen_at FROM similar_seen WHERE id=? AND uuid=?")?;
        let mut delete = tx.prepare("DELETE FROM similar_seen WHERE id=? AND uuid=?")?;
        for id in &covering {
            for uuid in &members {
                let seen_at: String = select.query_row(params![id, uuid], |r| r.get(0))?;
                delete.execute(params![id, uuid])?;
                removed.push(json!([id, uuid, seen_at]));
            }
        }
        (json!({"id": null, "rows": removed}), "已标为没看过")
    };
    let snapshot = json!([{ "similar_seen": before }]);
    tx.execute("INSERT INTO actions(label,before_json) VALUES (?,?)", [label, &snapshot.to_string()])?;
    let undo = app_commands::last_action(&tx)?;
    tx.commit()?;
    Ok(Json::object([("undo", undo)]))
}

/// 撤销一次标记（删掉这次标记的行），或者撤销一次「标为没看过」（放回去掉的行）。
pub(crate) fn restore(c: &Connection, before: &Value) -> Result<()> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Before {
        id: Option<i64>,
        rows: Option<Vec<(i64, String, String)>>,
    }
    let unknown = || Error::Io(std::io::Error::other("「看过了」的历史不完整"));
    let before: Before = serde_json::from_value(before.clone())
        .map_err(|e| Error::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e)))?;
    match (before.id, before.rows) {
        (Some(id), None) => {
            c.execute("DELETE FROM similar_seen WHERE id=?", [id])?;
        }
        (None, Some(rows)) => {
            let mut insert = c.prepare("INSERT INTO similar_seen(id,uuid,seen_at) VALUES (?,?,?)")?;
            for (id, uuid, seen_at) in rows {
                insert.execute(params![id, uuid, seen_at])?;
            }
        }
        _ => return Err(unknown()),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn covering_needs_every_member_in_one_mark() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE similar_seen(id INTEGER,uuid TEXT,seen_at TEXT);
             INSERT INTO similar_seen VALUES (1,'a','t'),(1,'b','t'),(1,'c','t'),(2,'d','t'),(2,'e','t');",
        )
        .unwrap();
        let seen = Seen::load(&c).unwrap();
        assert_eq!(seen.covering(&["a", "b"]), [1], "排除掉一张后仍算看过");
        assert_eq!(seen.covering(&["a", "b", "c"]), [1]);
        assert!(seen.covering(&["a", "b", "x"]).is_empty(), "进了新图");
        assert!(seen.covering(&["a", "b", "d", "e"]).is_empty(), "两组连成一组");
        assert!(seen.covering::<&str>(&[]).is_empty());
    }
}
