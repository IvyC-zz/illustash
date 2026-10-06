use super::*;
#[test]
fn registry_defaults_and_explicit_photos_mapping_use_synthetic_database() {
    let root = std::env::temp_dir().join(format!("illustash-origin-photos-{}", crate::store::new_key().unwrap()));
    let photos = root.join("synthetic.photoslibrary");
    std::fs::create_dir_all(photos.join("database")).unwrap();
    let c = crate::sqlite::open(photos.join("database/Photos.sqlite")).unwrap();
    c.execute_batch("CREATE TABLE ZASSET(ZUUID TEXT,Z_PK INTEGER,ZWIDTH INTEGER,ZHEIGHT INTEGER,ZADDEDDATE REAL,ZHIDDEN INTEGER,ZDIRECTORY TEXT,ZFILENAME TEXT,ZADDITIONALATTRIBUTES INTEGER,ZKIND INTEGER,ZTRASHEDSTATE INTEGER); CREATE TABLE ZADDITIONALASSETATTRIBUTES(Z_PK INTEGER,ZIMPORTEDBYBUNDLEIDENTIFIER TEXT,ZORIGINALSTABLEHASH TEXT,ZORIGINALFILESIZE INTEGER);").unwrap();
    for (id, app) in [(1, "com.atebits.Tweetie2"), (2, "net.pxv.iphone"), (3, "synthetic.unknown")] {
        c.execute(
            "INSERT INTO ZASSET VALUES (?, ?, 1,1,0,0,'A','synthetic.jpg',?,0,0)",
            params![format!("SYNTHETIC-{id}"), id, id],
        )
        .unwrap();
        c.execute("INSERT INTO ZADDITIONALASSETATTRIBUTES VALUES (?,?,'synthetic-hash',1)", params![id, app]).unwrap();
    }
    drop(c);
    let before = std::fs::read(photos.join("database/Photos.sqlite")).unwrap();
    for (name, apps, expected) in [
        ("default", None, vec![("SYNTHETIC-1", "x"), ("SYNTHETIC-2", "pixiv")]),
        (
            "explicit",
            Some(json!({"com.atebits.Tweetie2":"other","synthetic.unknown":"other"})),
            vec![("SYNTHETIC-1", "other"), ("SYNTHETIC-3", "other")],
        ),
    ] {
        let package = root.join(format!("{name}.pilib"));
        crate::app_create::create_photos(&package, &photos, Some("synthetic"), None).unwrap();
        let mut source = json!({"id":"s1","kind":"photos","path":photos});
        if let Some(apps) = apps {
            source["apps"] = apps;
        }
        let cancelled = std::cell::Cell::new(false);
        let result = scan(
            &source,
            &package.join("source.sqlite"),
            &|| {
                if cancelled.get() {
                    Err(Error::Conflict("扫描已中断".into()))
                } else {
                    Ok(())
                }
            },
            &|_, _| cancelled.set(true),
        );
        assert!(matches!(result, Err(Error::Conflict(_))));
        let untouched = crate::sqlite::open(package.join("source.sqlite")).unwrap();
        assert_eq!(untouched.query_row("SELECT count(*) FROM assets", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        drop(untouched);
        let counts = scan(&source, &package.join("source.sqlite"), &|| Ok(()), &|_, _| {}).unwrap();
        assert_eq!(counts["new"], 2);
        let c = crate::app_library::read_connection(&package.join("source.sqlite")).unwrap();
        let actual = c
            .prepare("SELECT uuid,preset FROM assets ORDER BY uuid")
            .unwrap()
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(actual, expected.into_iter().map(|(a, p)| (a.to_owned(), p.to_owned())).collect::<Vec<_>>());
    }
    assert_eq!(std::fs::read(photos.join("database/Photos.sqlite")).unwrap(), before);
    std::fs::remove_dir_all(root).unwrap();
}
