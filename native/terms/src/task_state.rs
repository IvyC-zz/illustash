//! 任务状态枚举和保留 HTTP 字段顺序的快照。外来 progress 必须先解析再发布。
use crate::{http_json::Json, worker_protocol, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Status {
    Idle,
    Off,
    Checking,
    Checked,
    Fetching,
    Building,
    Cleaning,
    Scanning,
    Recognising,
    NeedsRecognition,
    NeedsWindows,
    Importing,
    Interrupted,
    Error,
    Running,
    Stopping,
    Stopped,
    Failed,
    Done,
    Pairing,
    Comparing,
    Grouping,
    Learning,
    Reading,
    /// 研究输入在发布前发生了变化；稍后用新的快照重试。
    Waiting,
    /// 在发布任何内容之前应要求停下（或因切换、退出而停下）（#88 的导入）。
    Cancelled,
    /// 保留已有的进度，等用户确认联网后继续（#287：添加角色做到一半时没有联网授权）。
    Paused,
}
#[derive(Deserialize)]
struct Header {
    status: Status,
}
#[derive(Clone)]
pub(crate) struct State {
    status: Status,
    fields: Json,
}
impl State {
    pub fn parse(fields: Json, file: &Path) -> Result<Self> {
        let header: Header = worker_protocol::parse(
            serde_json::from_str(&fields.dump()).map_err(|e| crate::Error::Invalid(e.to_string()))?,
            file,
        )?;
        Ok(Self { status: header.status, fields })
    }
    pub fn status(&self) -> Status {
        self.status
    }
    pub fn snapshot(&self) -> Json {
        self.fields.clone()
    }
    pub fn value(&self) -> Value {
        serde_json::from_str(&self.fields.dump()).expect("task snapshot is valid JSON")
    }
    pub fn set(&mut self, key: &str, value: Json) {
        if key == "status" {
            self.status = serde_json::from_str(&value.dump()).expect("internal task status must be enumerated");
        }
        self.fields.set(key, value);
    }
    pub fn remove(&mut self, key: &str) {
        if let Json::Object(fields) = &mut self.fields {
            fields.retain(|(k, _)| k != key);
        }
    }
    pub fn merge(&mut self, value: Value) {
        if let Value::Object(fields) = value {
            for (key, value) in fields {
                self.set(&key, value.into());
            }
        }
    }
}
impl From<Json> for State {
    fn from(value: Json) -> Self {
        Self::parse(value, Path::new("internal task state"))
            .expect("internal task state must have an enumerated status")
    }
}
impl From<Value> for State {
    fn from(value: Value) -> Self {
        Self::from(Json::from(value))
    }
}
impl std::ops::Deref for State {
    type Target = Json;
    fn deref(&self) -> &Json {
        &self.fields
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn progress_state_rejects_missing_wrong_and_unknown_status_with_context() {
        for input in ["{}", r#"{"status":3}"#, r#"{"status":"unknown"}"#] {
            let fields = crate::http_json::parse_request(input).unwrap();
            let error = State::parse(fields, Path::new("research/progress.json")).err().unwrap().to_string();
            assert!(error.contains("status") && error.contains("progress.json"), "{error}");
        }
    }
    #[test]
    fn state_retains_http_order_and_existing_terminal_status() {
        let fields =
            crate::http_json::parse_request(r#"{"status":"scanning","started_at":"synthetic","done":1}"#).unwrap();
        let mut state = State::parse(fields.clone(), Path::new("internal")).unwrap();
        assert_eq!(state.snapshot().dump(), fields.dump());
        state.set("status", "needs_recognition".into());
        assert_eq!(state.status(), Status::NeedsRecognition);
    }
}
