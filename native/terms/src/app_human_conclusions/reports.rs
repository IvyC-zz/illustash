//! 导入报告与导入事务一起提交，重启后仍可按序分页读取完整明细。
use super::*;
use crate::http_json::Json;
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};

pub(super) fn summary(id: &str, snapshot: &Snapshot, report: &Report) -> Value {
    json!({"committed":true,"snapshot_id":snapshot.header().snapshot_id,"report_id":id,"undo":report.action_id,
        "counts":report.records,"asset_counts":report.assets,"definition_counts":report.definitions,
        "definition_fields":report.definition_fields,"hash_coverage":snapshot.coverage(),"portable_complete":snapshot.coverage().missing==0,
        "detail_count":report.details.len(),"pending_effects":report.action_id.is_some()})
}
pub(super) fn save(
    c: &Connection,
    id: &str,
    summary: &Value,
    report: &Report,
    check: &dyn Fn() -> Result<()>,
) -> Result<()> {
    c.execute(
        "INSERT INTO human_import_reports(id,action_id,summary_json) VALUES (?1,?2,?3)",
        params![id, report.action_id, summary.to_string()],
    )?;
    let mut insert = c.prepare("INSERT INTO human_import_report_rows(report_id,seq,detail_json) VALUES (?1,?2,?3)")?;
    for (seq, detail) in report.details.iter().enumerate() {
        check()?;
        insert.execute(params![id, seq as i64, serde_json::to_string(detail).expect("导入明细可序列化")])?;
    }
    Ok(())
}
pub(super) fn page(db: &Path, id: &str, offset: usize, limit: usize) -> Result<Json> {
    let c = crate::app_library::read_connection(db)?;
    let summary: Option<String> =
        c.query_row("SELECT summary_json FROM human_import_reports WHERE id=?1", [id], |r| r.get(0)).optional()?;
    let Some(summary) = summary else { return Err(Error::invalid("报告不存在")) };
    let mut summary: Value = serde_json::from_str(&summary).map_err(|_| Error::invalid("报告摘要不完整"))?;
    summary["pending_effects"] = json!(pending(&c));
    let rows = c
        .prepare(
            "SELECT detail_json FROM human_import_report_rows WHERE report_id=?1 AND seq>=?2 ORDER BY seq LIMIT ?3",
        )?
        .query_map(params![id, offset as i64, limit as i64], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let details = rows
        .into_iter()
        .map(|s| serde_json::from_str::<Value>(&s).map_err(|_| Error::invalid("报告明细不完整")))
        .collect::<Result<Vec<_>>>()?;
    Ok(json!({"report":summary,"offset":offset,"limit":limit,"details":details}).into())
}
pub(super) fn pending(c: &Connection) -> bool {
    c.query_row("SELECT EXISTS(SELECT 1 FROM backend_effects WHERE kind='verdict' AND status='pending')", [], |r| {
        r.get(0)
    })
    .unwrap_or_else(|error| {
        // 导入已经提交；后续状态读取失败不能伪称效果已完成，也不能改报零提交。
        eprintln!("人工导入效果状态暂不可读：{error}");
        true
    })
}
