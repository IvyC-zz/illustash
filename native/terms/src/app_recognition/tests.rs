//! 正式导入的合成回归测试；不用模型、私有锚点或外部图库。
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
        let input = images.join("synthetic.png");
        fs::write(&input, b"synthetic input bytes A").unwrap();
        src.execute("UPDATE assets SET src_path=? WHERE uuid='a'", [input.to_str().unwrap()]).unwrap();
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
    fn input(&self) -> PathBuf {
        self.root.join("images/synthetic.png")
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
            &[("a".into(), producer::input_hash(&self.input(), &|| Ok(())).unwrap())],
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
        import_worker_package(&self.library, &prepared, &inputs, &cancel, &|_, _| Ok(()), None).map(Recovery::response)
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
        let result = import_worker_package(
            &f.library,
            &prepared,
            &inputs,
            &cancel,
            &|_, _| Err(Error::Invalid("synthetic dispatch crash".into())),
            None,
        )
        .unwrap();
        assert_eq!(result.response()["status"], "pending");
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
        assert!(
            import_worker_package(&f.library, &prepared, &inputs, &cancel, &|_, _| Ok(()), None).is_err(),
            "{case}"
        );
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
        // 进入一个交叉乘积分块（两个矩阵都已归一化之后），然后取消。
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
    // 由专门的子进程设置 ILLUSTASH_MODELS；并行测试里绝不改动进程全局的环境变量。
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
        import_worker_package(&f.library, &prepared, &inputs, &cancel, &|_, _| Ok(()), None).unwrap().response()
            ["status"],
        "done"
    );
    let before = count(&f.source(), "SELECT count(*) FROM recognition_imports");
    fs::write(external.join("ccip/model_feat.onnx"), b"external-model-changed").unwrap();
    assert!(import_worker_package(&f.library, &prepared, &inputs, &cancel, &|_, _| Ok(()), None).is_err());
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
    // 实际复现出的旧标量结果在 p90=1e-6 时错误地给出 d=0。
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
/// 记录导入报告的内容（#88），到达 `cancel_at` 时像取消按钮那样取消。
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

/// 新图库里 WD 认出的新角色按 tag 后缀归到作品（#296）：同一个后缀只建一个待核对的作品；不是作品的后缀不建，
/// 角色未归类。数据全部合成。
#[test]
fn wd_characters_in_a_new_library_get_the_work_of_their_tag_suffix() {
    let f = Fixture::new(false);
    let src = f.source();
    src.execute_batch(
        "INSERT INTO assets(uuid,filename,src_path,width,height,added_at,src_kind,in_scope,trashed) VALUES
           ('b','b.png','b.png',16,16,'2026-10-03','folder',1,0);
         INSERT INTO labels(uuid,dim,value,conf,source) VALUES ('b','character','first_girl_(synthetic_work)',0.9,'wd_v3'),
           ('b','character','second_girl_(synthetic_work)',0.8,'wd_v3'),('b','character','third_girl_(vtuber)',0.7,'wd_v3'),
           ('b','character','fourth_girl',0.6,'wd_v3');",
    )
    .unwrap();
    let mut app = f.app();
    let tx = app.transaction_with_behavior(TransactionBehavior::Immediate).unwrap();
    crate::schema::begin_writing(&tx).unwrap();
    import_changes(&tx, &src, &f.library, &["b".into()]).unwrap();
    crate::schema::commit_writing(tx).unwrap();
    let app = f.app();
    let works: Vec<String> = app
        .prepare(
            "SELECT l.key||'→'||coalesce(q.key,'-')||'→'||r.work_text FROM term_links l JOIN term_tree r ON r.term_id=l.term_id
             LEFT JOIN terms w ON w.key=r.work_id LEFT JOIN term_links q ON q.term_id=w.id AND q.system='qualifier'
             WHERE l.system='wd_character' ORDER BY l.key",
        )
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(
        works,
        [
            "first_girl_(synthetic_work)→synthetic_work→synthetic_work",
            "fourth_girl→-→",
            "second_girl_(synthetic_work)→synthetic_work→synthetic_work",
            "third_girl_(vtuber)→-→vtuber",
        ]
    );
    assert_eq!(
        count(
            &app,
            "SELECT count(*) FROM terms t JOIN term_tree r ON r.term_id=t.id WHERE t.kind='work' AND r.review_status='pending'"
        ),
        1
    );
}

