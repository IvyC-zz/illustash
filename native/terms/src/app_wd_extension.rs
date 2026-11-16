//! 显式用 #534 扩展头重扫图库已存的 WD 向量。
use crate::app_library::Library;
use crate::{Error, Result};
use illustash_core::wd_extension::{self, Hit};
use rusqlite::{Connection, OpenFlags, TransactionBehavior};
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::Path;

const PACKAGE: &str = "9da0365457e479eff189150926289c6cf8325cd9bb98c0bd2504c26d7dcae471";
const PACKAGE_BYTES: usize = 940_701;
const DIM: usize = 1024;
const HEADS: usize = 228;

#[derive(Deserialize)]
struct Manifest {
    format: String,
    wd_model_sha256: String,
    wd_tags_sha256: String,
    threshold: f64,
    labels: Vec<String>,
}

struct Package {
    manifest: Manifest,
    weights: Vec<f32>,
    bias: Vec<f32>,
}

fn float32(bytes: &[u8]) -> Vec<f32> {
    bytes.chunks_exact(4).map(|b| f32::from_le_bytes(b.try_into().expect("四字节分块"))).collect()
}

impl Package {
    fn load(path: &Path) -> Result<Self> {
        let input = crate::app_recognition::file(path, PACKAGE_BYTES as u64)?;
        if input.len() != PACKAGE_BYTES || crate::strict_json::sha256_hex(&input) != PACKAGE {
            return Err(Error::invalid("WD 扩展包与 #534 冻结版本不符"));
        }
        let end = input.iter().position(|&byte| byte == b'\n').ok_or_else(|| Error::invalid("缺少 WD 扩展包清单"))?;
        let manifest: Manifest =
            serde_json::from_slice(&input[..end]).map_err(|e| Error::invalid(format!("WD 扩展包清单无效：{e}")))?;
        let coefficients = &input[end + 1..];
        if manifest.format != "illustash-wd-extension/1"
            || manifest.labels.len() != HEADS
            || coefficients.len() != HEADS * (DIM + 1) * 4
        {
            return Err(Error::invalid("WD 扩展包格式不符"));
        }
        let (weights, bias) = coefficients.split_at(HEADS * DIM * 4);
        Ok(Self { manifest, weights: float32(weights), bias: float32(bias) })
    }

    fn compatible(&self, manifest: &str) -> Result<()> {
        let doc: Value =
            serde_json::from_str(manifest).map_err(|e| Error::invalid(format!("识别账本清单无效：{e}")))?;
        let wd = match doc["format"].as_str() {
            Some("illustash-recognition/2") => &doc["recognition"]["models"]["wd"],
            Some("illustash-recognition/1") => &doc["models"]["wd"],
            _ => &Value::Null,
        };
        if wd["version"] != "swinv2_448"
            || wd["dimension"].as_u64() != Some(DIM as u64)
            || wd["sha256"].as_str() != Some(&self.manifest.wd_model_sha256)
            || wd["tags_sha256"].as_str() != Some(&self.manifest.wd_tags_sha256)
        {
            return Err(Error::Conflict("WD 扩展包与图库向量的底座模型或标签表版本不匹配".into()));
        }
        Ok(())
    }

    fn hits(&self, uuid: &str, bytes: &[u8], out: &mut Vec<Hit>) -> Result<()> {
        if bytes.len() != DIM * 4 {
            return Err(Error::invalid("WD 特征向量维数不正确"));
        }
        let vector = float32(bytes);
        if vector.iter().any(|v| !v.is_finite()) {
            return Err(Error::invalid("WD 特征向量包含非有限值"));
        }
        for ((tag, weights), bias) in self.manifest.labels.iter().zip(self.weights.chunks_exact(DIM)).zip(&self.bias) {
            // 累加与 sigmoid 用 f64，阈值与证据分数沿用冻结实验的 f32 概率精度。
            let sum: f64 = vector.iter().zip(weights).map(|(&x, &w)| f64::from(x) * f64::from(w)).sum();
            let probability = (1.0 / (1.0 + (-(sum + f64::from(*bias))).exp())) as f32;
            if f64::from(probability) >= self.manifest.threshold {
                out.push(Hit { uuid: uuid.to_owned(), tag: tag.clone(), probability: f64::from(probability) });
            }
        }
        Ok(())
    }
}

