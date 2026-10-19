//! The core version 8 upgrade (#201) on a version 7 library from the comparison baseline: upgraded only when the
//! legacy tables and `term_*` agree and every domain was switched to the core, and then with counted history steps'
//! kind and count filled in. A refused upgrade leaves the library exactly as it was. (Every checkpoint of
//! `baseline.rs` is upgraded the same way; this file covers the refusals and what the comparison leaves out.)
use rusqlite::Connection;
use serde_json::Value;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

#[path = "common/baseline.rs"]
mod baseline;

/// A switched version 7 library with characters, links, names and history steps (the first checkpoint of the golden
/// name-writes scenario).
const CASE: &str = "tests.test_terms_golden.GoldenNameWrites.test_core_name_writes_match_the_snapshot";

struct Library {
    dir: PathBuf,
    db: PathBuf,
}

impl Drop for Library {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn library(name: &str) -> Library {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/baseline/test_terms_golden.json.gz");
    let mut text = String::new();
    flate2::read::GzDecoder::new(fs::File::open(path).unwrap()).read_to_string(&mut text).unwrap();
    let data: Value = serde_json::from_str(&text).unwrap();
    let entry = data["cases"][CASE]["calls"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|call| call["env"].as_array().cloned().unwrap_or_default())
        .find(|e| e["path"] == "@ROOT@/app.sqlite" && e.get("db").is_some())
        .expect("a checkpoint of the library");
    assert_eq!(entry["terms"], 7);
    let dir = std::env::temp_dir().join(format!("illustash-v8-upgrade-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let root = dir.to_string_lossy().replace('\\', "/");
    let part = |i: usize| data["blobs"][entry["db"][i].as_str().unwrap()].as_str().unwrap().replace("@ROOT@", &root);
    let db = dir.join("app.sqlite");
    baseline::restore(&Connection::open(&db).unwrap(), &part(0), &part(1), &part(2));
    Library { dir, db }
}

/// Runs the core's upgrade; Ok(stdout) or Err(stderr).
fn upgrade(db: &Path) -> Result<String, String> {
    let done = Command::new(env!("CARGO_BIN_EXE_illustash-terms")).args(["migrate", "--db"]).arg(db).output().unwrap();
    let (out, err) =
        (String::from_utf8_lossy(&done.stdout).into_owned(), String::from_utf8_lossy(&done.stderr).into_owned());
    if done.status.success() {
        Ok(out)
    } else {
        Err(err)
    }
}

/// Writes a guarded table as an earlier version (or a bug) might have left it.
fn write(c: &Connection, sql: &str) {
    c.execute_batch(&format!(
        "INSERT INTO meta VALUES ('terms_writer','1'); {sql}; DELETE FROM meta WHERE key='terms_writer';"
    ))
    .unwrap();
}

fn exact(db: &Path) -> std::collections::BTreeMap<String, Vec<String>> {
    baseline::exact(&Connection::open(db).unwrap(), &|t| t.to_owned())
}

#[test]
fn a_consistent_library_upgrades_and_counted_steps_get_their_kind_and_count() {
    let lib = library("consistent");
    {
        let c = Connection::open(&lib.db).unwrap();
        c.execute_batch(
            "INSERT INTO actions(label,before_json) VALUES ('已将 2 张设为 合成角色','[]'),
              ('已将 合成角色 标为已检查（3 张）','[]'),('已将 1 张从 甲 换成 乙','[]'),('已更新 合成角色 的资料','[]');",
        )
        .unwrap();
    }
    assert_eq!(upgrade(&lib.db).unwrap(), "{\"from\":7,\"to\":8}\n");
    let c = Connection::open(&lib.db).unwrap();
    let filled: Vec<(String, Option<String>, Option<String>)> = c
        .prepare("SELECT label,label_kind,label_count FROM actions ORDER BY id DESC LIMIT 4")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let kinds: Vec<(Option<&str>, Option<&str>)> =
        filled.iter().map(|(_, k, n)| (k.as_deref(), n.as_deref())).collect();
    assert_eq!(kinds, [(None, None), (Some("swap"), Some("1")), (Some("sweep"), Some("3")), (Some("set"), Some("2"))]);
    let version: String = c.query_row("SELECT value FROM meta WHERE key='terms_schema'", [], |r| r.get(0)).unwrap();
    assert_eq!(version, "8");
}

#[test]
fn legacy_tables_that_disagree_with_the_core_refuse_the_upgrade_and_change_nothing() {
    let lib = library("disagree");
    write(
        &Connection::open(&lib.db).unwrap(),
        "UPDATE links SET current=1-current WHERE rowid=(SELECT min(rowid) FROM links)",
    );
    let before = exact(&lib.db);
    let refused = upgrade(&lib.db).unwrap_err();
    assert!(refused.contains("旧兼容表与词条核心不一致"), "{refused}");
    assert_eq!(exact(&lib.db), before);
}

#[test]
fn a_library_not_switched_in_every_domain_is_refused_and_unchanged() {
    let lib = library("unswitched");
    Connection::open(&lib.db).unwrap().execute("DELETE FROM meta WHERE key='terms_names'", []).unwrap();
    let before = exact(&lib.db);
    let refused = upgrade(&lib.db).unwrap_err();
    assert!(refused.contains("pre-v8"), "{refused}");
    assert_eq!(exact(&lib.db), before);
}

#[test]
fn an_outdated_names_mirror_is_brought_up_to_date_first_as_every_core_write_does() {
    let lib = library("mirror");
    let entity: String = Connection::open(&lib.db)
        .unwrap()
        .query_row("SELECT entity_id FROM names ORDER BY rowid LIMIT 1", [], |r| r.get(0))
        .unwrap();
    write(
        &Connection::open(&lib.db).unwrap(),
        &format!("INSERT INTO names(entity_id,lang,value,source,role,preferred,position) VALUES ('{entity}','zh','待同步','user','display',0,0)"),
    );
    upgrade(&lib.db).unwrap();
    let mirrored: i64 = Connection::open(&lib.db)
        .unwrap()
        .query_row(
            "SELECT count(*) FROM term_names n JOIN terms t ON t.id=n.term_id WHERE t.key=?1 AND n.value='待同步'",
            [&entity],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(mirrored, 1);
}
