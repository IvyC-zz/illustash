//! 区分正值、明确收回和从未决定；无行状态与普通命令共用同一份撤销账本。
use crate::{error::invalid, Error, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// 具有“明确收回”语义的人工状态领域。
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Domain {
    /// 收录、排除，或交回自动规则。
    Curation,
    /// 暂时跳过，或恢复处理。
    Deferred,
    /// 身份未定，或清除未定状态。
    Uncertain,
}

impl Domain {
    pub(crate) fn key(self) -> &'static str {
        match self {
            Self::Curation => "curation",
            Self::Deferred => "deferred",
            Self::Uncertain => "uncertain",
        }
    }

    /// 该领域明确收回的便携状态名称。
    pub fn absent_state(self) -> &'static str {
        match self {
            Self::Curation => "reset",
            Self::Deferred => "resumed",
            Self::Uncertain => "cleared",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self> {
        match value {
            "curation" => Ok(Self::Curation),
            "deferred" => Ok(Self::Deferred),
            "uncertain" => Ok(Self::Uncertain),
            _ => invalid("不认识的人工状态领域"),
        }
    }
}

/// 明确收回的来源；action 属于本库，来源引用只用于审计。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mark {
    /// 本库中拥有这个状态的有效步骤。
    pub owner_action_id: i64,
    /// 人工来源：user、album 或 legacy。
    pub origin: String,
    /// 源快照记录的稳定引用；本地操作为空。
    pub source_ref: Option<String>,
    /// 原人工操作时间，不能用导入时间覆盖。
    pub created_at: String,
}

/// 导出和冲突检查共用的三态读取结果。
#[derive(Clone, Debug, PartialEq)]
pub enum State {
    /// 没有可证明的人工决定。
    Undecided,
    /// 业务表中的正值；只包含该领域的白名单列。
    Positive(Value),
    /// 人明确收回了这个状态。
    Absent(Mark),
}

pub(crate) fn positive(c: &Connection, domain: Domain, key: &str) -> Result<Option<Value>> {
    let value = match domain {
        Domain::Curation => c
            .query_row("SELECT status,origin,created_at FROM curation WHERE uuid=?1", [key], |r| {
                Ok(json!({"status": r.get::<_, String>(0)?, "origin": r.get::<_, String>(1)?,
                    "created_at": r.get::<_, String>(2)?}))
            })
            .optional()?,
        Domain::Deferred => {
            c.query_row("SELECT 1 FROM deferred WHERE uuid=?1", [key], |_| Ok(json!(true))).optional()?
        }
        Domain::Uncertain => c
            .query_row("SELECT options_json,reason,created_at FROM identity_uncertain WHERE uuid=?1", [key], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
            })
            .optional()?
            .map(|(options, reason, created_at)| -> Result<Value> {
                let options = crate::strict_json::parse(&options).map_err(Error::invalid)?;
                Ok(json!({"options": options, "reason": reason, "created_at": created_at}))
            })
            .transpose()?,
    };
    Ok(value)
}

fn mark(c: &Connection, domain: Domain, key: &str) -> Result<Option<Mark>> {
    Ok(c.query_row(
        "SELECT owner_action_id,origin,source_ref,created_at FROM human_state_marks
             WHERE domain=?1 AND asset_key=?2",
        params![domain.key(), key],
        |r| Ok(Mark { owner_action_id: r.get(0)?, origin: r.get(1)?, source_ref: r.get(2)?, created_at: r.get(3)? }),
    )
    .optional()?)
}

/// 读取一个领域的当前人工状态；正值和明确收回同时存在属于数据冲突。
pub fn read(c: &Connection, domain: Domain, key: &str) -> Result<State> {
    match (positive(c, domain, key)?, mark(c, domain, key)?) {
        (None, None) => Ok(State::Undecided),
        (Some(value), None) => Ok(State::Positive(value)),
        (None, Some(mark)) => Ok(State::Absent(mark)),
        (Some(_), Some(_)) => Err(Error::Conflict("人工状态同时包含正值和收回标记".into())),
    }
}

