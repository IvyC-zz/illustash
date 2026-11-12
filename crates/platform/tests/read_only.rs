//! 直接调用平台层的使用方；只用合成来源，不依赖应用层或 HTTP。
#![allow(clippy::unwrap_used, reason = "集成测试里 unwrap 就是断言")]
use illustash_platform::{platform_folder as folder, platform_photos as photos, sqlite, Error};
use std::{fs, path::PathBuf};
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "illustash-platform-read-{}-{}",
            std::process::id(),
            illustash_core::store::new_key().unwrap()
        ));
        fs::create_dir(&root).unwrap();
        Self(root)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
#[test]
fn folder_reads_keep_source_bytes_and_supplied_cancel() {
    let f = Fixture::new();
    let file = f.0.join("synthetic.png");
    image::RgbImage::new(3, 2).save(&file).unwrap();
    let before = fs::read(&file).unwrap();
    let (mut files, mut unreadable) = (Vec::new(), Vec::new());
    folder::walk(&f.0, &f.0, &mut files, &mut unreadable, &|| Ok(())).unwrap();
    assert_eq!(files.len(), 1);
    assert!(unreadable.is_empty());
    assert_eq!(files[0].rel, "synthetic.png");
    assert_eq!(&folder::describe(&files[0]).0, &3);
    assert_eq!(&folder::describe(&files[0]).1, &2);
    assert_eq!(folder::hash(&file, &|| Ok(())).unwrap().len(), 32);
    let cancel = || Err(Error::Invalid("synthetic cancellation".into()));
    assert!(matches!(folder::hash(&file, &cancel), Err(Error::Invalid(s)) if s=="synthetic cancellation"));
    let mut rejected = Vec::new();
    assert!(folder::walk(&f.0, &f.0, &mut rejected, &mut unreadable, &cancel).is_err());
    assert!(rejected.is_empty());
    assert_eq!(fs::read(file).unwrap(), before);
}
#[cfg(unix)]
#[test]
fn folder_that_cannot_be_listed_is_reported_not_skipped() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    image::RgbImage::new(1, 1).save(f.0.join("kept.png")).unwrap();
    let locked = f.0.join("sub").join("locked");
    fs::create_dir_all(&locked).unwrap();
    image::RgbImage::new(1, 1).save(locked.join("hidden.png")).unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    let (mut files, mut unreadable) = (Vec::new(), Vec::new());
    let result = folder::walk(&f.0, &f.0, &mut files, &mut unreadable, &|| Ok(()));
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    result.unwrap();
    assert_eq!(files.iter().map(|f| f.rel.as_str()).collect::<Vec<_>>(), ["kept.png"]);
    assert_eq!(unreadable, ["sub/locked"]);
}
#[test]
fn photos_reads_only_explicit_fixture_and_preserves_database() {
    let f = Fixture::new();
    fs::create_dir(f.0.join("database")).unwrap();
    let db = f.0.join("database/Photos.sqlite");
    let c = sqlite::open(&db).unwrap();
    c.execute_batch("CREATE TABLE ZASSET(ZUUID,Z_PK,ZWIDTH,ZHEIGHT,ZADDEDDATE,ZHIDDEN,ZDIRECTORY,ZFILENAME,ZADDITIONALATTRIBUTES,ZKIND,ZTRASHEDSTATE); CREATE TABLE ZADDITIONALASSETATTRIBUTES(Z_PK,ZIMPORTEDBYBUNDLEIDENTIFIER,ZORIGINALSTABLEHASH,ZORIGINALFILESIZE); INSERT INTO ZASSET VALUES ('SYNTHETIC',1,3,2,0,0,'a','b.png',1,0,0); INSERT INTO ZADDITIONALASSETATTRIBUTES VALUES(1,'synthetic-app','hash',10);").unwrap();
    drop(c);
    let before = fs::read(&db).unwrap();
    let (root, rows) = photos::read(f.0.to_str().unwrap()).unwrap();
    assert_eq!(root, f.0);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], "SYNTHETIC");
    assert_eq!(rows[0][8], "synthetic-app");
    assert_eq!(photos::path(&root, "SYNTHETIC", "a", "b.png").unwrap().0, "deriv");
    assert!(photos::path(&root, "SYNTHETIC", "..", "b.png").is_err());
    assert_eq!(fs::read(db).unwrap(), before);
}
#[test]
fn photos_refused_by_the_system_is_told_apart_from_other_failures() {
    // macOS 隐私设置拒绝时返回 EPERM；普通文件权限返回 EACCES，保留数据库错误（#202）。
    let at = std::path::Path::new("Photos.sqlite");
    let refused = illustash_core::error::io_at(std::io::Error::from_raw_os_error(libc::EPERM), at, None);
    assert!(photos::denied(&refused));
    let locked = illustash_core::error::io_at(std::io::Error::from_raw_os_error(libc::EACCES), at, None);
    assert!(!photos::denied(&locked));
    assert!(!photos::denied(&Error::Io(std::io::Error::from_raw_os_error(libc::EPERM))));
    assert!(!photos::denied(&Error::Invalid("synthetic".into())));
}
#[cfg(unix)]
#[test]
fn photos_database_without_read_permission_is_not_called_refused() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    fs::create_dir(f.0.join("database")).unwrap();
    let db = f.0.join("database/Photos.sqlite");
    sqlite::open(&db).unwrap().execute_batch("CREATE TABLE ZASSET(ZUUID)").unwrap();
    fs::set_permissions(&db, fs::Permissions::from_mode(0o000)).unwrap();
    let result = photos::read(f.0.to_str().unwrap());
    fs::set_permissions(&db, fs::Permissions::from_mode(0o644)).unwrap();
    let error = result.unwrap_err();
    assert!(matches!(error, Error::Sql(_)), "{error}");
    assert!(!photos::denied(&error));
}
