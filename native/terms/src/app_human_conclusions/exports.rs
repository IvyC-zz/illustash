//! 手动导出拥有独立取消槽；只保留本会话最近一次导出报告。
use super::*;
use crate::{
    app_imports::{Imports, Kind, Observer},
    app_library::Library,
    app_tasks::TaskIdentity,
    http_json::Json,
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

struct Report {
    id: String,
    summary: Value,
    details: Vec<Value>,
}
#[derive(Default)]
pub(crate) struct Exports {
    tasks: Imports,
    report: Arc<Mutex<Option<Report>>>,
}
impl Exports {
    pub(crate) fn running(&self) -> bool {
        self.tasks.running()
    }

    pub(crate) fn state(&self) -> Json {
        self.tasks.state()
    }
    pub(crate) fn start(&self, library: &Library, generation: u64, token: &str) -> Result<Json> {
        let identity = TaskIdentity::new(library, generation, token)?;
        let (db, library_id, report) = (library.db(), library.id().to_owned(), self.report.clone());
        self.tasks.start(identity, Kind::HumanExport, "", move |progress| {
            progress.stage("reading");
            let context = progress.context();
            let captured = capture_guarded(&db, &library_id, Mode::Fill, &|| context.check(), Some(context))?;
            let snapshot = &captured.snapshot;
            let dest = crate::app_paths::user_home()?
                .join("Downloads")
                .join(format!("Illustash-human-{}.illustash-human.jsonl", snapshot.header().snapshot_id));
            progress.stage("writing");
            let prepared = files::Prepared::new(snapshot, &dest, &|| context.check())?;
            let item = files::Item::from_snapshot(snapshot)?;
            let details: Vec<_> = snapshot
                .record_keys()
                .map(|(kind, key)| {
                    json!({
                        "kind": kind, "record_key": key, "result": "exported", "target_key": null, "reason": null
                    })
                })
                .collect();
            let assets = details.iter().filter(|detail| detail["kind"] == "asset").count();
            let definitions = details.iter().filter(|detail| detail["kind"] == "entity").count();
            let id = context.identity().task_id.clone();
            let summary = json!({
                "committed": true, "snapshot_id": item.backup_id, "report_id": id, "undo": null,
                "hash_coverage": item.hash_coverage, "portable_complete": item.portable_complete,
                "detail_count": details.len(), "path": dest, "pending_effects": false,
                "counts": {"exported": details.len() - assets - definitions},
                "asset_counts": {"exported": assets}, "definition_counts": {"exported": definitions},
                "definition_fields": {}
            });
            progress.stage("committing");
            let mut prepared = Some(prepared);
            progress.publish(&mut || prepared.take().expect("导出文件只发布一次").publish().map(|_| ()))?;
            *report.lock().expect("人工导出报告锁中毒") = Some(Report { id, summary: summary.clone(), details });
            Ok(summary.into())
        })
    }
    pub(crate) fn cancel(&self, id: &str) -> Result<Json> {
        if self.tasks.state().get("id").map(Json::text) != Some(id) {
            return Err(Error::Conflict("导出任务已变化".into()));
        }
        Ok(self.tasks.cancel("人工结论导出已取消"))
    }
    pub(crate) fn page(&self, db: &Path, id: &str, offset: usize, limit: usize) -> Result<Json> {
        let report = self.report.lock().expect("人工导出报告锁中毒");
        if let Some(report) = report.as_ref().filter(|r| r.id == id) {
            let details: Vec<_> = report.details.iter().skip(offset).take(limit).collect();
            return Ok(json!({"report":report.summary,"offset":offset,"limit":limit,"details":details}).into());
        }
        reports::page(db, id, offset, limit)
    }
    pub(crate) fn stop(&self) {
        self.tasks.shutdown(std::time::Duration::ZERO);
    }
}
impl Drop for Exports {
    fn drop(&mut self) {
        self.stop();
    }
}