fn writer(path: &Path) -> Result<Connection> {
    let conn = crate::sqlite::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    crate::configure(&conn)?;
    conn.busy_timeout(crate::app_library::TASK_WRITE_WAIT)?;
    Ok(conn)
}

fn scan(src: &Connection, package: &Package) -> Result<(usize, Vec<Hit>)> {
    let tables = src.prepare("SELECT name FROM sqlite_schema WHERE type='table' AND name IN ('embeddings','recognition_assets','recognition_imports')")?
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<std::collections::HashSet<_>>>()?;
    if !tables.contains("embeddings") {
        return Err(Error::Conflict("这个图库没有已保存的 WD 向量".into()));
    }
    let has_ledger = tables.contains("recognition_assets");
    if has_ledger != tables.contains("recognition_imports") {
        return Err(Error::Schema("WD 识别账本不完整".into()));
    }
    const LIVE: &str = "WHERE v.model='wd' AND a.in_scope=1 AND coalesce(a.trashed,0)=0 ORDER BY v.uuid";
    let sql = if has_ledger {
        format!("SELECT v.uuid,v.vec,r.package,i.manifest,i.status FROM embeddings v JOIN assets a ON a.uuid=v.uuid
                 LEFT JOIN recognition_assets r ON r.asset=a.uuid LEFT JOIN recognition_imports i ON i.digest=r.package {LIVE}")
    } else {
        format!("SELECT v.uuid,v.vec,NULL,NULL,NULL FROM embeddings v JOIN assets a ON a.uuid=v.uuid {LIVE}")
    };
    let mut seen = std::collections::HashSet::new();
    let mut hits = Vec::new();
    let mut checked = 0;
    let mut stmt = src.prepare(&sql)?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        if let Some(delivery) = row.get::<_, Option<String>>(2)? {
            if row.get::<_, Option<String>>(4)?.as_deref() != Some("done") {
                return Err(Error::Conflict("WD 识别交付尚未完成，请先恢复这次交付".into()));
            }
            if seen.insert(delivery) {
                package.compatible(&row.get::<_, String>(3)?)?;
            }
        }
        // 旧图库无逐图交付账本时，使用已经存储的 WD 向量。
        package.hits(&row.get::<_, String>(0)?, &row.get::<_, Vec<u8>>(1)?, &mut hits)?;
        checked += 1;
    }
    Ok((checked, hits))
}

pub fn apply(library: &Library, path: &Path) -> Result<Value> {
    ensure_writable(library)?;
    apply_loaded(library, &Package::load(path)?)
}

fn apply_loaded(library: &Library, package: &Package) -> Result<Value> {
    let mut source = writer(&library.source())?;
    // 识别和来源扫描在这个快照完成应用提交之前不能改写 WD 向量。锁顺序与识别交付一致。
    let source_snapshot = source.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let (checked, hits) = scan(&source_snapshot, package)?;
    let mut app = writer(&library.db())?;
    crate::schema::require_current(&app)?;
    let tx = app.transaction_with_behavior(TransactionBehavior::Immediate)?;
    crate::schema::begin_writing(&tx)?;
    let changed = wd_extension::apply(&tx, PACKAGE, &hits)?;
    crate::schema::commit_writing(tx)?;
    Ok(json!({"package_sha256":PACKAGE,"scored":checked,"hits":hits.len(),"changed":changed}))
}

pub fn withdraw(library: &Library) -> Result<Value> {
    ensure_writable(library)?;
    let mut app = writer(&library.db())?;
    crate::schema::require_current(&app)?;
    let tx = app.transaction_with_behavior(TransactionBehavior::Immediate)?;
    crate::schema::begin_writing(&tx)?;
    let changed = wd_extension::withdraw(&tx)?;
    crate::schema::commit_writing(tx)?;
    Ok(json!({"changed":changed}))
}