#[test]
fn redelivery_after_invalidation_uses_new_projection_with_or_without_anchors() {
    for anchored in [false, true] {
        let f = Fixture::new(anchored);
        let before = human(&f.app());
        let (path, sidecar) = f.batch("original", feature(1));
        let first = f.import(&path, &sidecar).unwrap();
        assert_eq!(first["duplicate"], false);
        f.source().execute_batch("DELETE FROM labels WHERE uuid='a'; DELETE FROM embeddings WHERE uuid='a'; DELETE FROM recognition_assets WHERE asset='a';").unwrap();
        // 应用已经有旧投影标记：修复前会直接报 duplicate，连新投影都不尝试。
        f.app().execute_batch("CREATE TRIGGER fail BEFORE INSERT ON assets BEGIN SELECT RAISE(ABORT,'synthetic redelivery failure'); END;").unwrap();
        let retried = f.import(&path, &sidecar).unwrap();
        assert_eq!(retried["digest"], first["digest"]);
        assert_eq!(retried["duplicate"], false);
        assert_eq!(retried["status"], "pending");
        assert_eq!(count(&f.source(), "SELECT count(*) FROM recognition_assets"), 1);
        assert_eq!(count(&f.source(), "SELECT count(*) FROM recognition_imports WHERE status='pending'"), 1);
        f.app().execute_batch("DROP TRIGGER fail").unwrap();
        let reopened = Library::open(f.library.instance_path()).unwrap();
        assert_eq!(resume(&reopened, &AtomicBool::new(false), &|_, _| Ok(())).unwrap()["status"], "done");
        assert_eq!(count(&f.app(), "SELECT count(*) FROM meta WHERE key LIKE 'recognition_projection:%'"), 2);
        let events = count(&f.app(), "SELECT count(*) FROM backend_effects");
        assert_eq!(f.import(&path, &sidecar).unwrap()["duplicate"], true);
        assert_eq!(count(&f.app(), "SELECT count(*) FROM backend_effects"), events);
        assert_eq!(human(&f.app()), before);
        // 历史抢救路径会删除源包账本，却保留应用标记；新交付也必须绕开它。
        f.source().execute_batch("DELETE FROM labels; DELETE FROM embeddings; DELETE FROM recognition_assets; DELETE FROM recognition_imports;").unwrap();
        assert_eq!(f.import(&path, &sidecar).unwrap()["duplicate"], false);
        assert_eq!(count(&f.app(), "SELECT count(*) FROM meta WHERE key LIKE 'recognition_projection:%'"), 3);
        assert_eq!(f.import(&path, &sidecar).unwrap()["duplicate"], true);
        assert_eq!(human(&f.app()), before);
    }
}

