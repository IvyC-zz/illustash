use super::*;
use crate::store::{state, undo, Undo};
use std::path::PathBuf;

/// provenance/db.py（#9 已删除） v4 里本模块读的几张表（原样）。
const PROVENANCE: &str = "
CREATE TABLE artist_names(artist_id TEXT NOT NULL REFERENCES artists(id), source TEXT NOT NULL, name TEXT NOT NULL,
 updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, PRIMARY KEY(artist_id,source));
CREATE TABLE artists(id TEXT PRIMARY KEY, merged_into TEXT REFERENCES artists(id), created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
CREATE TABLE artist_links(system TEXT NOT NULL, key TEXT NOT NULL, artist_id TEXT NOT NULL REFERENCES artists(id),
 url TEXT NOT NULL DEFAULT '', source TEXT NOT NULL, added_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, PRIMARY KEY(system, key));
CREATE TABLE image_artists(asset TEXT NOT NULL, artist_id TEXT NOT NULL REFERENCES artists(id), method TEXT NOT NULL, score REAL,
 evidence TEXT NOT NULL DEFAULT '{}', status TEXT NOT NULL CHECK(status IN ('auto','candidate')),
 updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, PRIMARY KEY(asset, artist_id));
INSERT INTO artists(id) VALUES ('ART-A'),('ART-B');
INSERT INTO artist_links(system,key,artist_id,url,source) VALUES
 ('danbooru','foo_(bar)','ART-A','https://danbooru.donmai.us/artists?name=foo_(bar)','danbooru_post'),
 ('x','foo_bar','ART-A','https://x.com/foo_bar','danbooru_artist'),
 ('x','plain','ART-B','','ocr');
INSERT INTO artist_names(artist_id,source,name) VALUES ('ART-A','pixiv','フー');
INSERT INTO image_artists(asset,artist_id,method,score,evidence,status,updated_at) VALUES
 ('a','ART-A','pixel_hash',1,'{\"danbooru_post\": 1}','auto','2026-09-28 01:00:00'),
 ('b','ART-A','pixel_hash',1,'{\"danbooru_post\": 2}','auto','2026-09-28 01:00:00'),
 ('c','ART-B','ocr',0.8,'{\"text\": \"@plain\"}','candidate','2026-09-28 01:00:00'),
 ('gone','ART-B','pixel_hash',1,'{}','auto','2026-09-28 01:00:00');
";

/// tests/reference/web/store.py（#9 已删除） 里本模块读的几张表，以及一段画师判断历史：
/// 1 判 a 是 ART-A；2 判 b 是 ART-A；3 改判 a 不是 ART-A；4 判 c 是 ART-B 后又撤销；5 别的操作（标签）。
const LIBRARY_SQL: &str = r#"
CREATE TABLE meta(key TEXT PRIMARY KEY,value TEXT NOT NULL);
CREATE TABLE actions(id INTEGER PRIMARY KEY AUTOINCREMENT,label TEXT NOT NULL,before_json TEXT NOT NULL,ts TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,undone INTEGER NOT NULL DEFAULT 0);
CREATE TABLE assets(uuid TEXT PRIMARY KEY,filename TEXT,src_path TEXT,width INTEGER,height INTEGER,added_at TEXT,src_kind TEXT);
CREATE TABLE artist_verdicts(uuid TEXT NOT NULL,artist TEXT NOT NULL,verdict TEXT NOT NULL CHECK(verdict IN ('yes','no')),created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,PRIMARY KEY(uuid,artist));
INSERT INTO assets(uuid) VALUES ('a'),('b'),('c'),('d');
INSERT INTO actions(label,before_json) VALUES ('已确认 1 张是 A 画的','[{"artist_verdict":{"uuid":"a","artist":"ART-A","had":null}}]');
INSERT INTO actions(label,before_json) VALUES ('已确认 1 张是 A 画的','[{"artist_verdict":{"uuid":"b","artist":"ART-A","had":null}}]');
INSERT INTO actions(label,before_json) VALUES ('已标记 1 张不是 A 画的','[{"artist_verdict":{"uuid":"a","artist":"ART-A","had":{"verdict":"yes","created_at":"2026-09-28 02:00:00"}}}]');
INSERT INTO actions(label,before_json,undone) VALUES ('已确认 1 张是 B 画的','[{"artist_verdict":{"uuid":"c","artist":"ART-B","had":null}}]',1);
INSERT INTO actions(label,before_json) VALUES ('添加标签','[{"uuid":"d","tag":"x","had":null}]');
INSERT INTO artist_verdicts(uuid,artist,verdict) VALUES ('a','ART-A','no'),('b','ART-A','yes');
"#;

struct Fixture {
    dir: PathBuf,
    conn: Connection,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Fixture {
    fn new(name: &str) -> Fixture {
        let dir = std::env::temp_dir().join(format!("illustash-legacy-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        crate::sqlite::open(dir.join("provenance.sqlite")).unwrap().execute_batch(PROVENANCE).unwrap();
        let mut conn = crate::sqlite::open(dir.join("library.sqlite")).unwrap();
        crate::configure(&conn).unwrap();
        conn.execute_batch(LIBRARY_SQL).unwrap();
        crate::migrate(&mut conn).unwrap();
        Fixture { dir, conn }
    }

    fn sync(&mut self) -> Result<Report> {
        sync(&mut self.conn, &self.dir.join("provenance.sqlite"))
    }

    fn prov(&self, sql: &str) {
        crate::sqlite::open(self.dir.join("provenance.sqlite")).unwrap().execute_batch(sql).unwrap();
    }

    fn asset(&self, key: &str) -> AssetId {
        self.conn
            .query_row("SELECT id FROM term_assets WHERE namespace='library' AND key=?1", [key], |x| x.get(0))
            .unwrap()
    }

    fn term(&self, key: &str) -> TermId {
        self.conn.query_row("SELECT id FROM terms WHERE key=?1", [key], |x| x.get(0)).unwrap()
    }

    fn state(&self, asset: &str, artist: &str) -> Option<String> {
        state(&self.conn, self.asset(asset), self.term(artist)).unwrap()
    }

    fn count(&self, sql: &str) -> i64 {
        self.conn.query_row(sql, [], |x| x.get(0)).unwrap()
    }
}

#[test]
fn builds_artist_terms_evidence_and_verdicts() {
    let mut f = Fixture::new("build");
    let r = f.sync().unwrap();
    assert_eq!((r.terms_created, r.evidence_added, r.verdicts_added, r.verdict_pairs_checked), (2, 4, 3, 2));
    assert_eq!((r.assets_added, r.assets_removed), (4, 0));
    // 链接：旧表三条，加每位画师一条 provenance 链接；名字一条。
    assert_eq!(f.count("SELECT count(*) FROM term_links"), 5);
    assert_eq!(term_by_link(&f.conn, "provenance", "ART-A"), Some(f.term("ART-A")));
    assert_eq!(f.count("SELECT count(*) FROM term_names WHERE value='フー'"), 1);
    // 像素哈希是强证据，OCR 只是「也许是」；人判的「不是」「是」先于证据。
    assert_eq!(f.state("a", "ART-A").as_deref(), Some("rejected"));
    assert_eq!(f.state("b", "ART-A").as_deref(), Some("confirmed"));
    assert_eq!(f.state("c", "ART-B").as_deref(), Some("maybe"));
    // 不在图库里的图：登记为已移除，证据照留，不投影。
    assert_eq!(f.count("SELECT count(*) FROM term_assets WHERE key='gone' AND removed_at IS NOT NULL"), 1);
    assert_eq!(
        f.count("SELECT count(*) FROM term_evidence e JOIN term_assets a ON a.id=e.asset_id WHERE a.key='gone'"),
        1
    );
    assert_eq!(f.state("gone", "ART-B"), None);
    // 裁决挂在原来那一步上；撤销了的第 4 步不收。
    let steps: Vec<(i64, String)> = f
        .conn
        .prepare("SELECT action_id,verdict FROM term_verdicts ORDER BY action_id")
        .unwrap()
        .query_map([], |x| Ok((x.get(0)?, x.get(1)?)))
        .unwrap()
        .map(|x| x.unwrap())
        .collect();
    assert_eq!(steps, [(1, "yes".into()), (2, "yes".into()), (3, "no".into())]);
    let status: String = f
        .conn
        .query_row("SELECT json_extract(data,'$.status') FROM term_evidence WHERE method='ocr'", [], |x| x.get(0))
        .unwrap();
    assert_eq!(status, "candidate");
}

fn term_by_link(conn: &Connection, system: &str, key: &str) -> Option<TermId> {
    crate::store::term_by_link(conn, system, key).unwrap()
}

#[test]
fn rerunning_adds_nothing_and_follows_changes() {
    let mut f = Fixture::new("rerun");
    f.sync().unwrap();
    let again = f.sync().unwrap();
    assert_eq!(
        again,
        Report { evidence_unchanged: 4, verdict_pairs_checked: 2, projected: 2, ..Report::default() },
        "第二次同步不写任何东西，只重投带旧裁决的图"
    );
    // 旧表改写了一行（OCR 换了分数）、画师改了名、链接移到另一位、一张图被移除、用户又判了一次。
    f.prov(
        "UPDATE image_artists SET score=0.9,updated_at='2026-09-28 03:00:00' WHERE asset='c';
         INSERT OR REPLACE INTO artist_names(artist_id,source,name) VALUES ('ART-A','pixiv','Foo');
         UPDATE artist_links SET artist_id='ART-B' WHERE system='x' AND key='foo_bar';",
    );
    f.conn
        .execute_batch(
            r#"DELETE FROM assets WHERE uuid='b';
               INSERT INTO actions(label,before_json) VALUES ('已标记 1 张不是 B 画的','[{"artist_verdict":{"uuid":"c","artist":"ART-B","had":null}}]');
               INSERT INTO artist_verdicts(uuid,artist,verdict) VALUES ('c','ART-B','no');"#,
        )
        .unwrap();
    let r = f.sync().unwrap();
    assert_eq!((r.evidence_added, r.evidence_unchanged, r.verdicts_added), (1, 3, 1));
    assert_eq!((r.names_changed, r.links_changed, r.assets_removed), (2, 1, 1));
    assert_eq!(f.count("SELECT count(*) FROM term_evidence WHERE method='ocr'"), 2, "旧的一条留着");
    assert_eq!(f.count("SELECT count(*) FROM term_names WHERE value='フー'"), 0);
    assert_eq!(term_by_link(&f.conn, "x", "foo_bar"), Some(f.term("ART-B")));
    assert_eq!(f.state("c", "ART-B").as_deref(), Some("rejected"));
    assert_eq!(f.state("b", "ART-A"), None, "移除的图不投影");
}

#[test]
fn undoing_a_legacy_step_through_the_core_matches_python() {
    let mut f = Fixture::new("undo");
    f.sync().unwrap();
    f.conn.execute("UPDATE actions SET undone=1 WHERE id=5", []).unwrap(); // 先把最近的标签操作撤掉
                                                                           // 最近一步是第 3 步（改判不是）：核心撤它，a 回到「是」，与 Python 把 had 写回 artist_verdicts 的结果一致。
    assert!(matches!(undo(&mut f.conn, Some(3)).unwrap(), Undo::Foreign { action: 3 }), "旧步骤仍归 Python 撤");
    f.conn.execute("UPDATE actions SET undone=1 WHERE id=3", []).unwrap();
    f.conn.execute("UPDATE artist_verdicts SET verdict='yes' WHERE uuid='a'", []).unwrap();
    let r = f.sync().unwrap();
    assert_eq!(r.verdicts_added, 0);
    assert_eq!(f.state("a", "ART-A").as_deref(), Some("confirmed"));
}

#[test]
fn a_verdict_without_history_rolls_everything_back() {
    let mut f = Fixture::new("orphan");
    f.conn.execute("INSERT INTO artist_verdicts(uuid,artist,verdict) VALUES ('d','ART-B','yes')", []).unwrap();
    let err = f.sync().unwrap_err();
    assert!(matches!(err, Error::Conflict(ref m) if m.contains("找不到对应的历史步骤")), "{err}");
    assert_eq!(f.count("SELECT count(*) FROM terms"), 0);
    assert_eq!(f.count("SELECT count(*) FROM term_assets"), 0);
    // 失败后已经卸下 provenance，可以再试。
    assert_eq!(f.count("SELECT count(*) FROM pragma_database_list WHERE name='prov'"), 0);
}

#[test]
fn a_history_that_disagrees_with_the_table_is_refused() {
    let mut f = Fixture::new("disagree");
    f.sync().unwrap();
    // 绕过历史直接改表：核心里第 3 步说「不是」，表里却是「是」。
    f.conn.execute("UPDATE artist_verdicts SET verdict='yes' WHERE uuid='a'", []).unwrap();
    let err = f.sync().unwrap_err();
    assert!(matches!(err, Error::Conflict(ref m) if m.contains("绕过历史")), "{err}");
    // 表里少了一条也一样。
    f.conn.execute("UPDATE artist_verdicts SET verdict='no' WHERE uuid='a'", []).unwrap();
    f.conn.execute("DELETE FROM artist_verdicts WHERE uuid='b'", []).unwrap();
    assert!(f.sync().is_err());
}

#[test]
fn merges_become_same_as_evidence_and_links_on_other_kinds_are_refused() {
    let mut f = Fixture::new("merge");
    f.prov("UPDATE artists SET merged_into='ART-A' WHERE id='ART-B'");
    f.sync().unwrap();
    let state: String =
        f.conn.query_row("SELECT state FROM term_relation_states WHERE kind='same_as'", [], |x| x.get(0)).unwrap();
    assert_eq!(state, "maybe");
    let topic = create_term(&f.conn, Kind::Topic, Some("t"), "user").unwrap();
    f.conn
        .execute("INSERT INTO term_links(system,key,term_id,source) VALUES ('danbooru','shared',?1,'user')", [topic])
        .unwrap();
    f.prov("INSERT INTO artist_links(system,key,artist_id,source) VALUES ('danbooru','shared','ART-A','x')");
    assert!(matches!(f.sync(), Err(Error::Conflict(ref m)) if m.contains("danbooru:shared")));
}

#[test]
fn paths_with_non_ascii_attach_read_only() {
    assert_eq!(
        file_uri(Path::new("/tmp/我的 图库/p.sqlite")),
        "file:/tmp/%E6%88%91%E7%9A%84%20%E5%9B%BE%E5%BA%93/p.sqlite?mode=ro"
    );
    let mut f = Fixture::new("readonly");
    f.sync().unwrap();
    // 只读挂上：同步过程中不可能写溯源库。
    f.conn.execute("ATTACH DATABASE ?1 AS prov", [file_uri(&f.dir.join("provenance.sqlite"))]).unwrap();
    assert!(f.conn.execute("DELETE FROM prov.artists", []).is_err());
    f.conn.execute("DETACH DATABASE prov", []).unwrap();
}

#[test]
fn needs_the_current_schema_and_the_file() {
    let mut f = Fixture::new("schema");
    assert!(matches!(sync(&mut f.conn, &f.dir.join("missing.sqlite")), Err(Error::Invalid(_))));
    let mut bare = Connection::open_in_memory().unwrap();
    assert!(matches!(sync(&mut bare, &f.dir.join("provenance.sqlite")), Err(Error::Schema(_))));
}

#[test]
fn cutover_drops_the_old_table_and_hands_old_steps_to_the_core() {
    let mut f = Fixture::new("cutover");
    let r = cutover(&mut f.conn, Some(&f.dir.join("provenance.sqlite"))).unwrap().unwrap();
    assert_eq!(r.verdicts_added, 3);
    assert!(crate::store::artists_in_core(&f.conn).unwrap());
    assert_eq!(cutover(&mut f.conn, Some(&f.dir.join("provenance.sqlite"))).unwrap(), None, "切过就不再切");
    assert!(matches!(f.sync(), Err(Error::Conflict(ref m)) if m.contains("已切到词条核心")));
    // 最近的标签操作归 Python；它撤掉之后，旧的第 3 步（改判不是）由核心撤：a 回到「是」。
    assert!(matches!(undo(&mut f.conn, Some(5)).unwrap(), Undo::Foreign { action: 5 }));
    f.conn.execute("UPDATE actions SET undone=1 WHERE id=5", []).unwrap();
    assert!(matches!(undo(&mut f.conn, Some(3)).unwrap(), Undo::Undone { action: 3, .. }));
    assert_eq!(f.state("a", "ART-A").as_deref(), Some("confirmed"));
    let kept: bool = f
        .conn
        .query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='artist_verdicts')", [], |x| x.get(0))
        .unwrap();
    assert!(!kept, "第 4 版起切换不留冻结副本");
}

#[test]
fn cutover_without_provenance_keeps_the_human_verdicts() {
    let mut f = Fixture::new("noprov");
    let r = cutover(&mut f.conn, None).unwrap().unwrap();
    // 没有溯源库：画师词条从判断里建出来，证据为零，判断照样收。
    assert_eq!((r.terms_created, r.evidence_added, r.verdicts_added), (1, 0, 3));
    assert_eq!(f.state("b", "ART-A").as_deref(), Some("confirmed"));
    assert_eq!(f.count("SELECT count(*) FROM pragma_database_list WHERE name='prov'"), 0);
}
