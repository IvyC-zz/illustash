use super::*;
#[test]
fn upstream_posts_keep_user_origin_and_statistics_scope() {
    let root = std::env::temp_dir().join(format!("illustash-origin-{}", crate::store::new_key().unwrap()));
    std::fs::create_dir_all(root.join("images")).unwrap();
    let package = root.join("synthetic.pilib");
    crate::app_create::create_folder(&package, &root.join("images"), Some("synthetic"), Some("other"), None).unwrap();
    let lib = Library::open(&package).unwrap();
    let source = crate::sqlite::open(lib.source()).unwrap();
    for (asset, preset, trashed, scope) in [
        ("a", "other", 0, 1),
        ("b", "pixiv", 0, 1),
        ("c", "x", 0, 1),
        ("d", "unknown", 0, 1),
        ("e", "x", 1, 1),
        ("f", "x", 0, 0),
    ] {
        source
            .execute(
                "INSERT INTO assets(uuid,filename,preset,trashed,in_scope) VALUES (?,'synthetic.png',?,?,?)",
                params![asset, preset, trashed, scope],
            )
            .unwrap();
    }
    drop(source);
    let before = std::fs::read(lib.source()).unwrap();
    let cache_path = root.join("provenance.sqlite");
    let c = cache::connect(&cache_path).unwrap();
    post(
        &c,
        "a",
        &json!({"id":1,"tag_string_artist":"synthetic","source":"https://www.pixiv.net/artworks/123"}),
        "pixel_hash",
        1.0,
    )
    .unwrap();
    post(&c, "c", &json!({"id":2,"tag_string_artist":"","source":"https://x.com/Synthetic/status/456"}), "iqdb", 95.0)
        .unwrap();
    assert_eq!(
        c.query_row("SELECT post_id FROM image_sources WHERE asset='a' AND site='pixiv'", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        "123"
    );
    assert_eq!(
        c.query_row("SELECT key FROM artist_links WHERE system='x'", [], |r| r.get::<_, String>(0)).unwrap(),
        "synthetic"
    );
    drop(c);
    let result = report(&lib, &cache_path).unwrap();
    assert_eq!(result["numbers"]["pictures"], json!({"other":1,"pixiv":1,"x":1}));
    assert_eq!(result["numbers"]["with_original_post"], 2);
    assert_eq!(std::fs::read(lib.source()).unwrap(), before);
    assert_eq!(
        crate::app_library::read_connection(&lib.source())
            .unwrap()
            .query_row("SELECT preset FROM assets WHERE uuid='a'", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "other"
    );
    std::fs::remove_dir_all(root).unwrap();
}