#[test]
fn old_input_package_is_rejected_even_without_a_scan_and_can_retry_after_bytes_are_restored() {
    let f = Fixture::new(true);
    let before = human(&f.app());
    let original = fs::read(f.input()).unwrap();
    let (path, sidecar) = f.batch("old", feature(1));
    fs::write(f.input(), b"new bytes with unchanged asset id").unwrap();
    assert!(f.import(&path, &sidecar).unwrap_err().to_string().contains("已过期"));
    assert_no_wd(&f);
    assert_eq!(human(&f.app()), before);
    fs::write(f.input(), original).unwrap();
    assert_eq!(f.import(&path, &sidecar).unwrap()["status"], "done");
    assert_eq!(f.import(&path, &sidecar).unwrap()["duplicate"], true);
    assert_eq!(human(&f.app()), before);
}
#[test]
fn input_changes_at_publication_roll_back_all_machine_writes() {
    struct Replace(PathBuf);
    impl Observer for Replace {
        fn publish(&self, apply: &mut dyn FnMut() -> Result<()>) -> Result<()> {
            fs::write(&self.0, b"changed after validation")?;
            apply()
        }
    }
    let f = Fixture::new(false);
    let before = human(&f.app());
    let (path, _) = f.batch("result", feature(1));
    let result = import_package_observed(
        &f.library,
        &path,
        &f.resources,
        &AtomicBool::new(false),
        &|_, _| Ok(()),
        &Replace(f.input()),
    );
    assert!(result.unwrap_err().to_string().contains("已过期"));
    assert_no_wd(&f);
    assert_eq!(human(&f.app()), before);
}
#[test]
fn publication_checks_bytes_even_when_input_size_and_mtime_are_unchanged() {
    struct Replace(PathBuf);
    impl Observer for Replace {
        fn publish(&self, apply: &mut dyn FnMut() -> Result<()>) -> Result<()> {
            let metadata = fs::metadata(&self.0)?;
            let mut bytes = fs::read(&self.0)?;
            bytes[0] ^= 1;
            fs::write(&self.0, bytes)?;
            fs::OpenOptions::new().write(true).open(&self.0)?.set_modified(metadata.modified()?)?;
            assert_eq!(fs::metadata(&self.0)?.len(), metadata.len());
            assert_eq!(fs::metadata(&self.0)?.modified()?, metadata.modified()?);
            apply()
        }
    }
    for duplicate in [false, true] {
        let f = Fixture::new(false);
        let (path, _) = f.batch("result", feature(1));
        let import = |observer: &dyn Observer| {
            import_package_observed(&f.library, &path, &f.resources, &AtomicBool::new(false), &|_, _| Ok(()), observer)
        };
        if duplicate {
            assert_eq!(import(&Silent).unwrap()["status"], "done");
        }
        let before = human(&f.app());
        assert!(import(&Replace(f.input())).unwrap_err().to_string().contains("已过期"));
        if duplicate {
            // 重复包也须复核内容；拒绝它不撤销已经发布的历史交付。
            assert_eq!(count(&f.source(), "SELECT count(*) FROM recognition_imports"), 1);
            assert_eq!(count(&f.source(), "SELECT count(*) FROM recognition_assets"), 1);
        } else {
            assert_no_wd(&f);
        }
        assert_eq!(human(&f.app()), before);
    }
}
#[test]
fn v1_and_malformed_v2_inputs_are_rejected_before_writes() {
    let f = Fixture::new(false);
    let (path, _) = f.batch("result", feature(0));
    let manifest = path.join("manifest.json");
    let original: Value = serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
    for kind in ["v1", "missing", "extra", "invalid", "preprocessing"] {
        let mut value = original.clone();
        match kind {
            "v1" => value["format"] = json!("illustash-recognition/1"),
            "missing" => {
                value.as_object_mut().unwrap().remove("inputs");
            }
            "extra" => value["inputs"]["other"] = json!("a".repeat(64)),
            "invalid" => value["inputs"]["a"] = json!("A".repeat(64)),
            _ => value["preprocessing"] = json!("unknown"),
        }
        fs::write(&manifest, value.to_string()).unwrap();
        let err = import_package(&f.library, &path, &f.resources, &AtomicBool::new(false), &|_, _| Ok(())).unwrap_err();
        if kind == "v1" {
            assert!(err.to_string().contains("重新生成 v2"));
        }
        assert_no_wd(&f);
    }
}
#[test]
fn mixed_versions_reject_the_whole_batch_and_current_version_gets_a_distinct_fingerprint() {
    let f = Fixture::new(false);
    let second = f.root.join("images/b.png");
    fs::write(&second, b"input B").unwrap();
    f.source()
        .execute(
            "INSERT INTO assets(uuid,filename,src_path,in_scope,trashed) VALUES ('b','b.png',?,1,0)",
            [second.to_str().unwrap()],
        )
        .unwrap();
    let inputs = vec![
        ("a".into(), producer::input_hash(&f.input(), &|| Ok(())).unwrap()),
        ("b".into(), producer::input_hash(&second, &|| Ok(())).unwrap()),
    ];
    let path = f.root.join("mixed");
    producer::write_batch(
        &path,
        &json!(f.identity),
        &model_specs(&f.resources).unwrap(),
        &inputs,
        &inputs
            .iter()
            .map(|(id, _)| producer::Label {
                asset: id.clone(),
                dim: "rating".into(),
                value: "general".into(),
                confidence: 0.9,
            })
            .collect::<Vec<_>>(),
        &[vec![1.; 1024], vec![1.; 1024]],
        &[feature(0), feature(0)],
    )
    .unwrap();
    fs::write(&second, b"changed B").unwrap();
    assert!(import_package(&f.library, &path, &f.resources, &AtomicBool::new(false), &|_, _| Ok(())).is_err());
    assert_no_wd(&f);
    let (old, _) = f.batch("old-a", feature(0));
    let first = import_package(&f.library, &old, &f.resources, &AtomicBool::new(false), &|_, _| Ok(())).unwrap();
    let old_fingerprint: String =
        f.source().query_row("SELECT digest FROM recognition_assets WHERE asset='a'", [], |r| r.get(0)).unwrap();
    fs::write(f.input(), b"replacement A").unwrap();
    // 模拟扫描已使旧资产结果失效；旧包收据仍然保留。
    f.source().execute("DELETE FROM recognition_assets WHERE asset='a'", []).unwrap();
    let (new, _) = f.batch("new-a", feature(0));
    let second = import_package(&f.library, &new, &f.resources, &AtomicBool::new(false), &|_, _| Ok(())).unwrap();
    let new_fingerprint: String =
        f.source().query_row("SELECT digest FROM recognition_assets WHERE asset='a'", [], |r| r.get(0)).unwrap();
    assert_ne!(first["digest"], second["digest"]);
    assert_ne!(old_fingerprint, new_fingerprint);
}

