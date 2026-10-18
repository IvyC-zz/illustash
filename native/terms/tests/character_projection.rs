//! 角色投影的增量重算（只算受影响的图）与全量重算逐行一致（#200）。数据全部合成。
use illustash_terms::character_projection::rebuild;
use rusqlite::{params, types::ValueRef, Connection};

const SCHEMA: &str = include_str!("../src/app_create_schema.sql");
const HYBRID: &str = include_str!("../src/app_migration_sql/hybrid.sql");
const ENTITIES: [&str; 7] = ["alice", "alice_summer", "bob", "bob_winter", "carol", "mascot", "dave"];
const GENERAL: [&str; 8] = ["solo", "1girl", "1boy", "multiple_girls", "2girls", "6+boys", "smile", "solo_focus"];
const SCORES: [f64; 9] = [0.4, 0.6, 0.8, 0.85, 0.9, 0.92, 0.925, 0.95, 0.97];
const ASSETS: usize = 48;
const DERIVED: [&str; 7] = [
    "family_evidence",
    "single_assets",
    "lead_assets",
    "memberships",
    "tag_memberships",
    "duplicates",
    // 镜像存的是 term_assets 的自增 id，登记先后不同 id 就不同；按图片和词条的键比。
    "(SELECT a.key,t.key,p.state,p.score,p.sources FROM term_projection p
      JOIN term_assets a ON a.id=p.asset_id JOIN terms t ON t.id=p.term_id)",
];

/// 固定种子的线性同余数列：同一个种子在两个库里给出同样的合成数据。
struct Seq(u64);
impl Seq {
    fn next(&mut self, n: usize) -> usize {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.0 >> 33) % n as u64) as usize
    }
}

fn uuid(i: usize) -> String {
    format!("00000000-0000-4000-8000-{i:012}")
}

fn library(hybrid: bool) -> Connection {
    let mut c = Connection::open_in_memory().unwrap();
    illustash_terms::configure(&c).unwrap();
    c.execute_batch(SCHEMA).unwrap();
    c.execute_batch(
        "INSERT INTO entities(id,name,work,kind) VALUES ('alice','','Example','character'),('bob','','Example','character'),
           ('carol','','Other','character'),('dave','','Other','character');
         INSERT INTO entities(id,name,kind,parent_id,variant) VALUES ('alice_summer','','skin','alice','alice_(summer)'),
           ('bob_winter','','skin','bob','bob_(winter)');
         INSERT INTO entities(id,name,kind,mascot,mascot_origin) VALUES ('mascot','','character',1,'user');",
    )
    .unwrap();
    if hybrid {
        c.execute_batch("INSERT INTO meta VALUES ('identity_policy','hybrid');").unwrap();
        c.execute_batch(HYBRID).unwrap();
    }
    for i in 0..ASSETS {
        c.execute("INSERT INTO assets(uuid,src_path) VALUES (?,'')", [uuid(i)]).unwrap();
    }
    // 重复组：每 4 张一组，组里第一张是代表；有一组跨过增量范围的边界。
    for i in 0..ASSETS {
        let (grp, pos) = (i / 4, i % 4);
        c.execute(
            "INSERT INTO duplicates(uuid,grp,size,pos,grp_added,rep) VALUES (?,?,4,?,'2026-10-01',?)",
            params![uuid(i), format!("g{grp:02}"), pos, i64::from(pos == 0)],
        )
        .unwrap();
    }
    for i in 0..ASSETS {
        inputs(&c, i, &mut Seq(i as u64 * 7919 + 1));
    }
    illustash_terms::migrate(&mut c).unwrap();
    illustash_terms::legacy_characters::sync(&mut c).unwrap();
    c
}

