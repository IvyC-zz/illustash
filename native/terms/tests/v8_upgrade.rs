//! The core version 8 upgrade (#201) on a version 7 library from the comparison baseline: upgraded only when the
//! legacy tables and `term_*` agree and every domain was switched to the core, and then with the aliases and display
//! names moved to the core and counted history steps' kind and count filled in. A refused upgrade leaves the library
//! exactly as it was. (Every checkpoint of `baseline.rs` is upgraded the same way; this file covers the refusals and
//! what the comparison leaves out.)
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

/// Every row of a query, as text (enough to compare two tables' contents).
fn rows(c: &Connection, sql: &str) -> Vec<String> {
    let mut st = c.prepare(sql).unwrap();
    let n = st.column_count();
    st.query_map([], |r| Ok(format!("{:?}", (0..n).map(|i| r.get::<_, rusqlite::types::Value>(i)).collect::<Vec<_>>())))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn exists(c: &Connection, name: &str) -> bool {
    c.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name = ?1)", [name], |r| r.get(0)).unwrap()
}

fn names_version(c: &Connection) -> i64 {
    c.query_row("SELECT CAST(value AS INTEGER) FROM meta WHERE key = 'names_version'", [], |r| r.get(0)).unwrap()
}

#[test]
fn a_consistent_library_upgrades_retires_the_key_tables_and_fills_in_counted_steps() {
    let lib = library("consistent");
    {
        let c = Connection::open(&lib.db).unwrap();
        c.execute_batch(
            "INSERT INTO actions(label,before_json) VALUES ('已将 2 张设为 合成角色','[]'),
              ('已将 合成角色 标为已检查（3 张）','[]'),('已将 1 张从 甲 换成 乙','[]'),('已更新 合成角色 的资料','[]');",
        )
        .unwrap();
    }
    // Rows the core's copies do not have yet: the legacy tables are the authority up to version 7.
    write(
        &Connection::open(&lib.db).unwrap(),
        "INSERT INTO name_aliases VALUES ('work','synthetic_work','合成别名',0);
         INSERT INTO display_names VALUES ('work','synthetic_work','zh','user','合成作品名')",
    );
    let aliases = "SELECT kind, key, value, position FROM {} ORDER BY kind, key, value";
    let display = "SELECT kind, key, lang, source, value FROM {} ORDER BY kind, key, lang, source, value";
    let before = Connection::open(&lib.db).unwrap();
    let old_aliases = rows(&before, &aliases.replace("{}", "name_aliases"));
    let old_display = rows(&before, &display.replace("{}", "display_names"));
    let old_names =
        rows(&before, "SELECT entity_id, lang, value, source, role, preferred, position FROM names ORDER BY rowid");
    assert!(old_names.len() > 1);
    let old_links =
        rows(&before, "SELECT system, key, entity_id, current, source, added_at FROM links ORDER BY system, key");
    assert!(old_links.iter().any(|l| l.contains("Text(\"wd\")")), "{old_links:?}");
    assert!(old_aliases.iter().any(|a| a.contains("合成别名")) && old_display.iter().any(|a| a.contains("合成作品名")));
    drop(before);
    assert_eq!(upgrade(&lib.db).unwrap(), "{\"from\":7,\"to\":8}\n");
    let c = Connection::open(&lib.db).unwrap();
    assert_eq!(rows(&c, &aliases.replace("{}", "term_key_aliases")), old_aliases, "aliases move to the core unchanged");
    assert_eq!(rows(&c, &display.replace("{}", "term_key_names")), old_display, "display names move unchanged");
    assert!(!exists(&c, "name_aliases") && !exists(&c, "display_names"), "the legacy tables are dropped");
    // The names keep their order: `seq` takes the former rowids.
    let names = "SELECT t.key, n.lang, n.value, n.source, n.role, n.preferred, n.position
                 FROM term_names n JOIN terms t ON t.id = n.term_id JOIN term_tree r ON r.term_id = n.term_id ORDER BY n.seq";
    assert_eq!(rows(&c, names), old_names, "names move to the core in their order");
    assert!(!exists(&c, "names"));
    // The links too, WD written `wd_character` in the core.
    let links =
        "SELECT CASE x.system WHEN 'wd_character' THEN 'wd' ELSE x.system END, x.key, t.key, x.current, x.source,
                   x.added_at
                 FROM term_links x JOIN terms t ON t.id = x.term_id JOIN term_tree r ON r.term_id = x.term_id
                 ORDER BY 1, 2";
    assert_eq!(rows(&c, links), old_links, "links move to the core unchanged");
    assert!(!exists(&c, "links"));
    // The names version now counts writes to the core's display names, as it counted the legacy table's.
    let version = names_version(&c);
    c.execute("INSERT INTO term_key_names VALUES ('skin','synthetic_skin','zh','user','合成皮肤名')", []).unwrap();
    assert_eq!(names_version(&c), version + 1);
    // … and to the core's names and links of characters, skins and works, not to artists'.
    c.execute(
        "INSERT INTO term_names(term_id, lang, value, source, role, seq)
         SELECT term_id, 'zh', '合成名', 'user', 'alias', 0 FROM term_tree LIMIT 1",
        [],
    )
    .unwrap();
    assert_eq!(names_version(&c), version + 2);
    c.execute(
        "INSERT INTO term_links(system, key, term_id, source) SELECT 'danbooru', 'synthetic_tag', term_id, 'user' FROM term_tree LIMIT 1",
        [],
    )
    .unwrap();
    assert_eq!(names_version(&c), version + 3);
    c.execute_batch(
        "INSERT INTO terms(key, kind, origin) VALUES ('synthetic_artist', 'artist', 'user');
         INSERT INTO term_names(term_id, lang, value, source, role)
         SELECT id, 'zh', '合成画师', 'user', 'display' FROM terms WHERE key = 'synthetic_artist';
         INSERT INTO term_links(system, key, term_id, source)
         SELECT 'pixiv', '42', id, 'user' FROM terms WHERE key = 'synthetic_artist';",
    )
    .unwrap();
    assert_eq!(names_version(&c), version + 3);
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
    for (name, sql, refusal) in [
        (
            "disagree-entities",
            "UPDATE entities SET work='改过' WHERE rowid=(SELECT min(rowid) FROM entities)",
            "旧兼容表与词条核心不一致",
        ),
        (
            "disagree-links",
            "UPDATE links SET current=1-current WHERE rowid=(SELECT min(rowid) FROM links)",
            "旧链接表与词条核心不一致",
        ),
    ] {
        let lib = library(name);
        write(&Connection::open(&lib.db).unwrap(), sql);
        let before = exact(&lib.db);
        let refused = upgrade(&lib.db).unwrap_err();
        assert!(refused.contains(refusal), "{name}: {refused}");
        assert_eq!(exact(&lib.db), before);
    }
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
