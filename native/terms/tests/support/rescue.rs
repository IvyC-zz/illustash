//! Rescuing source.sqlite (#228) on synthetic packages: synthetic pictures, rows and deliberately overwritten pages.
use super::*;
use std::collections::BTreeMap;

struct Fixture {
    root: PathBuf,
    folder: PathBuf,
    package: PathBuf,
}
impl Fixture {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("illustash-rescue-{name}-{}", crate::store::new_key().unwrap()));
        let folder = root.join("pictures");
        fs::create_dir_all(&folder).unwrap();
        let package = root.join("synthetic.pilib");
        crate::app_create::create_folder(&package, &folder, Some("合成图库"), Some("other"), None).unwrap();
        Self { root, folder, package }
    }
    fn library(&self) -> Library {
        Library::locate(&self.package).unwrap()
    }
    fn source(&self) -> PathBuf {
        self.package.join("source.sqlite")
    }
    fn picture(&self, rel: &str, shade: u8) {
        let path = self.folder.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        image::RgbImage::from_pixel(4, 3, image::Rgb([shade, 7, 9])).save(path).unwrap();
    }
    fn scan(&self) {
        let source = self.library().config()["sources"][0].clone();
        crate::app_folder::scan(&source, &self.source(), &|| Ok(()), &|_, _| {}).unwrap();
    }
    /// What a sync's refresh records in library.sqlite: the pictures and their content.
    fn remember(&self) {
        let src = crate::sqlite::open(self.source()).unwrap();
        let rows = src
            .prepare("SELECT uuid,filename,src_path,width,height,added_at,src_kind,stable_hash FROM assets WHERE coalesce(trashed,0)=0")
            .unwrap()
            .query_map([], |r| (0..8).map(|i| r.get::<_, Sql>(i)).collect::<rusqlite::Result<Vec<_>>>())
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        let lib = crate::sqlite::open(self.package.join("library.sqlite")).unwrap();
        for row in rows {
            lib.execute("INSERT OR REPLACE INTO assets VALUES (?,?,?,?,?,?,?)", rusqlite::params_from_iter(&row[..7]))
                .unwrap();
            let Sql::Text(uuid) = &row[0] else { panic!() };
            lib.execute(
                "INSERT OR REPLACE INTO meta VALUES (?,?)",
                rusqlite::params![format!("folder_hash:{uuid}"), row[7]],
            )
            .unwrap();
        }
    }
    /// Relative path → ID, as `files` in `db` has it.
    fn ids(db: &Path) -> BTreeMap<String, String> {
        let c = crate::sqlite::open(db).unwrap();
        let ids = c
            .prepare("SELECT path,asset FROM files")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        ids
    }
    fn count(db: &Path, sql: &str) -> i64 {
        crate::sqlite::open(db).unwrap().query_row(sql, [], |r| r.get(0)).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// Overwrites page `page` (1-based) of `db` with garbage.
fn corrupt(db: &Path, page: u64) {
    let size: u64 = crate::sqlite::open(db).unwrap().query_row("PRAGMA page_size", [], |r| r.get(0)).unwrap();
    let mut bytes = fs::read(db).unwrap();
    let start = ((page - 1) * size) as usize;
    for (i, byte) in bytes[start..start + size as usize].iter_mut().enumerate() {
        *byte = (i as u8).wrapping_mul(37) ^ 0xA5;
    }
    fs::write(db, bytes).unwrap();
}
fn sound(db: &Path) -> bool {
    let c = crate::app_library::read_connection(db).unwrap();
    c.query_row("PRAGMA quick_check", [], |r| r.get::<_, String>(0)).is_ok_and(|v| v == "ok")
}
fn rebuild_quietly(library: &Library) -> Result<Rebuilt> {
    rebuild(library, None, &AtomicBool::new(false), &|_, _| {})
}

#[test]
fn pictures_get_their_ids_back_by_path_and_content_and_stale_results_go() {
    let f = Fixture::new("ids");
    for (rel, shade) in [("a.png", 10), ("b.png", 20), ("sub/c.png", 30), ("d.png", 40), ("e.png", 10)] {
        f.picture(rel, shade);
    }
    f.scan();
    f.remember();
    let before = Fixture::ids(&f.source());
    assert_eq!(before.len(), 5);
    let src = crate::sqlite::open(f.source()).unwrap();
    src.execute_batch(
        "CREATE TABLE recognition_assets(asset TEXT PRIMARY KEY, digest TEXT NOT NULL, package TEXT NOT NULL);
         CREATE TABLE recognition_imports(digest TEXT PRIMARY KEY, manifest TEXT NOT NULL, assets TEXT NOT NULL,
             status TEXT NOT NULL, error TEXT NOT NULL DEFAULT '', imported_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
         INSERT INTO labels(uuid,dim,value,conf,source) SELECT uuid,'rating','general',0.9,'wd_v3' FROM assets;
         INSERT INTO embeddings SELECT uuid,'wd',x'00' FROM assets;
         INSERT INTO recognition_assets SELECT uuid,'synthetic','synthetic' FROM assets;",
    )
    .unwrap();
    // One package per picture, as if each had been recognized on its own.
    for uuid in before.values() {
        src.execute(
            "INSERT INTO recognition_imports(digest,manifest,assets,status) VALUES (?,'{}',?,'done')",
            [format!("package-{uuid}"), format!("[\"{uuid}\"]")],
        )
        .unwrap();
    }
    // What damage took: the folder bookkeeping, two picture rows, and d's results (its ledger entry stayed).
    let id = |rel: &str| before[rel].clone();
    src.execute_batch("DELETE FROM files; DELETE FROM asset_links;").unwrap();
    for rel in ["a.png", "sub/c.png"] {
        src.execute("DELETE FROM assets WHERE uuid=?", [id(rel)]).unwrap();
    }
    src.execute("DELETE FROM labels WHERE uuid=?", [id("d.png")]).unwrap();
    drop(src);
    // Meanwhile c moved and b got new content.
    fs::create_dir_all(f.folder.join("moved")).unwrap();
    fs::rename(f.folder.join("sub/c.png"), f.folder.join("moved/c2.png")).unwrap();
    f.picture("b.png", 21);

    let library = f.library();
    let rebuilt = rebuild_quietly(&library).unwrap();
    let building = f.package.join(BUILDING);
    let after = Fixture::ids(&building);
    let expected =
        [("a.png", "a.png"), ("b.png", "b.png"), ("moved/c2.png", "sub/c.png"), ("d.png", "d.png"), ("e.png", "e.png")]
            .into_iter()
            .map(|(now, was)| (now.to_owned(), id(was)))
            .collect::<BTreeMap<_, _>>();
    assert_eq!(after, expected);
    assert_eq!((rebuilt.report.kept, rebuilt.report.new), (5, 0), "{:?}", rebuilt.report);
    // b's results described its old content and d's were lost: those two wait for recognition, without ledger entries.
    assert_eq!(rebuilt.report.pending, 2, "{:?}", rebuilt.report);
    for (rel, results) in [("a.png", 1), ("b.png", 0), ("moved/c2.png", 1), ("d.png", 0), ("e.png", 1)] {
        let uuid = &expected[rel];
        let rating = format!("SELECT count(*) FROM labels WHERE uuid='{uuid}' AND dim='rating'");
        assert_eq!(Fixture::count(&building, &rating), results, "{rel}");
        let ledger = format!("SELECT count(*) FROM recognition_assets WHERE asset='{uuid}'");
        assert_eq!(Fixture::count(&building, &ledger), results, "{rel}");
        let package = format!("SELECT count(*) FROM recognition_imports WHERE digest='package-{uuid}'");
        assert_eq!(Fixture::count(&building, &package), results, "{rel}");
    }
    assert_eq!(Fixture::count(&building, "SELECT count(*) FROM assets WHERE coalesce(trashed,0)=0"), 5);
    // The damaged file stays as it was until the new one is put in place.
    assert_eq!(Fixture::count(&f.source(), "SELECT count(*) FROM files"), 0);

    let health = Health::load(&f.package);
    let report = install(&library, &health, rebuilt).unwrap();
    assert!(!building.exists());
    assert_eq!(Fixture::ids(&f.source()), expected);
    let copy = report.damaged_copy.unwrap();
    assert!(copy.starts_with(library.backups()) && copy.is_file(), "{copy:?}");
    assert_eq!(Fixture::count(&copy, "SELECT count(*) FROM files"), 0);
    assert!(health.checked().contains_key(&crate::app_integrity::Database::Source));
    // The scan's notes for the next refresh of library.sqlite follow the new file.
    assert!(!f.package.join(BUILDING_JOURNAL).exists());
}

#[test]
fn reading_resumes_past_a_damaged_page_and_indices_are_made_again() {
    for (case, index_only) in [("table", false), ("index", true), ("root", false)] {
        let f = Fixture::new(case);
        let pages = {
            let c = crate::sqlite::open(f.source()).unwrap();
            c.execute_batch(
                "CREATE TABLE synthetic_rows(id INTEGER PRIMARY KEY, body TEXT);
                 CREATE TABLE synthetic_plain(body TEXT);
                 WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<6000)
                 INSERT INTO synthetic_rows SELECT i, printf('%0300d', i) FROM n;
                 INSERT INTO synthetic_plain VALUES ('synthetic');",
            )
            .unwrap();
            let pages: u64 = c.query_row("PRAGMA page_count", [], |r| r.get(0)).unwrap();
            c.execute_batch("CREATE INDEX synthetic_rows_body ON synthetic_rows(body)").unwrap();
            pages
        };
        let total: u64 =
            crate::sqlite::open(f.source()).unwrap().query_row("PRAGMA page_count", [], |r| r.get(0)).unwrap();
        let root: u64 = crate::sqlite::open(f.source())
            .unwrap()
            .query_row("SELECT rootpage FROM sqlite_master WHERE name='synthetic_rows'", [], |r| r.get(0))
            .unwrap();
        corrupt(
            &f.source(),
            if case == "root" {
                root
            } else if index_only {
                total - 1
            } else {
                pages / 2
            },
        );
        assert!(!sound(&f.source()));
        let rebuilt = rebuild_quietly(&f.library()).unwrap();
        let building = f.package.join(BUILDING);
        assert!(sound(&building));
        let rows = Fixture::count(&building, "SELECT count(*) FROM synthetic_rows");
        if index_only {
            assert!(rebuilt.report.damaged.is_empty(), "{:?}", rebuilt.report);
            assert_eq!(rows, 6000);
        } else if case == "root" {
            // No seek gets past the table's root: its rows are lost, the rest of the file is not.
            assert_eq!(rebuilt.report.damaged, vec!["synthetic_rows".to_owned()]);
            assert_eq!(rows, 0);
        } else {
            assert_eq!(rebuilt.report.damaged, vec!["synthetic_rows".to_owned()]);
            // One leaf of some twenty rows is lost; reading goes on to the end.
            assert!((5000..6000).contains(&rows), "{rows}");
            assert_eq!(Fixture::count(&building, "SELECT max(id) FROM synthetic_rows"), 6000);
        }
        assert_eq!(
            Fixture::count(&building, "SELECT count(*) FROM synthetic_rows WHERE body<>printf('%0300d', id)"),
            0
        );
        assert_eq!(Fixture::count(&building, "SELECT count(*) FROM synthetic_plain"), 1);
        assert_eq!(Fixture::count(&building, "SELECT count(*) FROM sqlite_master WHERE name='synthetic_rows_body'"), 1);
    }
}

#[test]
fn a_lost_table_list_starts_from_an_empty_source_and_ids_still_come_back() {
    let f = Fixture::new("schema");
    for (rel, shade) in [("a.png", 10), ("b.png", 20)] {
        f.picture(rel, shade);
    }
    f.scan();
    f.remember();
    let before = Fixture::ids(&f.source());
    corrupt(&f.source(), 1);
    let rebuilt = rebuild_quietly(&f.library()).unwrap();
    assert!(rebuilt.report.schema_lost);
    assert_eq!(Fixture::ids(&f.package.join(BUILDING)), before);
    assert_eq!((rebuilt.report.kept, rebuilt.report.pending), (2, 2));
}

#[test]
fn an_unreadable_source_folder_stops_the_rescue_before_anything_is_written() {
    let f = Fixture::new("offline");
    f.picture("a.png", 10);
    f.scan();
    let before = fs::read(f.source()).unwrap();
    fs::rename(&f.folder, f.root.join("elsewhere")).unwrap();
    let error = rebuild_quietly(&f.library()).unwrap_err();
    assert!(error.to_string().contains("来源文件夹现在读不到"), "{error}");
    assert!(!f.package.join(BUILDING).exists());
    assert_eq!(fs::read(f.source()).unwrap(), before);
}