/// 一张图的识别输入：候选、原型认领、通用标签、人工结论。
fn inputs(c: &Connection, i: usize, seq: &mut Seq) {
    let u = uuid(i);
    for table in
        ["candidates", "proto_claims", "general_hits", "assertions", "identity_suspended", "identity_uncertain"]
    {
        c.execute(&format!("DELETE FROM {table} WHERE uuid=?"), [&u]).unwrap();
    }
    for _ in 0..seq.next(4) {
        let entity = ENTITIES[seq.next(ENTITIES.len())];
        let source = ["WD", "CCIP"][seq.next(2)];
        let score = SCORES[seq.next(SCORES.len())];
        c.execute("INSERT OR IGNORE INTO candidates VALUES (?,?,?,?)", params![u, entity, source, score]).unwrap();
    }
    if seq.next(4) == 0 {
        let entity = ENTITIES[seq.next(ENTITIES.len())];
        c.execute(
            "INSERT OR IGNORE INTO proto_claims VALUES (?,?,?)",
            params![u, entity, SCORES[seq.next(SCORES.len())]],
        )
        .unwrap();
    }
    for value in GENERAL {
        if seq.next(3) == 0 {
            c.execute("INSERT INTO general_hits VALUES (?,?)", params![u, value]).unwrap();
        }
    }
    if seq.next(3) == 0 {
        let entity = ENTITIES[seq.next(ENTITIES.len())];
        let status = ["confirmed", "rejected"][seq.next(2)];
        let origin = ["review", "album", "legacy_import"][seq.next(3)];
        c.execute("INSERT INTO assertions VALUES (?,?,?,?)", params![u, entity, status, origin]).unwrap();
    }
    if seq.next(10) == 0 {
        c.execute("INSERT INTO identity_suspended VALUES (?,?,'synthetic','[]')", params![u, "alice"]).unwrap();
    }
}

fn rows(c: &Connection, table: &str) -> Vec<String> {
    let mut stmt = c.prepare(&format!("SELECT * FROM {table}")).unwrap();
    let width = stmt.column_count();
    let mut out: Vec<String> = stmt
        .query_map([], |r| {
            Ok((0..width)
                .map(|k| match r.get_ref(k).unwrap() {
                    ValueRef::Text(t) => format!("'{}'", String::from_utf8_lossy(t)),
                    other => format!("{other:?}"),
                })
                .collect::<Vec<_>>()
                .join("|"))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    out.sort();
    out
}

fn compare(hybrid: bool, touched: &[usize]) {
    let changed = |c: &Connection| {
        for &i in touched {
            inputs(c, i, &mut Seq(i as u64 * 104729 + 17));
        }
    };
    // 增量：先全量算好，改动受影响的图的输入，只重算这些图。
    let incremental = library(hybrid);
    rebuild(&incremental, None).unwrap();
    let before: Vec<_> = DERIVED.iter().map(|t| rows(&incremental, t)).collect();
    changed(&incremental);
    let ids: Vec<String> = touched.iter().map(|&i| uuid(i)).collect();
    rebuild(&incremental, Some(&ids)).unwrap();
    // 全量：同样的最终输入，从头重算。
    let full = library(hybrid);
    changed(&full);
    rebuild(&full, None).unwrap();
    let mut moved = 0;
    for (k, table) in DERIVED.iter().enumerate() {
        let (a, b) = (rows(&incremental, table), rows(&full, table));
        assert_eq!(a, b, "hybrid={hybrid} touched={} {table}", touched.len());
        moved += usize::from(a != before[k]);
    }
    // 合成改动确实让派生表变了，比较不是在比两份没动过的表。
    assert!(moved >= 4, "hybrid={hybrid} touched={} 只变了 {moved} 张表", touched.len());
    assert!(!rows(&full, DERIVED[6]).is_empty());
}

#[test]
fn incremental_rebuild_matches_full_rebuild_row_by_row() {
    for hybrid in [false, true] {
        // ≤10 张逐图刷新归属；更多的走 touched 临时表。
        compare(hybrid, &[1, 2, 5, 9, 14, 30]);
        compare(hybrid, &(0..ASSETS).filter(|i| i % 3 != 1).collect::<Vec<_>>());
    }
}