/// 记录本地明确收回；调用方先修改业务行，持有核心写钥匙及同一个写事务。
pub fn absent(c: &Connection, domain: Domain, key: &str, action: i64) -> Result<()> {
    let created_at = c.query_row("SELECT ts FROM actions WHERE id=?1 AND undone=0", [action], |r| r.get(0))?;
    set(
        c,
        domain,
        key,
        action,
        Some(Mark { owner_action_id: action, origin: "user".into(), source_ref: None, created_at }),
    )
}

/// 写入正值后清除旧的收回标记，并保存精确撤销信息。
pub fn present(c: &Connection, domain: Domain, key: &str, action: i64) -> Result<()> {
    set(c, domain, key, action, None)
}

/// 保存本次步骤的状态贡献；同一键重复写入保留最初 before 和最终 after。
/// 标记必须属于本步骤，业务行和标记在调用方的一个写事务中修改。
pub fn set(c: &Connection, domain: Domain, key: &str, action: i64, after: Option<Mark>) -> Result<()> {
    if c.is_autocommit() {
        return invalid("人工状态修改需要写事务");
    }
    let active: bool =
        c.query_row("SELECT EXISTS(SELECT 1 FROM actions WHERE id=?1 AND undone=0)", [action], |r| r.get(0))?;
    if !active || after.as_ref().is_some_and(|m| m.owner_action_id != action) {
        return invalid("人工状态必须属于当前有效步骤");
    }
    if after.is_some() && positive(c, domain, key)?.is_some() {
        return Err(Error::Conflict("正值仍在，不能写入收回标记".into()));
    }
    let before = mark(c, domain, key)?;
    if before == after {
        return Ok(());
    }
    c.execute(
        "INSERT INTO human_state_mark_undo(action_id,domain,asset_key,before_json,after_json)
         VALUES (?1,?2,?3,?4,?5) ON CONFLICT(action_id,domain,asset_key) DO UPDATE SET after_json=excluded.after_json",
        params![action, domain.key(), key, json!(before).to_string(), json!(after).to_string()],
    )?;
    replace(c, domain, key, after.as_ref())
}

pub(crate) fn replace(c: &Connection, domain: Domain, key: &str, value: Option<&Mark>) -> Result<()> {
    c.execute("DELETE FROM human_state_marks WHERE domain=?1 AND asset_key=?2", params![domain.key(), key])?;
    if let Some(m) = value {
        c.execute(
            "INSERT INTO human_state_marks(domain,asset_key,state,owner_action_id,origin,source_ref,created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![domain.key(), key, domain.absent_state(), m.owner_action_id, m.origin, m.source_ref, m.created_at],
        )?;
    }
    Ok(())
}

/// 业务行恢复后恢复本步骤的标记；有独立变更时失败，由外层事务整体回滚。
pub fn undo(c: &Connection, action: i64) -> Result<()> {
    if c.is_autocommit() {
        return invalid("人工状态撤销需要写事务");
    }
    let rows: Vec<(String, String, String, String)> = c
        .prepare("SELECT domain,asset_key,before_json,after_json FROM human_state_mark_undo WHERE action_id=?1 ORDER BY domain,asset_key")?
        .query_map([action], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (domain, key, before, after) in rows {
        let domain = Domain::parse(&domain)?;
        let decode = |s: &str| -> Result<Option<Mark>> {
            serde_json::from_str(s).map_err(|_| Error::Conflict("人工状态撤销记录不完整".into()))
        };
        let before = decode(&before)?;
        if mark(c, domain, &key)? != decode(&after)? || (before.is_some() && positive(c, domain, &key)?.is_some()) {
            return Err(Error::Conflict("人工状态在这一步之后已经改变".into()));
        }
        replace(c, domain, &key, before.as_ref())?;
    }
    Ok(())
}
