use super::*;
#[test]
fn legacy_photos_upgrade_preserves_missing_apps_fallback_and_explicit_mapping() {
    let root = std::env::temp_dir().join(format!("illustash-origin-migrate-{}", crate::store::new_key().unwrap()));
    let photos = root.join("synthetic.photoslibrary");
    std::fs::create_dir_all(photos.join("database")).unwrap();
    let photos_path = photos.join("database/Photos.sqlite");
    let c = crate::sqlite::open(&photos_path).unwrap();
    c.execute_batch("CREATE TABLE ZASSET(ZUUID TEXT,Z_PK INTEGER,ZADDITIONALATTRIBUTES INTEGER,ZKIND INTEGER,ZTRASHEDSTATE INTEGER); CREATE TABLE ZADDITIONALASSETATTRIBUTES(Z_PK INTEGER,ZIMPORTEDBYBUNDLEIDENTIFIER TEXT);").unwrap();
    for (id, app) in [(1, "com.atebits.Tweetie2"), (2, "net.pxv.iphone"), (3, "com.atebits.Tweetie2")] {
        c.execute("INSERT INTO ZASSET VALUES (?,?,?,0,0)", params![format!("SYNTHETIC-{id}"), id, id]).unwrap();
        c.execute("INSERT INTO ZADDITIONALASSETATTRIBUTES VALUES (?,?)", params![id, app]).unwrap();
    }
    drop(c);
    let before_photos = std::fs::read(&photos_path).unwrap();
    for (name, mapping, expected) in [
        ("absent", None, ["other", "other", "other", "custom"]),
        (
            "explicit",
            Some(json!({"com.atebits.Tweetie2":"x","net.pxv.iphone":"pixiv"})),
            ["x", "pixiv", "other", "custom"],
        ),
    ] {
        let db = root.join(format!("{name}.sqlite"));
        let c = crate::sqlite::open(&db).unwrap();
        c.execute_batch("CREATE TABLE assets(uuid TEXT PRIMARY KEY,source_id TEXT,preset TEXT); INSERT INTO assets VALUES ('SYNTHETIC-1',NULL,'other'),('SYNTHETIC-2',NULL,NULL),('SYNTHETIC-3','existing','other'),('SYNTHETIC-4',NULL,'custom');").unwrap();
        drop(c);
        let mut config = json!({"sources":[{"id":"s1","kind":"photos","path":photos,"preset_origin":"user"}]});
        if let Some(mapping) = mapping {
            config["sources"][0]["apps"] = mapping;
        }
        let before_config = config.clone();
        let reference = root.join(format!("{name}-reference.sqlite"));
        std::fs::copy(&db, &reference).unwrap();
        let python = std::env::var("ILLUSTASH_TEST_PYTHON").unwrap_or_else(|_| "python3".into());
        let legacy = std::process::Command::new(python)
            .current_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
            .env("PYTHONUTF8", "1")
            .args(["-c", r#"
import json, sqlite3, sys
from pathlib import Path
from web.library import ensure_source_schema, link_photos
source = json.loads(sys.argv[2])['sources'][0]
with sqlite3.connect((Path(source['path']) / 'database/Photos.sqlite').as_uri() + '?mode=ro', uri=True) as photos:
    app = {u.upper(): a for u, a in photos.execute('SELECT a.ZUUID,coalesce(d.ZIMPORTEDBYBUNDLEIDENTIFIER,\'\') FROM ZASSET a LEFT JOIN ZADDITIONALASSETATTRIBUTES d ON a.ZADDITIONALATTRIBUTES=d.Z_PK WHERE a.ZKIND=0 AND a.ZTRASHEDSTATE=0')}
with sqlite3.connect(sys.argv[1]) as c:
    ensure_source_schema(c)
    missing = [u for (u,) in c.execute('SELECT uuid FROM assets WHERE source_id IS NULL OR preset IS NULL')]
    link_photos(c, source, [(u, app.get(u)) for u in missing])
    print(json.dumps(c.execute('SELECT source_id,preset FROM assets ORDER BY uuid').fetchall()))
"#])
            .arg(&reference).arg(config.to_string()).output().unwrap();
        assert!(legacy.status.success(), "{}", String::from_utf8_lossy(&legacy.stderr));
        let legacy_rows: Value = serde_json::from_slice(&legacy.stdout).unwrap();
        source(&db, &config).unwrap();
        let c = crate::app_library::read_connection(&db).unwrap();
        let rows = c
            .prepare("SELECT source_id,preset FROM assets ORDER BY uuid")
            .unwrap()
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(serde_json::to_value(&rows).unwrap(), legacy_rows);
        // 原迁移的完整输出：仅缺少来源/预设的行回填，完整行保持不变；无元数据保留已有值。
        assert_eq!(
            rows,
            vec![
                ("s1".into(), expected[0].into()),
                ("s1".into(), expected[1].into()),
                ("existing".into(), expected[2].into()),
                ("s1".into(), expected[3].into())
            ]
        );
        assert_eq!(config, before_config);
    }
    assert_eq!(std::fs::read(photos_path).unwrap(), before_photos);
    std::fs::remove_dir_all(root).unwrap();
}