#[test]
fn legacy_unpublished_receipts_cannot_silently_gain_input_version_protection() {
    let f = Fixture::new(false);
    let before = human(&f.app());
    let (path, _) = f.batch("result", feature(0));
    let mut manifest: Value = serde_json::from_slice(&fs::read(path.join("manifest.json")).unwrap()).unwrap();
    manifest["format"] = json!("illustash-recognition/1");
    manifest.as_object_mut().unwrap().remove("inputs");
    for legacy in [manifest.clone(), json!({"recognition":manifest,"character_claims":{}})] {
        assert!(verify_current_inputs(&f.source(), &legacy, &["a".into()], &AtomicBool::new(false))
            .unwrap_err()
            .to_string()
            .contains("不能发布"));
    }
    assert_no_wd(&f);
    assert_eq!(human(&f.app()), before);
}

#[test]
fn public_metadata_cannot_shadow_the_actual_manifest_input_binding() {
    let f = Fixture::new(false);
    let original = fs::read(f.input()).unwrap();
    let (path, _) = f.batch("result", feature(0));
    let file = path.join("manifest.json");
    let mut manifest: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
    let mut nested = manifest.clone();
    fs::write(f.input(), b"replacement input").unwrap();
    nested["inputs"]["a"] = json!(producer::input_hash(&f.input(), &|| Ok(())).unwrap());
    manifest["recognition"] = nested;
    fs::write(&file, manifest.to_string()).unwrap();
    assert!(import_package(&f.library, &path, &f.resources, &AtomicBool::new(false), &|_, _| Ok(()))
        .unwrap_err()
        .to_string()
        .contains("已过期"));
    assert_no_wd(&f);
    // 仍支持公开包原有的附加元数据，只有外层字段决定输入版本。
    fs::write(f.input(), original).unwrap();
    assert_eq!(
        import_package(&f.library, &path, &f.resources, &AtomicBool::new(false), &|_, _| Ok(())).unwrap()["status"],
        "done"
    );
}

