//! 隔离 worker 和索引恢复日志的内部契约；序列化经 Value 保持现有键序与字节。
use crate::{app_tasks::TaskIdentity, Error, Result};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};

pub(crate) fn parse<T: DeserializeOwned>(value: Value, file: &Path) -> Result<T> {
    serde_path_to_error::deserialize(value)
        .map_err(|e| Error::Invalid(format!("{}: 字段 {}: {}", file.display(), e.path(), e.inner())))
}
pub(crate) fn read<T: DeserializeOwned>(file: &Path) -> Result<T> {
    let bytes = std::fs::read(file).map_err(|e| crate::error::io_at(e, file, None))?;
    let value = crate::strict_json::parse(
        std::str::from_utf8(&bytes).map_err(|e| Error::Invalid(format!("{}: {e}", file.display())))?,
    )
    .map_err(|e| Error::Invalid(format!("{}: {e}", file.display())))?;
    parse(value, file)
}
pub(crate) fn value<T: Serialize>(v: &T) -> Result<Value> {
    serde_json::to_value(v).map_err(|e| Error::Invalid(e.to_string()))
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) enum Format {
    #[serde(rename = "illustash-index/1")]
    Index,
    #[serde(rename = "illustash-sync/1")]
    Sync,
    #[serde(rename = "illustash-provenance/1")]
    Provenance,
    #[serde(rename = "illustash-research/1")]
    Research,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum IndexAction {
    Apply,
    Drop,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum JobPhase {
    Prepared,
    Ready,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct IndexRow {
    pub short_tag: String,
    pub tag: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct IndexResponse {
    pub format: Format,
    pub identity: TaskIdentity,
    pub before: String,
    pub characters: Vec<IndexCharacter>,
    pub aliases: Vec<(String, String, String)>,
    pub labels: Vec<(String, String, f64)>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct IndexCharacter {
    pub char_id: String,
    #[serde(flatten)]
    pub columns: Map<String, Value>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct IndexJob {
    pub version: u32,
    pub package: PathBuf,
    pub job_id: String,
    pub identity: TaskIdentity,
    pub action: IndexAction,
    pub reason: Option<String>,
    pub rows: Vec<IndexRow>,
    pub folder: PathBuf,
    pub before: String,
    pub phase: JobPhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<IndexResponse>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}
#[derive(Serialize)]
pub(crate) struct IndexRequest<'a> {
    pub format: Format,
    pub identity: &'a TaskIdentity,
    pub before: &'a str,
    pub action: IndexAction,
    pub rows: &'a [IndexRow],
    pub reason: &'a Option<String>,
    pub review: &'a Path,
}
#[derive(Serialize)]
pub(crate) struct SyncRequest<'a> {
    pub format: Format,
    pub identity: &'a TaskIdentity,
    pub assets: &'a [SyncAsset],
    pub output: &'a Path,
}
#[derive(Serialize, Deserialize)]
pub(crate) struct SyncAsset {
    pub id: String,
    pub path: String,
}
#[derive(Deserialize)]
pub(crate) struct SyncResponse {
    // Existing sync workers return identity/packages/failed without a format field.
    pub identity: TaskIdentity,
    pub packages: Vec<PathBuf>,
    pub failed: Vec<SyncFailure>,
    #[serde(default)]
    pub character_claims: Option<Vec<PathBuf>>,
}
#[derive(Deserialize)]
pub(crate) struct SyncFailure {
    pub uuid: String,
    /// Workers before #112 send no reason; their failures stay retryable, as before.
    #[serde(default)]
    pub reason: FailureReason,
}
/// Why the worker gave up on one picture. Decided by where the error came from, never by its text:
/// reading the file (retry next sync) or decoding bytes that were read in full (retry once they change).
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FailureReason {
    #[default]
    Unreadable,
    Undecodable,
}
#[derive(Deserialize)]
pub(crate) struct RecognitionManifest {
    pub assets: Vec<String>,
}
#[derive(Serialize)]
pub(crate) struct ProvenanceRequest<'a> {
    pub format: Format,
    pub identity: &'a TaskIdentity,
    pub owner_pid: u32,
    pub library: &'a Path,
    pub cache: &'a Path,
    pub step: &'a str,
    pub only: &'a [String],
    pub artists: &'a [String],
    pub network: bool,
    pub mailbox: &'a Path,
}
#[derive(Deserialize)]
pub(crate) struct ProvenanceResponse {
    pub format: Format,
    pub identity: TaskIdentity,
    pub step: String,
    pub result: ProvenanceResult,
}
#[derive(Serialize)]
pub(crate) struct ResearchRequest<'a> {
    pub format: Format,
    pub identity: &'a TaskIdentity,
    pub kind: &'a str,
    pub args: &'a Value,
    pub regroup: bool,
    pub snapshot: &'a Path,
    pub features: PathBuf,
    pub version: &'a str,
    pub learning_token: &'a Value,
    pub anchors: &'a Path,
    pub core: PathBuf,
    /// The owner accepts similarity caches as change lists against the snapshot (#31).
    pub delta_tables: bool,
}
#[derive(Deserialize)]
pub(crate) struct ResearchResponse {
    pub format: Format,
    pub identity: TaskIdentity,
    pub version: String,
    pub result: Map<String, Value>,
    #[serde(default)]
    pub learning: Option<Value>,
    #[serde(default)]
    pub tables: Option<Map<String, Value>>,
}

#[derive(Deserialize)]
pub(crate) struct NetRequest {
    pub identity: TaskIdentity,
    pub url: String,
}

#[derive(Serialize)]
#[serde(untagged)]
pub(crate) enum ProvenanceResult {
    Counts(Map<String, Value>),
    Hashes(Vec<HashEntry>),
}
// Dispatch by the JSON container, without an untagged deserialize retry that erases
// errors such as result[0].asset. serde_path_to_error can track these accessors.
impl<'de> Deserialize<'de> for ProvenanceResult {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = ProvenanceResult;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a result object or an array of asset/hash entries")
            }
            fn visit_map<M: serde::de::MapAccess<'de>>(self, map: M) -> std::result::Result<Self::Value, M::Error> {
                Map::deserialize(serde::de::value::MapAccessDeserializer::new(map)).map(ProvenanceResult::Counts)
            }
            fn visit_seq<S: serde::de::SeqAccess<'de>>(self, seq: S) -> std::result::Result<Self::Value, S::Error> {
                Vec::deserialize(serde::de::value::SeqAccessDeserializer::new(seq)).map(ProvenanceResult::Hashes)
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}
#[derive(Deserialize, Serialize)]
pub(crate) struct HashEntry {
    pub asset: String,
    pub hash: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn identity() -> Value {
        json!({"package":"/synthetic/package", "library_id":"synthetic", "generation":1, "session":"synthetic", "task_id":"synthetic"})
    }
    fn response(format: &str) -> Value {
        json!({"format":format, "identity":identity()})
    }
    fn malformed<T: DeserializeOwned>(valid: Value, keys: &[&str], file: &str) {
        assert!(parse::<T>(valid.clone(), Path::new(file)).is_ok(), "valid {file}");
        for key in keys {
            let mut missing = valid.clone();
            missing.as_object_mut().unwrap().remove(*key);
            let error = parse::<T>(missing, Path::new(file)).err().unwrap().to_string();
            assert!(error.contains(key) && error.contains(file), "{error}");
            let mut wrong = valid.clone();
            wrong[*key] = json!(false);
            let error = parse::<T>(wrong, Path::new(file)).err().unwrap().to_string();
            assert!(error.contains(key) && error.contains(file), "{error}");
        }
    }
    fn index() -> Value {
        let mut v = response("illustash-index/1");
        v["before"] = json!("synthetic");
        v["characters"] = json!([{ "char_id":"synthetic" }]);
        v["aliases"] = json!([["synthetic", "alias", "index"]]);
        v["labels"] = json!([["asset", "synthetic", 0.9]]);
        v
    }
    #[test]
    fn index_response_missing_and_wrong_fields_have_file_and_field() {
        malformed::<IndexResponse>(
            index(),
            &["format", "identity", "before", "characters", "aliases", "labels"],
            "index/response.json",
        );
        let mut v = index();
        v["characters"][0] = json!({});
        let error = parse::<IndexResponse>(v, Path::new("index/response.json")).unwrap_err().to_string();
        assert!(error.contains("characters[0]") && error.contains("char_id"), "{error}");
        let mut v = index();
        v["labels"][0][2] = json!("wrong");
        let error = parse::<IndexResponse>(v, Path::new("index/response.json")).unwrap_err().to_string();
        assert!(error.contains("labels[0][2]"), "{error}");
    }
    #[test]
    fn index_journal_missing_and_wrong_fields_have_file_and_field() {
        let v = json!({"version":1,"package":"/synthetic/package","job_id":"synthetic","identity":identity(),"action":"apply","reason":null,"rows":[{"short_tag":"synthetic","tag":"synthetic"}],"folder":"/synthetic/package/index-jobs/synthetic","before":"synthetic","phase":"prepared"});
        malformed::<IndexJob>(
            v.clone(),
            &["version", "package", "job_id", "identity", "action", "rows", "folder", "before", "phase"],
            "index-job.json",
        );
        let mut wrong = v;
        wrong["rows"][0]["tag"] = json!(3);
        let error = parse::<IndexJob>(wrong, Path::new("index-job.json")).unwrap_err().to_string();
        assert!(error.contains("rows[0].tag") && error.contains("index-job.json"), "{error}");
    }
    #[test]
    fn sync_response_missing_and_wrong_fields_have_file_and_field() {
        malformed::<SyncResponse>(
            json!({"identity":identity(),"packages":["/synthetic/output"],"failed":[{"uuid":"synthetic"}]}),
            &["identity", "packages", "failed"],
            "sync/response.json",
        );
        let v = json!({"identity":identity(),"packages":[],"failed":[{"uuid":2}]});
        let error = parse::<SyncResponse>(v, Path::new("sync/response.json")).err().unwrap().to_string();
        assert!(error.contains("failed[0].uuid"), "{error}");
    }
    #[test]
    fn sync_failure_reason_defaults_to_retry_and_rejects_unknown_values() {
        let v = json!({"identity":identity(),"packages":[],"failed":[{"uuid":"old"},{"uuid":"read","reason":"unreadable"},{"uuid":"bad","reason":"undecodable"}]});
        let reply = parse::<SyncResponse>(v, Path::new("sync/response.json")).unwrap();
        assert_eq!(
            reply.failed.iter().map(|f| (f.uuid.as_str(), f.reason)).collect::<Vec<_>>(),
            [
                ("old", FailureReason::Unreadable),
                ("read", FailureReason::Unreadable),
                ("bad", FailureReason::Undecodable)
            ]
        );
        let v = json!({"identity":identity(),"packages":[],"failed":[{"uuid":"x","reason":"broken"}]});
        let error = parse::<SyncResponse>(v, Path::new("sync/response.json")).err().unwrap().to_string();
        assert!(error.contains("failed[0].reason"), "{error}");
    }
    #[test]
    fn recognition_manifest_missing_and_wrong_fields_have_file_and_field() {
        malformed::<RecognitionManifest>(json!({"assets":["synthetic"]}), &["assets"], "package/manifest.json");
        let error = parse::<RecognitionManifest>(json!({"assets":[3]}), Path::new("package/manifest.json"))
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("assets[0]"), "{error}");
    }
    #[test]
    fn provenance_response_missing_and_wrong_fields_have_file_and_field() {
        let mut v = response("illustash-provenance/1");
        v["step"] = json!("groups");
        v["result"] = json!({"auto":0});
        malformed::<ProvenanceResponse>(v, &["format", "identity", "step", "result"], "provenance/response.json");
    }
    #[test]
    fn broker_request_missing_and_wrong_fields_have_file_and_field() {
        malformed::<NetRequest>(
            json!({"identity":identity(),"url":"https://synthetic.invalid"}),
            &["identity", "url"],
            "net-request.json",
        );
    }
    #[test]
    fn research_response_missing_and_wrong_fields_have_file_and_field() {
        let mut v = response("illustash-research/1");
        v["version"] = json!("synthetic");
        v["result"] = json!({});
        malformed::<ResearchResponse>(v, &["format", "identity", "version", "result"], "research/response.json");
    }
    #[test]
    fn serialized_index_request_and_journal_keep_json_bytes() {
        let v = json!({"version":1,"package":"/synthetic/package","job_id":"synthetic","identity":identity(),"action":"apply","reason":null,"rows":[{"short_tag":"synthetic","tag":"synthetic","post_count":20}],"folder":"/synthetic/package/index-jobs/synthetic","before":"synthetic","phase":"prepared"});
        let job: IndexJob = parse(v.clone(), Path::new("index-job.json")).unwrap();
        assert_eq!(value(&job).unwrap().to_string(), v.to_string());
        let request = IndexRequest {
            format: Format::Index,
            identity: &job.identity,
            before: &job.before,
            action: job.action,
            rows: &job.rows,
            reason: &job.reason,
            review: &job.folder,
        };
        let old = json!({"format":"illustash-index/1","identity":job.identity,"before":job.before,"action":"apply","rows":v["rows"],"reason":null,"review":job.folder});
        assert_eq!(value(&request).unwrap().to_string(), old.to_string());
    }
    #[test]
    fn hash_file_missing_and_wrong_fields_have_file_and_field() {
        malformed::<HashEntry>(json!({"asset":"synthetic","hash":"synthetic"}), &["asset", "hash"], "hashes.json");
    }
    #[test]
    fn remaining_requests_keep_original_json_bytes() {
        let identity: TaskIdentity = parse(identity(), Path::new("internal")).unwrap();
        let path = Path::new("/synthetic");
        let assets = [SyncAsset { id: "synthetic".into(), path: "/synthetic/image.png".into() }];
        let request = SyncRequest { format: Format::Sync, identity: &identity, assets: &assets, output: path };
        assert_eq!(value(&request).unwrap().to_string(), json!({"format":"illustash-sync/1","identity":identity,"assets":[{"id":"synthetic","path":"/synthetic/image.png"}],"output":path}).to_string());
        let only = ["synthetic".into()];
        let request = ProvenanceRequest {
            format: Format::Provenance,
            identity: &identity,
            owner_pid: 1,
            library: path,
            cache: path,
            step: "groups",
            only: &only,
            artists: &[],
            network: false,
            mailbox: path,
        };
        assert_eq!(value(&request).unwrap().to_string(), json!({"format":"illustash-provenance/1","identity":identity,"owner_pid":1,"library":path,"cache":path,"step":"groups","only":only,"artists":[],"network":false,"mailbox":path}).to_string());
        let args = json!({});
        let learning = json!("synthetic");
        let request = ResearchRequest {
            format: Format::Research,
            identity: &identity,
            kind: "similar",
            args: &args,
            regroup: false,
            snapshot: path,
            features: path.into(),
            version: "synthetic",
            learning_token: &learning,
            anchors: path,
            core: path.into(),
            delta_tables: true,
        };
        assert_eq!(value(&request).unwrap().to_string(), json!({"format":"illustash-research/1","identity":identity,"kind":"similar","args":args,"regroup":false,"snapshot":path,"features":path,"version":"synthetic","learning_token":learning,"anchors":path,"core":path,"delta_tables":true}).to_string());
    }
    #[test]
    fn provenance_hash_response_retains_nested_error_field() {
        for field in ["asset", "hash"] {
            for wrong in [false, true] {
                let mut v = response("illustash-provenance/1");
                v["step"] = json!("pixel_hash");
                v["result"] = json!([{"asset":"synthetic","hash":"synthetic"}]);
                if wrong {
                    v["result"][0][field] = json!(false);
                } else {
                    v["result"][0].as_object_mut().unwrap().remove(field);
                }
                let error =
                    parse::<ProvenanceResponse>(v, Path::new("provenance/response.json")).err().unwrap().to_string();
                assert!(
                    error.contains(field) && error.contains("result[0]") && error.contains("response.json"),
                    "{error}"
                );
            }
        }
    }
}
