//! The compatibility facade preserves requested path text while adapting SQLite operands.
use illustash_terms::{legacy_artists, Error};
use rusqlite::Connection;
use std::path::Path;

#[test]
fn missing_relative_provenance_path_keeps_original_error_text() {
    let mut c = Connection::open_in_memory().unwrap();
    illustash_terms::configure(&c).unwrap();
    illustash_terms::migrate(&mut c).unwrap();
    let path = format!("synthetic-missing-{}.sqlite", illustash_terms::store::new_key().unwrap());
    let before: i64 = c.query_row("SELECT COUNT(*) FROM sqlite_master", [], |r| r.get(0)).unwrap();
    let attached_before: i64 = c.query_row("SELECT COUNT(*) FROM pragma_database_list", [], |r| r.get(0)).unwrap();
    assert!(
        matches!(legacy_artists::sync(&mut c,Path::new(&path)),Err(Error::Invalid(message)) if message==format!("找不到 {path}"))
    );
    assert_eq!(c.query_row("SELECT COUNT(*) FROM sqlite_master", [], |r| r.get::<_, i64>(0)).unwrap(), before);
    assert_eq!(
        c.query_row("SELECT COUNT(*) FROM pragma_database_list", [], |r| r.get::<_, i64>(0)).unwrap(),
        attached_before
    );
}