#[test]
fn unprojected_legacy_is_refused_and_sync_retirement_is_restart_safe() {
    let f = Fixture::new(false);
    let before = human(&f.app());
    let old_source =
        crate::app_migrate_data::rows(&f.source(), "SELECT * FROM labels WHERE source='char_index' AND dim='char_ext'")
            .unwrap();
    let old_candidates =
        crate::app_migrate_data::rows(&f.app(), "SELECT * FROM candidates WHERE source='CCIP'").unwrap();
    let (path, sidecar) = f.batch("legacy", feature(1));
    f.app()
        .execute_batch(
            "CREATE TRIGGER fail BEFORE INSERT ON assets BEGIN SELECT RAISE(ABORT,'synthetic app failure'); END;",
        )
        .unwrap();
    assert_eq!(f.import(&path, &sidecar).unwrap()["status"], "pending");
    f.app().execute_batch("DROP TRIGGER fail").unwrap();
    let mut manifest: Value = serde_json::from_slice(&fs::read(path.join("manifest.json")).unwrap()).unwrap();
    manifest["format"] = json!("illustash-recognition/1");
    manifest.as_object_mut().unwrap().remove("inputs");
    f.source().execute("UPDATE recognition_imports SET manifest=?", [manifest.to_string()]).unwrap();
    let cancel = AtomicBool::new(false);
    assert_eq!(resume(&f.library, &cancel, &|_, _| Ok(())).unwrap()["status"], "pending");
    assert_eq!(count(&f.app(), "SELECT count(*) FROM meta WHERE key LIKE 'recognition_projection:%'"), 0);
    // 模拟应用撤回已提交、源确认失败。重试沿持久化撤回标记完成，不重复正式导入。
    f.source().execute_batch("CREATE TRIGGER fail BEFORE UPDATE ON recognition_imports BEGIN SELECT RAISE(ABORT,'synthetic source confirmation failure'); END;").unwrap();
    assert!(retire_unverifiable(&f.library, &cancel).is_err());
    assert_eq!(count(&f.app(), "SELECT count(*) FROM meta WHERE key LIKE 'recognition_invalidated:%'"), 1);
    assert_eq!(count(&f.app(), "SELECT count(*) FROM asset_rating"), 0);
    assert_eq!(count(&f.source(), "SELECT count(*) FROM recognition_assets"), 1);
    let effects = count(&f.app(), "SELECT count(*) FROM backend_effects");
    f.source().execute_batch("DROP TRIGGER fail").unwrap();
    let reopened = Library::open(f.library.instance_path()).unwrap();
    retire_unverifiable(&reopened, &cancel).unwrap();
    assert_eq!(count(&f.source(), "SELECT count(*) FROM recognition_assets"), 0);
    assert_eq!(count(&f.source(), "SELECT count(*) FROM recognition_imports WHERE status='superseded'"), 1);
    assert_eq!(count(&f.app(), "SELECT count(*) FROM backend_effects"), effects);
    assert_eq!(resume(&reopened, &cancel, &|_, _| Ok(())).unwrap()["status"], "done");
    // 无锚点交付没有接管旧 CCIP 候选，撤回它也不能移除别的回执或历史来源的候选。
    assert_eq!(
        crate::app_migrate_data::rows(&f.source(), "SELECT * FROM labels WHERE source='char_index' AND dim='char_ext'")
            .unwrap(),
        old_source
    );
    assert_eq!(
        crate::app_migrate_data::rows(&f.app(), "SELECT * FROM candidates WHERE source='CCIP'").unwrap(),
        old_candidates
    );
    // 抢救源库后即使两张账本都已丢失，应用内孤立的撤回标记仍占用旧回执键。
    f.source().execute_batch("DELETE FROM recognition_assets; DELETE FROM recognition_imports;").unwrap();
    assert_eq!(count(&f.app(), "SELECT count(*) FROM meta WHERE key LIKE 'recognition_projection:%'"), 0);
    let restored = f.import(&path, &sidecar).unwrap();
    assert_eq!(restored["status"], "done");
    let receipt: String = f.source().query_row("SELECT digest FROM recognition_imports", [], |r| r.get(0)).unwrap();
    assert!(receipt.starts_with(&format!("{}:redelivery:", restored["digest"].as_str().unwrap())));
    assert_eq!(human(&f.app()), before);
    assert_eq!(
        crate::app_migrate_data::rows(&f.source(), "SELECT * FROM labels WHERE source='char_index' AND dim='char_ext'")
            .unwrap(),
        old_source
    );
    assert_eq!(
        crate::app_migrate_data::rows(&f.app(), "SELECT * FROM candidates WHERE source='CCIP'").unwrap(),
        old_candidates
    );
}