fn ensure_writable(library: &Library) -> Result<()> {
    if library.frozen() {
        return Err(Error::Conflict("图库只读".into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };

    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct Fixture {
        root: PathBuf,
        library: Library,
    }

    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "illustash-wd-extension-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let images = root.join("images");
            fs::create_dir_all(&images).unwrap();
            let dest = root.join("test.pilib");
            crate::app_create::create_folder(&dest, &images, None, None, None).unwrap();
            let library = Library::open(&dest).unwrap();
            Self { root, library }
        }

        fn write(&self, f: impl FnOnce(&Connection) -> Result<()>) -> Result<()> {
            let mut app = writer(&self.library.db())?;
            let tx = app.transaction_with_behavior(TransactionBehavior::Immediate)?;
            crate::schema::begin_writing(&tx)?;
            f(&tx)?;
            crate::schema::commit_writing(tx)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.root).unwrap();
        }
    }

    fn snapshot(c: &Connection, sql: &str) -> Vec<Value> {
        crate::app_migrate_data::rows(c, sql).unwrap()
    }

    #[test]
    fn evidence_replaces_and_restores_without_rewriting_old_sources() {
        round_trip(false);
        round_trip(true);
    }

    fn round_trip(hybrid: bool) {
        let f = Fixture::new();
        let assets = ["existing", "weak", "empty", "human", "rejected"];
        let app_rows = assets
            .iter()
            .map(|uuid| json!([uuid, "synthetic.png", "synthetic.png", 8, 8, "2026-11-16", "folder"]))
            .collect::<Vec<_>>();
        let mut weights = vec![0.; DIM];
        weights[0] = 2.;
        let package = Package {
            manifest: Manifest {
                format: "illustash-wd-extension/1".into(),
                wd_model_sha256: "model".into(),
                wd_tags_sha256: "tags".into(),
                threshold: 0.8008725047111511,
                labels: vec!["from_b".into()],
            },
            weights,
            bias: vec![0.],
        };
        let source = writer(&f.library.source()).unwrap();
        source
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS embeddings(uuid TEXT,model TEXT,vec BLOB,PRIMARY KEY(uuid,model))",
            )
            .unwrap();
        let mut wd = vec![0f32; DIM];
        wd[0] = 1.;
        let vector = wd.into_iter().flat_map(f32::to_le_bytes).collect::<Vec<_>>();
        for uuid in assets {
            source
                .execute(
                    "INSERT INTO assets(uuid,filename,src_path,width,height,added_at,src_kind,in_scope,trashed)
                 VALUES (?1,'synthetic.png','synthetic.png',8,8,'2026-11-16','folder',1,0)",
                    [uuid],
                )
                .unwrap();
            source.execute("INSERT INTO embeddings VALUES (?1,'wd',?2)", rusqlite::params![uuid, vector]).unwrap();
        }
        f.write(|tx| {
            if hybrid {
                tx.execute("INSERT OR REPLACE INTO meta VALUES ('identity_policy','hybrid')", [])?;
                tx.execute_batch(
                    &include_str!("app_migration_sql/hybrid.sql")
                        .replace("entities", illustash_core::character_entities::ENTITIES),
                )?;
            }
            crate::character_commands::command(
                tx,
                &json!({
                    "op":"refresh","assets":app_rows,"indexed":"2026-11-16","index":{},
                    "labels":[["existing","base",0.89,"character"],["weak","near",0.90,"char_ext"],
                              ["human","manually_chosen",0.40,"character"],["rejected","from_b",0.50,"character"]],
                    "decisions":[],"duplicates":[]
                }),
            )?;
            for (uuid, action) in [("human", "confirmed"), ("rejected", "rejected")] {
                let entity: String =
                    tx.query_row("SELECT entity FROM candidates WHERE uuid=?1", [uuid], |r| r.get(0))?;
                if uuid == "rejected" {
                    // 同一个公开键即使此前由人工操作或原来源创建，也不能绕过否定。
                    let term: i64 = tx.query_row("SELECT id FROM terms WHERE key=?1", [&entity], |r| r.get(0))?;
                    tx.execute(
                        "INSERT INTO term_links(system,key,term_id,source) VALUES ('danbooru','from_b',?1,'import')",
                        [term],
                    )?;
                }
                crate::character_commands::command(
                    tx,
                    &json!({"op":"change","asked":[uuid],"entity":entity,"action":action}),
                )?;
            }
            Ok(())
        })
        .unwrap();
        let app = writer(&f.library.db()).unwrap();
        let candidates = "SELECT * FROM candidates ORDER BY uuid,source,entity";
        let members = "SELECT * FROM memberships ORDER BY uuid,entity";
        let old_candidates = snapshot(&app, candidates);
        let old_members = snapshot(&app, members);
        let active_rows = "SELECT id,asset_id,term_id,version,score FROM term_current_evidence ORDER BY id";
        let old_evidence = snapshot(&app, active_rows);
        let verdicts = "SELECT * FROM term_verdicts ORDER BY id";
        let old_verdicts = snapshot(&app, verdicts);
        let projection = "SELECT * FROM term_projection ORDER BY asset_id,term_id";
        let old_projection = snapshot(&app, projection);
        assert_eq!(old_members.len(), 2);
        assert_eq!(withdraw(&f.library).unwrap()["changed"], 0);
        let applied = apply_loaded(&f.library, &package).unwrap();
        assert_eq!(applied["changed"], 5);
        assert_eq!(applied["hits"], 5);
        assert_eq!(
            snapshot(&app, "SELECT uuid FROM memberships WHERE sources='WD_EXT' ORDER BY uuid"),
            vec![json!(["empty"]), json!(["weak"])]
        );
        assert_eq!(
            snapshot(&app, "SELECT * FROM candidates WHERE source<>'WD_EXT' ORDER BY uuid,source,entity"),
            old_candidates
        );
        let evidence =
            snapshot(&app, "SELECT id,asset_id,term_id,version,score FROM term_current_evidence ORDER BY id");
        let sets = snapshot(&app, "SELECT * FROM term_evidence_sets ORDER BY id");
        assert_eq!(apply_loaded(&f.library, &package).unwrap()["changed"], 0);
        assert_eq!(snapshot(&app, "SELECT * FROM term_evidence_sets ORDER BY id"), sets);

        // 模拟批次生效途中数据库写入失败。上一个有效集和投影不动，重试仍能完成。
        app.execute_batch(
            "CREATE TRIGGER stop_extension BEFORE INSERT ON term_evidence_sets
             WHEN new.source='wd_extension' BEGIN SELECT RAISE(ABORT,'simulated interruption'); END;",
        )
        .unwrap();
        assert!(withdraw(&f.library).is_err());
        assert_eq!(
            snapshot(&app, "SELECT id,asset_id,term_id,version,score FROM term_current_evidence ORDER BY id"),
            evidence
        );
        app.execute_batch("DROP TRIGGER stop_extension").unwrap();
        assert_eq!(withdraw(&f.library).unwrap()["changed"], 5);
        assert_eq!(withdraw(&f.library).unwrap()["changed"], 0);
        assert_eq!(snapshot(&app, candidates), old_candidates);
        assert_eq!(snapshot(&app, members), old_members);
        assert_eq!(snapshot(&app, active_rows), old_evidence);
        assert_eq!(snapshot(&app, verdicts), old_verdicts);
        assert_eq!(snapshot(&app, projection), old_projection);
        assert!(snapshot(&app, "SELECT id FROM term_current_evidence WHERE source='wd_extension'").is_empty());
        assert_eq!(snapshot(&app, "SELECT id FROM term_evidence WHERE source='wd_extension'").len(), 5);
        let reopened = Library::open(&f.root.join("test.pilib")).unwrap();
        assert_eq!(snapshot(&writer(&reopened.db()).unwrap(), members), old_members);
        assert_eq!(apply_loaded(&reopened, &package).unwrap()["changed"], 5);
        assert_eq!(
            snapshot(&app, "SELECT id,asset_id,term_id,version,score FROM term_current_evidence ORDER BY id"),
            evidence
        );

        // 装错底座和向量损坏时，已有的有效集不被空结果覆盖。
        source.execute("UPDATE embeddings SET vec=X'00' WHERE uuid='weak'", []).unwrap();
        assert!(apply_loaded(&reopened, &package).is_err());
        assert_eq!(
            snapshot(&app, "SELECT id,asset_id,term_id,version,score FROM term_current_evidence ORDER BY id"),
            evidence
        );
        assert!(package
            .compatible(
                &json!({"format":"illustash-recognition/2","recognition":{"models":{"wd":{
                    "version":"swinv2_448","dimension":1024,"sha256":"unrelated","tags_sha256":"tags"
                }}}})
                .to_string()
            )
            .is_err());

        f.write(|tx| wd_extension::invalidate(tx, &["weak".into()])).unwrap();
        assert_eq!(
            snapshot(&app, "SELECT uuid FROM memberships WHERE sources='WD_EXT' ORDER BY uuid"),
            vec![json!(["empty"])]
        );
    }
}
