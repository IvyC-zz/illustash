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
        source(&db, &config).unwrap();
        let c = crate::app_library::read_connection(&db).unwrap();
        let rows = c
            .prepare("SELECT source_id,preset FROM assets ORDER BY uuid")
            .unwrap()
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        // 原迁移（旧 Python ensure_source_schema/link_photos，#9 起不再并跑）的完整输出：
        // 仅缺少来源/预设的行回填，完整行保持不变；无元数据保留已有值。
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
