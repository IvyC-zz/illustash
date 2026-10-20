//! Synthetic formal-import regression; no models, private anchors, or external libraries.
use super::*;
use crate::{
    app_tasks::TaskIdentity,
    recognition_compute::{anchor_snapshot::Snapshot, claims::Claims, producer},
};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture {
    root: PathBuf,
    resources: PathBuf,
    library: Library,
    identity: TaskIdentity,
}
impl Fixture {
    fn new(anchored: bool) -> Self {
        let root = std::env::temp_dir().join(format!(
            "illustash-claims-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let images = root.join("images");
        fs::create_dir(&images).unwrap();
        let package = root.join("synthetic.pilib");
        crate::app_create::create_folder(&package, &images, None, None, None).unwrap();
        let library = Library::open(&package).unwrap();
        let src = crate::sqlite::open(library.source()).unwrap();
        src.execute_batch("INSERT INTO assets(uuid,filename,src_path,width,height,added_at,src_kind,in_scope,trashed) VALUES ('a','synthetic.png','synthetic.png',16,16,'2026-10-03','folder',1,0);
          INSERT INTO characters(char_id,anchor_p90,anchor_p75,post_count,gender_conf) VALUES ('old',0.05,0.02,10,1),('new',0.05,0.02,20,1);
          INSERT INTO labels(uuid,dim,value,conf,source) VALUES ('a','char_ext','old',0.9,'char_index');").unwrap();
        let mut app = crate::sqlite::open(library.db()).unwrap();
        let tx = app.transaction_with_behavior(TransactionBehavior::Immediate).unwrap();
        crate::schema::begin_writing(&tx).unwrap();
        import_changes(&tx, &src, &library, &["a".into()]).unwrap();
        let entity: String =
            tx.query_row("SELECT entity FROM candidates WHERE uuid='a' AND source='CCIP'", [], |r| r.get(0)).unwrap();
        crate::character_commands::command(
            &tx,
            &json!({"op":"change","asked":["a"],"entity":entity,"action":"rejected"}),
        )
        .unwrap();
        crate::schema::commit_writing(tx).unwrap();
        let resources = root.join("resources");
        for p in ["models/ccip", "tags", "review"] {
            fs::create_dir_all(resources.join(p)).unwrap();
        }
        fs::write(resources.join("models/wd_swinv2_v3_emb.onnx"), b"synthetic-WD").unwrap();
        fs::write(resources.join("models/ccip/model_feat.onnx"), b"synthetic-CCIP").unwrap();
        fs::write(resources.join("tags/tags_v3.csv"), b"name,category\ngeneral,9\n").unwrap();
        if anchored {
            fs::write(
                resources.join("review/char-anchors.npy"),
                producer::npy(&[feature(0), feature(1)], 768).unwrap(),
            )
            .unwrap();
            fs::write(resources.join("review/char-anchors.txt"), b"old\nnew\n").unwrap();
        }
        let identity = TaskIdentity::new(&library, 1, "synthetic-session").unwrap();
        Self { root, resources, library, identity }
    }
    fn source(&self) -> Connection {
        crate::sqlite::open(self.library.source()).unwrap()
    }
    fn app(&self) -> Connection {
        crate::sqlite::open(self.library.db()).unwrap()
    }
    fn batch(&self, name: &str, feature: Vec<f32>) -> (PathBuf, PathBuf) {
        let path = self.root.join(name);
        let models = model_specs(&self.resources).unwrap();
        let ids = vec!["a".into()];
        producer::write_batch(
            &path,
            &json!(self.identity),
            &models,
            &ids,
            &[producer::Label { asset: "a".into(), dim: "rating".into(), value: "general".into(), confidence: 0.9 }],
            &[vec![1.; 1024]],
            std::slice::from_ref(&feature),
        )
        .unwrap();
        let snapshot = Snapshot::load(&self.resources.join("review"), &self.library.source()).unwrap();
        let claims = snapshot.claims("a", &feature).unwrap();
        let sidecar = path.join("character-claims.json");
        fs::write(
            &sidecar,
            serde_json::to_vec(&Claims::new(self.identity.clone(), ids, models, snapshot.fingerprint, claims)).unwrap(),
        )
        .unwrap();
        (path, sidecar)
    }
    fn import(&self, path: &Path, sidecar: &Path) -> Result<Value> {
        let cancel = AtomicBool::new(false);
        let inputs = WorkerInputs::load(&self.library, &self.identity, &self.resources, &cancel)?;
        let prepared = prepare_worker_package(&self.library, path, sidecar, &inputs, &cancel)?;
        import_worker_package(&self.library, &prepared, &inputs, &cancel, &|_, _| Ok(()))
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}
fn feature(i: usize) -> Vec<f32> {
    let mut row = vec![0.; 768];
    row[i] = 1.;
    row
}
fn count(c: &Connection, sql: &str) -> i64 {
    c.query_row(sql, [], |r| r.get(0)).unwrap()
}
fn human(c: &Connection) -> Vec<Value> {
    crate::app_migrate_data::rows(c, "SELECT * FROM term_verdicts ORDER BY id").unwrap()
}
fn assert_no_wd(f: &Fixture) {
    assert_eq!(count(&f.source(), "SELECT count(*) FROM labels WHERE source='wd_v3'"), 0);
    assert_eq!(count(&f.source(), "SELECT count(*) FROM embeddings"), 0);
    assert_eq!(count(&f.source(), "SELECT count(*) FROM sqlite_master WHERE name='recognition_imports'"), 0);
}
#[test]
fn native_claims_replace_effective_candidate_preserve_human_and_deduplicate() {
    let f = Fixture::new(true);
    let before = human(&f.app());
    let (path, sidecar) = f.batch("first", feature(1));
    assert_eq!(f.import(&path, &sidecar).unwrap()["status"], "done");
    assert_eq!(count(&f.source(), "SELECT count(*) FROM labels WHERE dim='char_ext' AND value='old'"), 0);
    assert_eq!(count(&f.source(), "SELECT count(*) FROM labels WHERE dim='char_ext' AND value='new'"), 1);
    assert_eq!(count(&f.app(),"SELECT count(*) FROM candidates c JOIN terms t ON t.key=c.entity JOIN term_links l ON l.term_id=t.id AND l.system='ccip' AND l.key='old' WHERE c.source='CCIP'"),0);
    assert_eq!(count(&f.app(),"SELECT count(*) FROM term_current_evidence WHERE source='ccip_index' AND method='char_ext' AND metric='ccip_score'"),1);
    assert_eq!(human(&f.app()), before);
    let events = count(&f.app(), "SELECT count(*) FROM backend_effects");
    assert_eq!(f.import(&path, &sidecar).unwrap()["duplicate"], true);
    assert_eq!(count(&f.app(), "SELECT count(*) FROM backend_effects"), events);
    assert_eq!(human(&f.app()), before);
}
#[test]
fn native_empty_claims_clear_old_machine_candidate_but_no_anchors_preserve_it() {
    for anchored in [true, false] {
        let f = Fixture::new(anchored);
        let before = human(&f.app());
        let (path, sidecar) = f.batch("empty", feature(2));
        assert_eq!(f.import(&path, &sidecar).unwrap()["status"], "done");
        assert_eq!(
            count(&f.source(), "SELECT count(*) FROM labels WHERE source='char_index' AND dim='char_ext'"),
            i64::from(!anchored)
        );
        assert_eq!(count(&f.app(), "SELECT count(*) FROM candidates WHERE source='CCIP'"), i64::from(!anchored));
        assert_eq!(human(&f.app()), before);
        if !anchored {
            assert_eq!(
                import_package(&f.library, &path, &f.resources, &AtomicBool::new(false), &|_, _| Ok(())).unwrap()
                    ["duplicate"],
                true
            );
        }
    }
}
#[test]
fn missing_tampered_stale_and_switched_claims_refuse_before_any_wd_write() {
    for case in ["missing", "tampered", "stale-anchors", "stale-role", "stale-model", "switched", "late-session"] {
        let f = Fixture::new(true);
        let (path, sidecar) = f.batch("bad", feature(1));
        match case {
            "missing" => fs::remove_file(&sidecar).unwrap(),
            "stale-anchors" => fs::write(f.resources.join("review/char-anchors.txt"), b"new\nold\n").unwrap(),
            "stale-role" => {
                f.source().execute("UPDATE characters SET post_count=post_count+1 WHERE char_id='new'", []).unwrap();
            }
            "stale-model" => fs::write(f.resources.join("models/ccip/model_feat.onnx"), b"changed").unwrap(),
            _ => {
                let mut value: Value = serde_json::from_slice(&fs::read(&sidecar).unwrap()).unwrap();
                match case {
                    "tampered" => value["claims"][0]["confidence"] = json!(0.5),
                    "switched" => value["identity"]["package"] = json!(f.root),
                    "late-session" => value["identity"]["session"] = json!("stale"),
                    _ => unreachable!(),
                }
                fs::write(&sidecar, serde_json::to_vec(&value).unwrap()).unwrap();
            }
        }
        assert!(f.import(&path, &sidecar).is_err(), "{case}");
        assert_no_wd(&f);
    }
}
#[test]
fn source_commit_projection_crash_and_dispatch_failure_use_existing_resume() {
    for projection_failure in [true, false] {
        let f = Fixture::new(true);
        let before = human(&f.app());
        let (path, sidecar) = f.batch("pending", feature(1));
        if projection_failure {
            f.app().execute_batch("CREATE TRIGGER synthetic_crash BEFORE INSERT ON assets BEGIN SELECT RAISE(ABORT,'synthetic projection crash'); END;").unwrap();
        }
        let cancel = AtomicBool::new(false);
        let inputs = WorkerInputs::load(&f.library, &f.identity, &f.resources, &cancel).unwrap();
        let prepared = prepare_worker_package(&f.library, &path, &sidecar, &inputs, &cancel).unwrap();
        let result = import_worker_package(&f.library, &prepared, &inputs, &cancel, &|_, _| {
            Err(Error::Invalid("synthetic dispatch crash".into()))
        })
        .unwrap();
        assert_eq!(result["status"], "pending");
        assert_eq!(count(&f.source(), "SELECT count(*) FROM recognition_imports WHERE status='pending'"), 1);
        assert_eq!(count(&f.source(), "SELECT count(*) FROM labels WHERE source='wd_v3'"), 1);
        if projection_failure {
            f.app().execute_batch("DROP TRIGGER synthetic_crash").unwrap();
        }
        assert_eq!(resume(&f.library, &AtomicBool::new(false), &|_, _| Ok(())).unwrap()["status"], "done");
        assert_eq!(human(&f.app()), before);
        assert_eq!(count(&f.app(), "SELECT count(*) FROM backend_effects WHERE kind='imported'"), 1);
        assert_eq!(f.import(&path, &sidecar).unwrap()["duplicate"], true);
        assert_eq!(count(&f.app(), "SELECT count(*) FROM backend_effects WHERE kind='imported'"), 1);
    }
}

#[test]
fn task_preflight_reuses_snapshot_and_commit_refuses_late_changes() {
    for case in ["sidecar", "payload", "role", "anchors", "models", "cancel"] {
        let f = Fixture::new(true);
        let (path, sidecar) = f.batch("late", feature(1));
        let cancel = AtomicBool::new(false);
        let inputs = WorkerInputs::load(&f.library, &f.identity, &f.resources, &cancel).unwrap();
        let prepared = prepare_worker_package(&f.library, &path, &sidecar, &inputs, &cancel).unwrap();
        match case {
            "sidecar" => {
                let mut v: Value = serde_json::from_slice(&fs::read(&sidecar).unwrap()).unwrap();
                v["claims"][0]["confidence"] = json!(0.5);
                fs::write(&sidecar, serde_json::to_vec(&v).unwrap()).unwrap();
            }
            "payload" => fs::write(path.join("wd.npy"), b"changed").unwrap(),
            "role" => {
                f.source().execute("UPDATE characters SET post_count=post_count+1", []).unwrap();
            }
            "anchors" => fs::write(f.resources.join("review/char-anchors.txt"), b"new\nold\n").unwrap(),
            "models" => fs::write(f.resources.join("models/ccip/model_feat.onnx"), b"changed").unwrap(),
            "cancel" => cancel.store(true, Ordering::SeqCst),
            _ => unreachable!(),
        }
        assert!(import_worker_package(&f.library, &prepared, &inputs, &cancel, &|_, _| Ok(())).is_err(), "{case}");
        assert_no_wd(&f);
    }
}
#[test]
fn all_batch_preflight_and_cancel_keep_source_unpublished() {
    let f = Fixture::new(true);
    let (a, sa) = f.batch("a", feature(1));
    let (b, sb) = f.batch("b", feature(2));
    let cancel = AtomicBool::new(false);
    let inputs = WorkerInputs::load(&f.library, &f.identity, &f.resources, &cancel).unwrap();
    prepare_worker_package(&f.library, &a, &sa, &inputs, &cancel).unwrap();
    let mut value: Value = serde_json::from_slice(&fs::read(&sb).unwrap()).unwrap();
    value["assets"] = json!(["outside"]);
    fs::write(&sb, serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(prepare_worker_package(&f.library, &b, &sb, &inputs, &cancel).is_err());
    assert_no_wd(&f);
    cancel.store(true, Ordering::SeqCst);
    assert!(WorkerInputs::load(&f.library, &f.identity, &f.resources, &cancel).is_err());
    assert_no_wd(&f);
}
#[test]
fn quadratic_index_checks_cancellation_before_product_without_source_lease() {
    let f = Fixture::new(true);
    let vectors = vec![feature(0); 64];
    let ids = (0..64).map(|i| format!("synthetic-{i}")).collect::<Vec<_>>();
    let count = std::cell::Cell::new(0usize);
    let result = crate::recognition_compute::anchors::anchor_index_checked(&vectors, &ids, &HashMap::new(), &|| {
        let n = count.get() + 1;
        count.set(n);
        // Reach a cross-product block (after normalizing both matrices), then cancel.
        if n == 130 {
            let c = f.source();
            c.busy_timeout(std::time::Duration::ZERO)?;
            c.execute_batch("BEGIN IMMEDIATE; ROLLBACK;")?;
            return Err(Error::Invalid("synthetic cancel".into()));
        }
        Ok(())
    });
    assert!(result.err().unwrap().to_string().contains("synthetic cancel"));
    assert_eq!(count.get(), 130);
    assert_no_wd(&f);
}
#[test]
fn external_model_root_is_authoritative_for_producer_and_importer() {
    // Dedicated subprocess owns ILLUSTASH_MODELS; never change process-global env in parallel tests.
    if std::env::var_os("ILLUSTASH_149_EXTERNAL_MODELS_TEST").is_none() {
        let parent = Fixture::new(false);
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "app_recognition::claims_tests::external_model_root_is_authoritative_for_producer_and_importer",
                "--nocapture",
            ])
            .env("ILLUSTASH_149_EXTERNAL_MODELS_TEST", "1")
            .env("ILLUSTASH_MODELS", parent.resources.join("models"))
            .status()
            .unwrap();
        assert!(status.success());
        return;
    }
    let f = Fixture::new(false);
    fs::write(f.resources.join("models/wd_swinv2_v3_emb.onnx"), b"wrong-local-WD").unwrap();
    let external = std::path::PathBuf::from(std::env::var_os("ILLUSTASH_MODELS").unwrap());
    assert_eq!(model_specs(&f.resources).unwrap(), producer::model_specs(&f.resources, &external).unwrap());
    let (path, sidecar) = f.batch("external", feature(1));
    let cancel = AtomicBool::new(false);
    let inputs = WorkerInputs::load(&f.library, &f.identity, &f.resources, &cancel).unwrap();
    let prepared = prepare_worker_package(&f.library, &path, &sidecar, &inputs, &cancel).unwrap();
    assert_eq!(
        import_worker_package(&f.library, &prepared, &inputs, &cancel, &|_, _| Ok(())).unwrap()["status"],
        "done"
    );
    let before = count(&f.source(), "SELECT count(*) FROM recognition_imports");
    fs::write(external.join("ccip/model_feat.onnx"), b"external-model-changed").unwrap();
    assert!(import_worker_package(&f.library, &prepared, &inputs, &cancel, &|_, _| Ok(())).is_err());
    assert_eq!(count(&f.source(), "SELECT count(*) FROM recognition_imports"), before);
}
#[test]
fn dense_anchor_boundary_refuses_old_scalar_claim_and_imports_empty_result() {
    let f = Fixture::new(true);
    let mut dense = vec![2.0f32.powi(-13); 768];
    dense[0] = 1.;
    fs::write(f.resources.join("review/char-anchors.npy"), producer::npy(&[dense], 768).unwrap()).unwrap();
    fs::write(f.resources.join("review/char-anchors.txt"), b"old\n").unwrap();
    f.source().execute("UPDATE characters SET anchor_p90=0.000001 WHERE char_id='old'", []).unwrap();
    let (path, sidecar) = f.batch("dense", feature(0));
    let original = fs::read(&sidecar).unwrap();
    let mut value: Value = serde_json::from_slice(&original).unwrap();
    assert_eq!(value["claims"], json!([]));
    // The actually reproduced old scalar result incorrectly had d=0 at p90=1e-6.
    value["claims"] = json!([{"asset":"a","character":"old","distance":0.0,"threshold":0.000001f32,"confidence":1.0}]);
    fs::write(&sidecar, serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(f.import(&path, &sidecar).is_err());
    assert_no_wd(&f);
    fs::write(&sidecar, original).unwrap();
    let before = human(&f.app());
    assert_eq!(f.import(&path, &sidecar).unwrap()["status"], "done");
    assert_eq!(count(&f.app(), "SELECT count(*) FROM candidates WHERE source='CCIP'"), 0);
    assert_eq!(human(&f.app()), before);
}
/// Records what an import reports (#88) and cancels when it reaches `cancel_at`, as the cancel button would.
struct Recorder {
    log: std::sync::Mutex<Vec<String>>,
    cancel_at: Option<&'static str>,
    cancel: AtomicBool,
}
impl crate::app_imports::Observer for Recorder {
    fn stage(&self, stage: &'static str) {
        self.log.lock().unwrap().push(stage.into());
        if self.cancel_at == Some(stage) {
            self.cancel.store(true, Ordering::SeqCst);
        }
    }
    fn count(&self, done: u64, total: u64) {
        self.log.lock().unwrap().push(format!("{done}/{total}"));
    }
    fn publish(&self, apply: &mut dyn FnMut() -> Result<()>) -> Result<()> {
        self.log.lock().unwrap().push("publish".into());
        if self.cancel.load(Ordering::SeqCst) {
            return Err(Error::Conflict("任务结果已过期或重复".into()));
        }
        apply()
    }
}
fn recorded(f: &Fixture, path: &Path, cancel_at: Option<&'static str>) -> (Result<Value>, Vec<String>) {
    let recorder = Recorder { log: Default::default(), cancel_at, cancel: AtomicBool::new(false) };
    let result = import_package_observed(&f.library, path, &f.resources, &recorder.cancel, &|_, _| Ok(()), &recorder);
    (result, recorder.log.into_inner().unwrap())
}
#[test]
fn an_import_reports_its_real_stages_and_a_cancel_at_any_stage_before_commit_writes_nothing() {
    let f = Fixture::new(false);
    let (path, _) = f.batch("plain", feature(1));
    let (result, log) = recorded(&f, &path, None);
    assert_eq!(result.unwrap()["status"], "done");
    assert_eq!(
        log,
        [
            "models",
            "reading",
            "0/3",
            "1/3",
            "2/3",
            "verifying",
            "writing",
            "0/1",
            "committing",
            "publish",
            "projecting"
        ]
    );
    for stage in ["models", "reading", "verifying", "writing", "committing"] {
        let f = Fixture::new(false);
        let (path, _) = f.batch("plain", feature(1));
        let app = human(&f.app());
        let (result, log) = recorded(&f, &path, Some(stage));
        assert!(result.is_err(), "{stage}");
        assert!(!log.contains(&"projecting".to_string()), "{stage}: {log:?}");
        assert_no_wd(&f);
        assert_eq!(human(&f.app()), app);
    }
}
