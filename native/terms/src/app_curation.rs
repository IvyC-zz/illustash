//! Human collection and folding projections, matching Store.curate/set_fold_kinds.
use crate::{http::LibraryContext, http_json::Json, Error, Result};
use rusqlite::{params, params_from_iter, Connection, OpenFlags, OptionalExtension, TransactionBehavior};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

pub(crate) fn curate(c: &mut Connection, context: &LibraryContext, body: &Json) -> Result<Json> {
    let action = match body.get("action") {
        Some(Json::Scalar(Value::String(s))) if matches!(s.as_str(), "exclude" | "include" | "reset") => s.as_str(),
        _ => return Err(Error::Invalid("未知操作".into())),
    };
    let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let ids = crate::app_commands::selected(&tx, context, body)?;
    // Persisted JSON has Python's default ASCII escaping and field insertion order.
    // HTTP Json/canonical JSON must not reorder or re-encode this history.
    let mut before = Vec::new();
    for id in &ids {
        let had: Option<(String, String, String)> = tx
            .query_row("SELECT status,origin,created_at FROM curation WHERE uuid=?", [id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .optional()?;
        let previous = match had {
            Some((status, origin, created)) => format!(
                "{{\"status\": {}, \"origin\": {}, \"created_at\": {}}}",
                ascii(&status),
                ascii(&origin),
                ascii(&created)
            ),
            None => "null".into(),
        };
        before.push(format!("{{\"uuid\": {}, \"curation\": {previous}}}", ascii(id)));
        if action == "reset" {
            tx.execute("DELETE FROM curation WHERE uuid=?", [id])?;
        } else {
            tx.execute(
                "INSERT OR REPLACE INTO curation(uuid,status,origin) VALUES (?,?,'review')",
                params![id, format!("{action}d")],
            )?;
        }
    }
    refresh_excluded(&tx, &ids)?;
    let encoded = serde_json::to_string(&ids).map_err(|e| Error::Invalid(e.to_string()))?;
    let groups: HashMap<String, String> = tx
        .prepare("SELECT uuid,grp FROM duplicates WHERE uuid IN (SELECT value FROM json_each(?))")?
        .query_map([encoded], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let count = ids.iter().map(|id| groups.get(id).unwrap_or(id)).collect::<HashSet<_>>().len();
    let label = match action {
        "exclude" => format!("已排除 {n} 张", n = count),
        "include" => format!("已收录 {n} 张", n = count),
        _ => format!("已将 {n} 张交回自动规则", n = count),
    };
    tx.execute(
        "INSERT INTO actions(label,before_json) VALUES (?,?)",
        params![label, format!("[{}]", before.join(", "))],
    )?;
    let action = crate::app_commands::last_action(&tx)?;
    tx.commit()?;
    Ok(Json::object([("undo", action)]))
}
fn ascii(s: &str) -> String {
    crate::character_json::dump(&Value::String(s.into()), true)
}

pub(crate) fn refresh_excluded(c: &Connection, ids: &[String]) -> Result<()> {
    c.execute_batch("CREATE TEMP TABLE IF NOT EXISTS touched(uuid TEXT PRIMARY KEY); DELETE FROM touched;")?;
    for id in ids {
        c.execute("INSERT OR IGNORE INTO touched VALUES (?)", [id])?;
    }
    c.execute("DELETE FROM excluded WHERE uuid IN (SELECT uuid FROM touched)", [])?;
    c.execute("INSERT INTO excluded SELECT a.uuid,coalesce(r.reason,'manual'),CASE WHEN k.status='excluded' THEN 'review' ELSE 'rule' END FROM assets a LEFT JOIN curation k ON k.uuid=a.uuid LEFT JOIN auto_rules r ON r.uuid=a.uuid AND r.kind='exclude' WHERE (k.status='excluded' OR (r.uuid IS NOT NULL AND k.uuid IS NULL)) AND a.uuid IN (SELECT uuid FROM touched)", [])?;
    Ok(())
}

pub(crate) fn fold(c: &mut Connection, context: &LibraryContext, body: &Json) -> Result<Json> {
    const ALLOWED: &[&str] = &["identical", "duplicate", "subtle", "crop", "variant", "edit"];
    let Some(Json::Array(values)) = body.get("kinds") else {
        return Err(Error::Invalid("未知关系".into()));
    };
    let mut kinds = Vec::new();
    for value in values {
        let Json::Scalar(Value::String(kind)) = value else {
            return Err(Error::Invalid("未知关系".into()));
        };
        if !ALLOWED.contains(&kind.as_str()) {
            return Err(Error::Invalid("未知关系".into()));
        }
        kinds.push(kind.clone());
    }
    kinds.sort();
    kinds.dedup();
    let path = context.source.as_ref().ok_or_else(|| Error::Io(std::io::Error::other("图库缺少源库")))?;
    let mut source = crate::sqlite::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    source.busy_timeout(Duration::from_secs(20))?;
    source.pragma_update(None, "query_only", true)?;
    let src = source.transaction()?;
    let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let persisted =
        crate::character_json::dump(&Value::Array(kinds.iter().cloned().map(Value::String).collect()), true);
    tx.execute("INSERT OR REPLACE INTO prefs VALUES ('fold_kinds',?)", [persisted])?;
    refresh_duplicates(&tx, &src, &kinds)?;
    crate::character_projection::fold(&tx)?;
    tx.commit()?;
    Ok(Json::object([("kinds", Json::Array(kinds.into_iter().map(Json::from).collect()))]))
}

// Python dict insertion order is observable through duplicates rowids. A union evaluates its RHS first.
#[derive(Default)]
struct Parents {
    index: HashMap<String, usize>,
    ids: Vec<String>,
    links: Vec<usize>,
}
impl Parents {
    fn find(&mut self, id: &str) -> usize {
        let at = if let Some(at) = self.index.get(id) {
            *at
        } else {
            let at = self.ids.len();
            self.ids.push(id.into());
            self.links.push(at);
            self.index.insert(id.into(), at);
            at
        };
        let mut u = at;
        while self.links[u] != u {
            self.links[u] = self.links[self.links[u]];
            u = self.links[u];
        }
        u
    }
    fn join(&mut self, a: &str, b: &str) {
        let b = self.find(b);
        let a = self.find(a);
        self.links[a] = b;
    }
}
pub(crate) fn refresh_duplicates(c: &Connection, src: &Connection, kinds: &[String]) -> Result<()> {
    let assets: HashMap<String, (String, i128)> = c
        .prepare("SELECT uuid,coalesce(added_at,''),coalesce(width,0),coalesce(height,0) FROM assets")?
        .query_map([], |r| {
            Ok((r.get(0)?, (r.get(1)?, i128::from(r.get::<_, i64>(2)?) * i128::from(r.get::<_, i64>(3)?))))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut parent = Parents::default();
    let mut keys: HashMap<String, String> = HashMap::new();
    let has_hash = src
        .prepare("PRAGMA table_info(assets)")?
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .iter()
        .any(|name| name == "stable_hash");
    if has_hash {
        let mut first: HashMap<String, String> = HashMap::new();
        for row in src
            .prepare("SELECT uuid,stable_hash FROM assets WHERE coalesce(stable_hash,'')<>''")?
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        {
            let (id, key) = row?;
            if assets.contains_key(&id) {
                let first = first.entry(key.clone()).or_insert_with(|| id.clone());
                parent.join(&id, first);
                keys.insert(id, key);
            }
        }
    }
    if !kinds.is_empty() {
        let marks = std::iter::repeat_n("?", kinds.len()).collect::<Vec<_>>().join(",");
        for row in c
            .prepare(&format!("SELECT a,b FROM relations WHERE kind IN ({marks})"))?
            .query_map(params_from_iter(kinds), |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        {
            let (a, b) = row?;
            if assets.contains_key(&a) && assets.contains_key(&b) {
                parent.join(&a, &b);
            }
        }
    }
    let mut roots = HashMap::new();
    let mut groups: Vec<Vec<String>> = Vec::new();
    for id in parent.ids.clone() {
        let root = parent.find(&id);
        let slot = *roots.entry(root).or_insert_with(|| {
            groups.push(Vec::new());
            groups.len() - 1
        });
        groups[slot].push(id);
    }
    // Same-key groups overwrite the earlier value but keep its dict position (old Python comprehension).
    let mut slots: HashMap<String, usize> = HashMap::new();
    let mut keyed: Vec<(String, Vec<String>)> = Vec::new();
    for group in groups {
        let key = group
            .iter()
            .filter_map(|id| keys.get(id))
            .min()
            .cloned()
            .unwrap_or_else(|| format!("sim:{}", group.iter().min().expect("nonempty")));
        if let Some(&slot) = slots.get(&key) {
            keyed[slot] = (key, group);
        } else {
            slots.insert(key.clone(), keyed.len());
            keyed.push((key, group));
        }
    }
    c.execute("DELETE FROM duplicates", [])?;
    for (key, mut members) in keyed {
        if members.len() < 2 {
            continue;
        }
        members.sort_by(|a, b| (&assets[a].0, a).cmp(&(&assets[b].0, b)));
        let best = members.iter().min_by_key(|id| (-assets[*id].1, &assets[*id].0, *id)).expect("nonempty");
        let added = &assets[members.last().expect("nonempty")].0;
        for (pos, id) in members.iter().enumerate() {
            c.execute(
                "INSERT INTO duplicates(uuid,grp,size,pos,grp_added,rep) VALUES (?,?,?,?,?,?)",
                params![id, key, members.len() as i64, pos as i64 + 1, added, id == best],
            )?;
        }
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
        c: Connection,
        context: LibraryContext,
    }
    impl Fixture {
        fn new(hash: bool) -> Self {
            let root = std::env::temp_dir().join(format!(
                "illustash-curation-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&root).unwrap();
            let db = root.join("library.sqlite");
            let source = root.join("source.sqlite");
            let c = crate::sqlite::open(&db).unwrap();
            c.execute_batch("CREATE TABLE assets(uuid TEXT PRIMARY KEY,added_at TEXT,width INTEGER,height INTEGER); INSERT INTO assets VALUES ('a','2026-01',10,10),('b','2026-02',20,20),('c','2026-02',20,20),('d','2025-01',NULL,NULL); CREATE TABLE duplicates(uuid TEXT PRIMARY KEY,grp TEXT NOT NULL,size INTEGER NOT NULL,pos INTEGER NOT NULL,grp_added TEXT NOT NULL,rep INTEGER NOT NULL,hidden INTEGER NOT NULL DEFAULT 0); CREATE INDEX duplicates_grp ON duplicates(grp,pos); CREATE TABLE curation(uuid TEXT PRIMARY KEY,status TEXT NOT NULL,origin TEXT NOT NULL,created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP); CREATE TABLE excluded(uuid TEXT PRIMARY KEY,reason TEXT,origin TEXT); CREATE TABLE auto_rules(uuid TEXT PRIMARY KEY,kind TEXT,reason TEXT); CREATE TABLE actions(id INTEGER PRIMARY KEY AUTOINCREMENT,label TEXT,before_json TEXT,ts TEXT DEFAULT CURRENT_TIMESTAMP,undone INTEGER DEFAULT 0); CREATE TABLE prefs(key TEXT PRIMARY KEY,value TEXT NOT NULL); CREATE TABLE relations(a TEXT,b TEXT,kind TEXT); CREATE TABLE memberships(uuid TEXT,entity TEXT);").unwrap();
            let src = crate::sqlite::open(&source).unwrap();
            if hash {
                src.execute_batch("CREATE TABLE assets(uuid TEXT PRIMARY KEY,stable_hash TEXT); INSERT INTO assets VALUES ('c','z'),('b','z'),('a','a'),('d',NULL),('not-in-library','z');").unwrap();
            } else {
                src.execute_batch(
                    "CREATE TABLE assets(uuid TEXT PRIMARY KEY); INSERT INTO assets VALUES ('a'),('b'),('c'),('d');",
                )
                .unwrap();
            }
            let context = LibraryContext {
                db,
                generation: "synthetic".into(),
                library: Some(root.clone()),
                tags: root.join("unused.csv"),
                source: Some(source),
                token: Some("synthetic".into()),
                read_only: None,
            };
            Self { root, c, context }
        }
        fn body(s: &str) -> Json {
            crate::http_json::parse_request(s).unwrap()
        }
        fn count(&self, table: &str) -> i64 {
            self.c.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0)).unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
    #[test]
    fn curate_history_is_ascii_ordered_and_rules_have_human_precedence() {
        let mut f = Fixture::new(false);
        f.c.execute_batch("INSERT INTO auto_rules VALUES ('a','exclude','photo'),('b','exclude','text'); INSERT INTO curation VALUES ('a','excluded','人🙂','old'); INSERT INTO duplicates VALUES ('b','pair',2,1,'2026-02',1,0),('c','pair',2,2,'2026-02',0,1);").unwrap();
        let out = curate(&mut f.c, &f.context, &Fixture::body(r#"{"ids":["b","a"],"action":"include"}"#)).unwrap();
        assert_eq!(out.get("undo").unwrap().get("label").unwrap().text(), "已收录 2 张");
        let before: String = f.c.query_row("SELECT before_json FROM actions", [], |r| r.get(0)).unwrap();
        assert_eq!(
            before,
            r#"[{"uuid": "b", "curation": null}, {"uuid": "a", "curation": {"status": "excluded", "origin": "\u4eba\ud83d\ude42", "created_at": "old"}}, {"uuid": "c", "curation": null}]"#
        );
        assert_eq!(f.count("excluded"), 0);
        curate(&mut f.c, &f.context, &Fixture::body(r#"{"ids":["b","a"],"action":"reset"}"#)).unwrap();
        let rows: Vec<(i64, String, String, String)> =
            f.c.prepare("SELECT rowid,uuid,reason,origin FROM excluded ORDER BY rowid")
                .unwrap()
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap();
        assert_eq!(
            rows,
            vec![(1, "a".into(), "photo".into(), "rule".into()), (2, "b".into(), "text".into(), "rule".into())]
        );
        assert_eq!(f.count("curation"), 0);
    }
    #[test]
    fn invalid_or_failed_curation_never_partially_commits() {
        let mut f = Fixture::new(false);
        assert!(
            matches!(curate(&mut f.c,&f.context,&Fixture::body(r#"{"action":true,"exclude":false}"#)),Err(Error::Invalid(m)) if m=="未知操作")
        );
        f.c.execute_batch("CREATE TRIGGER fail_history BEFORE INSERT ON actions BEGIN SELECT RAISE(ABORT,'synthetic history failure'); END;").unwrap();
        assert!(matches!(
            curate(&mut f.c, &f.context, &Fixture::body(r#"{"ids":["b","a"],"action":"exclude"}"#)),
            Err(Error::Sql(_))
        ));
        for table in ["curation", "excluded", "actions"] {
            assert_eq!(f.count(table), 0);
        }
    }
    #[test]
    fn fold_hashes_relations_representatives_mixed_visibility_and_rowids() {
        let mut f = Fixture::new(true);
        f.c.execute_batch("INSERT INTO relations VALUES ('a','b','duplicate'),('d','not-in-library','duplicate'); INSERT INTO memberships VALUES ('a','x'),('b','x'),('c','y');").unwrap();
        let bytes = fs::read(f.context.source.as_ref().unwrap()).unwrap();
        let out = fold(&mut f.c, &f.context, &Fixture::body(r#"{"kinds":["duplicate","duplicate"]}"#)).unwrap();
        assert_eq!(out.dump(), r#"{"kinds": ["duplicate"]}"#);
        let rows: Vec<(i64, String, String, i64, i64, i64)> =
            f.c.prepare("SELECT rowid,uuid,grp,pos,rep,hidden FROM duplicates ORDER BY rowid")
                .unwrap()
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap();
        assert_eq!(
            rows,
            vec![
                (1, "a".into(), "a".into(), 1, 0, 0),
                (2, "b".into(), "a".into(), 2, 1, 0),
                (3, "c".into(), "a".into(), 3, 0, 0)
            ]
        );
        assert_eq!(bytes, fs::read(f.context.source.as_ref().unwrap()).unwrap());
        fold(&mut f.c, &f.context, &Fixture::body(r#"{"kinds":[]}"#)).unwrap();
        assert_eq!(f.count("duplicates"), 2); // byte-identical copies still fold, but differing memberships stay visible.
        f.c.execute("DELETE FROM memberships", []).unwrap();
        fold(&mut f.c, &f.context, &Fixture::body(r#"{"kinds":[]}"#)).unwrap();
        assert_eq!(
            f.c.query_row("SELECT uuid FROM duplicates WHERE hidden=1", [], |r| r.get::<_, String>(0)).unwrap(),
            "c"
        );
        assert_eq!(f.count("actions"), 0);
    }
    #[test]
    fn no_hash_relation_groups_preserve_python_rhs_insertion_order() {
        let mut f = Fixture::new(false);
        f.c.execute_batch("INSERT INTO relations VALUES ('a','b','crop'),('c','d','crop');").unwrap();
        fold(&mut f.c, &f.context, &Fixture::body(r#"{"kinds":["crop"]}"#)).unwrap();
        let rows: Vec<(String, String)> =
            f.c.prepare("SELECT uuid,grp FROM duplicates ORDER BY rowid")
                .unwrap()
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap();
        assert_eq!(
            rows,
            vec![
                ("a".into(), "sim:a".into()),
                ("b".into(), "sim:a".into()),
                ("d".into(), "sim:c".into()),
                ("c".into(), "sim:c".into())
            ]
        );
        f.c.execute_batch("CREATE TRIGGER fail_duplicate BEFORE INSERT ON duplicates BEGIN SELECT RAISE(ABORT,'synthetic duplicate failure'); END;").unwrap();
        let before: String = f.c.query_row("SELECT value FROM prefs WHERE key='fold_kinds'", [], |r| r.get(0)).unwrap();
        assert!(matches!(
            fold(&mut f.c, &f.context, &Fixture::body(r#"{"kinds":["crop","edit"]}"#)),
            Err(Error::Sql(_))
        ));
        assert_eq!(f.count("duplicates"), 4);
        let current: String =
            f.c.query_row("SELECT value FROM prefs WHERE key='fold_kinds'", [], |r| r.get(0)).unwrap();
        assert_eq!(current, before);
        assert_eq!(before, r#"["crop"]"#);
    }
    #[test]
    fn bad_kinds_and_missing_source_do_not_write() {
        let mut f = Fixture::new(false);
        for text in [r#"{"kinds":null}"#, r#"{"kinds":[true]}"#, r#"{"kinds":["unknown"]}"#] {
            assert!(matches!(fold(&mut f.c,&f.context,&Fixture::body(text)),Err(Error::Invalid(m)) if m=="未知关系"));
        }
        fs::remove_file(f.context.source.as_ref().unwrap()).unwrap();
        assert!(matches!(fold(&mut f.c, &f.context, &Fixture::body(r#"{"kinds":[]}"#)), Err(Error::Sql(_))));
        assert_eq!(f.count("prefs"), 0);
    }
}
