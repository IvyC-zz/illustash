use super::*;

const OLD: &str = include_str!("old_schema.sql");
const ALICE: &str = "a11ce000-0000-4000-8000-000000000001";
const BOB: &str = "b0b00000-0000-4000-8000-000000000002";
const SKIN: &str = "5k1n0000-0000-4000-8000-000000000003";
const WORK: &str = "w0rk0000-0000-4000-8000-000000000004";

/// 一个小的旧库（hybrid）：作品、人物、皮肤，人工结论和候选。迁移时身份照旧表建成词条（核心里还没有身份）。旧的身份
/// 关系表是空的：核心里没有关系时，旧表里的关系会让升级拒绝（`retire_relations`）。
fn library() -> Connection {
    library_from(OLD)
}

fn library_from(old: &str) -> Connection {
    let mut c = Connection::open_in_memory().unwrap();
    crate::configure(&c).unwrap();
    c.execute_batch(old).unwrap();
    c.execute_batch(&format!(
        "INSERT INTO meta VALUES ('identity_policy','hybrid');
         INSERT INTO assets(uuid) VALUES ('a'),('b'),('c');
         INSERT INTO entities(id,name,work,kind,work_id) VALUES ('{WORK}','','','work',NULL),('{ALICE}','','Example','character','{WORK}'),
          ('{BOB}','','Example','character',NULL);
         INSERT INTO entities(id,name,kind,parent_id,variant,mascot,mascot_origin) VALUES ('{SKIN}','','skin','{ALICE}','alice_(summer)',1,'user');
         INSERT INTO actions(label,before_json) VALUES ('否定','[{{\"uuid\":\"a\",\"entity\":\"{ALICE}\",\"assertion\":{{\"status\":\"confirmed\",\"origin\":\"album\"}},\"deferred\":false}}]');
         INSERT INTO assertions VALUES ('a','{ALICE}','rejected','review'),('b','{BOB}','confirmed','legacy_import');
         INSERT INTO candidates VALUES ('a','{ALICE}','WD',0.9),('b','{ALICE}','CCIP',0.95),('b','{BOB}','WD',0.7);"
    ))
    .unwrap();
    crate::migrate(&mut c).unwrap();
    c
}

#[test]
fn shadow_tables_copy_the_old_shape_without_foreign_keys() {
    let sql = "CREATE TABLE links(system TEXT NOT NULL,key TEXT NOT NULL,entity_id TEXT NOT NULL REFERENCES entities(id),\n current INTEGER NOT NULL DEFAULT 1,PRIMARY KEY(system,key))";
    assert_eq!(
        shadow_ddl(sql, "links").unwrap(),
        "CREATE TABLE shadow_links(system TEXT NOT NULL,key TEXT NOT NULL,entity_id TEXT NOT NULL ,\n current INTEGER NOT NULL DEFAULT 1,PRIMARY KEY(system,key))"
    );
}

#[test]
fn a_library_whose_columns_came_in_another_order_upgrades_the_same() {
    const REAL: &str = "CREATE TABLE entities(id TEXT PRIMARY KEY,name TEXT NOT NULL,work TEXT NOT NULL DEFAULT '',external_key TEXT UNIQUE, variant TEXT NOT NULL DEFAULT '', parent_id TEXT REFERENCES entities(id), tag TEXT, merged_into TEXT REFERENCES entities(id), kind TEXT NOT NULL DEFAULT '', artist TEXT NOT NULL DEFAULT '', work_id TEXT REFERENCES entities(id), original INTEGER NOT NULL DEFAULT 0, label_only INTEGER NOT NULL DEFAULT 0, mascot INTEGER NOT NULL DEFAULT 0, mascot_origin TEXT);";
    let line = OLD.lines().find(|l| l.starts_with("CREATE TABLE entities(")).unwrap();
    let identities = |c: &Connection| -> Vec<String> {
        let mut st = c.prepare(&format!("SELECT * FROM {} ORDER BY seq", crate::character_entities::ENTITIES)).unwrap();
        let n = st.column_count();
        st.query_map([], |x| {
            let row = (0..n).map(|i| x.get::<_, rusqlite::types::Value>(i)).collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(format!("{row:?}"))
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
    };
    let c = library_from(&OLD.replace(line, REAL));
    assert_eq!(identities(&c), identities(&library()));
    assert_eq!(identities(&c).len(), 4);
}

#[test]
fn candidate_scope_anchor_appears_once_under_both_policies() {
    for select in [CANDIDATES_RESOLVED, compat_select("candidates").unwrap()] {
        let scoped = crate::character_projection::scoped(select, CANDIDATE_SCOPE.0, CANDIDATE_SCOPE.1).unwrap();
        assert_eq!(scoped.matches("FROM candidate_scope scoped").count(), 1);
    }
}
