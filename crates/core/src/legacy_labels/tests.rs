use super::*;
use crate::package::tests::build;
use crate::package::{Package, FORMAT};
use crate::store::{import_package_with, state, undo, Undo};

/// tests/reference/web/store.py（#9 已删除） 里本模块读的表（原样，含索引），以及一段用户标签历史：
/// 1 给 a、b 贴上 summer；2 从 b 去掉 summer；3 给 c 贴 x 后撤销。
const LIBRARY_SQL: &str = r#"
CREATE TABLE meta(key TEXT PRIMARY KEY,value TEXT NOT NULL);
CREATE TABLE actions(id INTEGER PRIMARY KEY AUTOINCREMENT,label TEXT NOT NULL,before_json TEXT NOT NULL,ts TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,undone INTEGER NOT NULL DEFAULT 0);
CREATE TABLE assets(uuid TEXT PRIMARY KEY,filename TEXT,src_path TEXT,width INTEGER,height INTEGER,added_at TEXT,src_kind TEXT);
CREATE TABLE general_hits(uuid TEXT NOT NULL,value TEXT NOT NULL);
CREATE INDEX idx_general_value ON general_hits(value,uuid);
CREATE TABLE asset_tags(uuid TEXT NOT NULL,tag TEXT NOT NULL,origin TEXT NOT NULL,created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,PRIMARY KEY(uuid,tag));
CREATE INDEX asset_tags_tag ON asset_tags(tag,uuid);
CREATE TABLE asset_rating(uuid TEXT PRIMARY KEY,rating TEXT NOT NULL);
INSERT INTO assets(uuid) VALUES ('a'),('b'),('c');
INSERT INTO general_hits VALUES ('a','solo'),('b','1girl');
INSERT INTO asset_rating VALUES ('a','general'),('b','explicit'),('c','sensitive');
INSERT INTO actions(label,before_json) VALUES ('已给 2 张加上标签 summer','[{"uuid":"a","tag":"summer","had":null},{"uuid":"b","tag":"summer","had":null}]');
INSERT INTO actions(label,before_json) VALUES ('已从 1 张去掉标签 summer','[{"uuid":"b","tag":"summer","had":{"origin":"review","created_at":"2026-09-28 01:00:00"}}]');
INSERT INTO actions(label,before_json,undone) VALUES ('已给 1 张加上标签 x','[{"uuid":"c","tag":"x","had":null}]',1);
INSERT INTO asset_tags(uuid,tag,origin) VALUES ('a','summer','review');
"#;

fn library() -> Connection {
    let mut c = Connection::open_in_memory().unwrap();
    crate::configure(&c).unwrap();
    c.execute_batch(LIBRARY_SQL).unwrap();
    crate::migrate(&mut c).unwrap();
    c
}

fn wd(asset: &str, dim: &str, value: &str, p: f64) -> Value {
    let (kind, system) = if dim == "rating" { ("rating", "wd_rating") } else { ("topic", "wd") };
    json!({"id": format!("{asset}:{dim}:{value}"), "asset": {"namespace": "library", "id": asset},
           "term": {"kind": kind, "link": {"system": system, "key": value}},
           "source": "wd", "method": "wd_v3", "version": "swinv2_448", "score": {"value": p, "metric": "wd_probability"},
           "observed_at": "2026-09-28T00:00:00Z", "data": {}})
}

fn deliver(c: &mut Connection, assets: &[&str], rows: &[Value], new_assets: bool) {
    let manifest = json!({"format": FORMAT, "library_id": "lib", "producer": {"name": "wd", "version": "labels"},
        "created_at": "2026-09-28T00:00:00Z", "files": {},
        "assets": assets.iter().map(|a| json!({"asset": {"namespace": "library", "id": a}, "status": "completed", "error": null})).collect::<Vec<_>>()});
    let (m, e) = build(manifest, rows);
    import_package_with(c, &Package::from_parts(&m, &e).unwrap(), "lib", new_assets).unwrap();
}

/// 与夹具里旧表一致的 WD 证据（题材 ≥ 0.35 的正好是 general_hits；每张图最高的分级正好是 asset_rating）。
fn deliver_all(c: &mut Connection) {
    let rows = [
        wd("a", "general", "solo", 0.9),
        wd("a", "general", "beach", 0.25),
        wd("b", "general", "1girl", 0.8),
        wd("a", "rating", "general", 0.7),
        wd("a", "rating", "sensitive", 0.2),
        wd("b", "rating", "explicit", 0.6),
        wd("b", "rating", "questionable", 0.3),
        wd("c", "rating", "sensitive", 0.9),
    ];
    deliver(c, &["a", "b", "c"], &rows, false);
}

fn rows(c: &Connection, sql: &str) -> Vec<(String, String)> {
    c.prepare(sql).unwrap().query_map([], |x| Ok((x.get(0)?, x.get(1)?))).unwrap().map(|x| x.unwrap()).collect()
}

#[test]
fn cutover_waits_for_every_rated_picture() {
    let mut c = library();
    deliver(&mut c, &["a", "b"], &[wd("a", "rating", "general", 0.7), wd("b", "rating", "explicit", 0.6)], false);
    let err = cutover(&mut c).unwrap_err();
    assert!(matches!(err, Error::Conflict(ref m) if m.contains("还有 1 张")), "{err}");
    assert_eq!(rows(&c, "SELECT uuid,tag FROM asset_tags"), [("a".into(), "summer".into())], "旧表原样");
}

