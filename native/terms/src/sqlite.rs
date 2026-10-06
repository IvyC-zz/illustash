pub use illustash_platform::sqlite::*;

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::OpenFlags;
    use std::fs;

    #[test]
    fn long_path_create_attach_wal_backup_and_restore() {
        let root = std::env::temp_dir().join(format!("illustash-long-create-{}", crate::store::new_key().unwrap()));
        let mut parent = root.clone();
        while parent.as_os_str().len() < 200 {
            parent = parent.join("synthetic-segment");
        }
        fs::create_dir_all(&parent).unwrap();
        let reference = parent.join("参考 # %.sqlite");
        open(&reference).unwrap().execute_batch(include_str!("app_source_empty.sql")).unwrap();
        let dest = parent.join("synthetic.pilib");
        crate::app_create::create_folder(&dest, &parent, Some("synthetic"), Some("other"), Some(&reference)).unwrap();
        let lib = crate::app_library::Library::open(&dest).unwrap();
        let writer = open(lib.db()).unwrap();
        writer.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE long_path_probe(value TEXT); INSERT INTO long_path_probe VALUES ('committed-wal');").unwrap();
        let backup = crate::app_settings::backup(&lib).unwrap();
        #[cfg(windows)]
        assert!(backup.as_os_str().len() > 260);
        let restored = parent.join("restored.sqlite");
        fs::copy(&backup, &restored).unwrap();
        let c = open_with_flags(&restored, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        assert_eq!(
            c.query_row("SELECT value FROM long_path_probe", [], |r| r.get::<_, String>(0)).unwrap(),
            "committed-wal"
        );
        crate::app_library::check_database(&c).unwrap();
        println!(
            "synthetic library path units: {}; backup path units: {}",
            dest.as_os_str().len(),
            backup.as_os_str().len()
        );
        drop((c, writer));
        fs::remove_dir_all(root).unwrap();
    }
}
