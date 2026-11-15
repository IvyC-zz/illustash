//! 需要真实 SQLite 连接的历史投影检查；与配置首次 SQLite 调用的重放二进制隔离。
use crate::common::baseline::{historical_projection, state, Masks, State};
use rusqlite::Connection;
use serde_json::{json, Value};

#[test]
fn raw_attrs_are_visible_and_only_historical_projection_reconstructs_them() {
    let _env = crate::shared();
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch(
        "CREATE TABLE meta(key TEXT,value TEXT); INSERT INTO meta VALUES('terms_schema','9');
        CREATE TABLE term_records(id INTEGER PRIMARY KEY,key TEXT,attrs TEXT);
        INSERT INTO term_records VALUES(1,'synthetic','{\"raw\":\"before\"}');
        CREATE VIEW terms AS SELECT * FROM term_records;
        CREATE TABLE term_tree(term_id INTEGER,variant TEXT,artist TEXT,label_only INTEGER,
            mascot INTEGER,mascot_origin TEXT,original INTEGER);
        INSERT INTO term_tree VALUES(1,'variant','artist',0,0,NULL,1);",
    )
    .unwrap();
    let masks = Masks { roots: &str::to_owned, version: 9 };
    let before = state(&c, &masks);
    c.execute("UPDATE term_records SET attrs=?1", ["{\"raw\":\"after\"}"]).unwrap();
    let after = state(&c, &masks);
    assert_ne!(before["term_records"], after["term_records"]);
    assert_ne!(before["terms"], after["terms"]);
    let projected = historical_projection(&after, &State::new(), 7, 9).unwrap();
    let row: Vec<Value> = serde_json::from_str(projected["terms"].rows.keys().next().unwrap()).unwrap();
    assert_eq!(row[2], json!("{\"variant\":\"variant\",\"artist\":\"artist\",\"label_only\":0,\"mascot\":0,\"mascot_origin\":null,\"original\":1}"));
    assert_eq!(before["term_tree"], after["term_tree"]);
    c.execute_batch("DROP VIEW terms; CREATE VIEW terms AS SELECT id,key,'{}' AS attrs FROM term_records;").unwrap();
    assert!(historical_projection(&state(&c, &masks), &State::new(), 7, 9).is_err());
}