#[test]
fn cutover_swaps_the_tables_for_views_that_read_the_same() {
    let mut c = library();
    let before = (
        rows(&c, "SELECT uuid,value FROM general_hits ORDER BY 1,2"),
        rows(&c, "SELECT uuid,rating FROM asset_rating ORDER BY 1"),
        rows(&c, "SELECT uuid,tag FROM asset_tags ORDER BY 1,2"),
    );
    deliver_all(&mut c);
    let r = cutover(&mut c).unwrap().unwrap();
    assert_eq!((r.concepts_created, r.verdicts_added, r.tag_pairs_checked, r.rated_assets), (1, 3, 2, 3));
    let after = (
        rows(&c, "SELECT uuid,value FROM general_hits ORDER BY 1,2"),
        rows(&c, "SELECT uuid,rating FROM asset_rating ORDER BY 1"),
        rows(&c, "SELECT uuid,tag FROM asset_tags ORDER BY 1,2"),
    );
    assert_eq!(before, after);
    let kinds: Vec<(String, String)> = rows(
        &c,
        "SELECT name,type FROM sqlite_master WHERE name IN ('general_hits','asset_rating','asset_tags','legacy_asset_tags') ORDER BY name",
    );
    assert_eq!(
        kinds,
        [("asset_rating", "view"), ("asset_tags", "view"), ("general_hits", "view")]
            .map(|(a, b)| (a.to_string(), b.to_string()))
    );
    assert!(crate::store::labels_in_core(&c).unwrap());
    assert_eq!(cutover(&mut c).unwrap(), None, "切过就不再切");
    // Python 升级 schema 时会重跑建表语句：CREATE TABLE IF NOT EXISTS 碰到同名视图不报错；给视图建索引则报错，
    // 即使索引名还挂在冻结的旧表上。所以 tests/reference/web/store.py（#9 已删除） 升级时要跳过这三个名字上的建索引语句。
    c.execute_batch(
        "CREATE TABLE IF NOT EXISTS asset_tags(uuid TEXT,tag TEXT);
         CREATE TABLE IF NOT EXISTS general_hits(uuid TEXT,value TEXT); CREATE TABLE IF NOT EXISTS asset_rating(uuid TEXT,rating TEXT);",
    )
    .unwrap();
    assert!(c.execute("CREATE INDEX IF NOT EXISTS asset_tags_tag ON asset_tags(tag,uuid)", []).is_err());
    // 图从图库移除：视图里跟着消失。
    c.execute("DELETE FROM assets WHERE uuid='b'", []).unwrap();
    assert_eq!(rows(&c, "SELECT uuid,value FROM general_hits"), [("a".into(), "solo".into())]);
}

#[test]
fn old_tag_steps_are_undone_by_the_core_after_cutover() {
    let mut c = library();
    deliver_all(&mut c);
    cutover(&mut c).unwrap();
    // 最近一步是第 2 步（从 b 去掉 summer）：核心撤它，b 又贴着 summer。
    assert!(matches!(undo(&mut c, Some(2)).unwrap(), Undo::Undone { action: 2, .. }));
    assert_eq!(
        rows(&c, "SELECT uuid,tag FROM asset_tags ORDER BY 1"),
        [("a".into(), "summer".into()), ("b".into(), "summer".into())]
    );
    // 新的「加上」直接写核心：概念按名字找到同一个。
    let a: AssetId = c.query_row("SELECT id FROM term_assets WHERE key='c'", [], |x| x.get(0)).unwrap();
    let summer = concept_by_name(&c, "summer", None).unwrap().unwrap();
    crate::store::judge(
        &mut c,
        &[crate::store::VerdictTarget::Image { asset: a, term: summer }],
        crate::store::Verdict::Yes,
        "user",
        "",
        "加上",
    )
    .unwrap();
    assert_eq!(state(&c, a, summer).unwrap().as_deref(), Some("confirmed"));
    assert_eq!(rows(&c, "SELECT uuid,tag FROM asset_tags WHERE uuid='c'"), [("c".into(), "summer".into())]);
    assert_eq!(concept_by_name(&c, "nope", None).unwrap(), None);
}

#[test]
fn a_tag_without_history_rolls_everything_back() {
    let mut c = library();
    c.execute("INSERT INTO asset_tags(uuid,tag,origin) VALUES ('c','orphan','review')", []).unwrap();
    deliver_all(&mut c);
    assert!(matches!(cutover(&mut c), Err(Error::Conflict(ref m)) if m.contains("找不到对应的历史步骤")));
    assert!(!crate::store::labels_in_core(&c).unwrap());
    let kind: String =
        c.query_row("SELECT type FROM sqlite_master WHERE name='general_hits'", [], |x| x.get(0)).unwrap();
    assert_eq!(kind, "table");
    assert_eq!(c.query_row("SELECT count(*) FROM terms WHERE kind='concept'", [], |x| x.get::<_, i64>(0)).unwrap(), 0);
}

#[test]
fn new_pictures_can_be_delivered_before_they_are_listed() {
    let mut c = library();
    deliver_all(&mut c);
    cutover(&mut c).unwrap();
    // 同步导入新图：WD 证据先到（图还不在 assets 里），视图里看不到；Python 把图写进 assets 后就出现。
    deliver(&mut c, &["d"], &[wd("d", "rating", "general", 0.8), wd("d", "general", "solo", 0.5)], true);
    assert_eq!(rows(&c, "SELECT uuid,rating FROM asset_rating WHERE uuid='d'"), []);
    c.execute("INSERT INTO assets(uuid) VALUES ('d')", []).unwrap();
    assert_eq!(rows(&c, "SELECT uuid,value FROM general_hits WHERE uuid='d'"), [("d".into(), "solo".into())]);
}
