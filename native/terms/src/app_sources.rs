//! 当前图库的来源管理：添加、移除文件夹（或照片）来源。
//!
//! 移除的来源从 `sources` 挪到 library.json 的 `removed_sources`（旧版本不认识这个键，会忽略），
//! 它的图按「文件已消失」标记为已删除，人工结论不物理删除。重新添加同一个文件夹时沿用原来的来源 id，
//! 下一次同步按 `files` 表里的记录把这些图恢复回来。
use crate::app_library::Library;
use crate::app_settings::{config_from_text, write_config, SettingValue};
use crate::http_json::Json;
use crate::{Error, Result};
use rusqlite::OpenFlags;
use serde_json::{json, Value};
use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

fn read(library: &Library) -> Result<SettingValue> {
    config_from_text(&fs::read_to_string(library.path().join("library.json"))?)
}
fn plain(value: &SettingValue) -> Value {
    serde_json::to_value(value).expect("settings serialize to JSON")
}
fn list<'a>(config: &'a mut SettingValue, key: &str) -> &'a mut Vec<SettingValue> {
    let SettingValue::Object(fields) = config else { unreachable!("config_from_text returns an object") };
    if !fields.iter().any(|(k, _)| k == key) {
        fields.push((key.into(), SettingValue::Array(Vec::new())));
    }
    match fields.iter_mut().find(|(k, _)| k == key) {
        Some((_, SettingValue::Array(items))) => items,
        _ => panic!("library.json {key} is not an array"),
    }
}
fn entries(config: &SettingValue, key: &str) -> Vec<Value> {
    plain(config)[key].as_array().cloned().unwrap_or_default()
}
fn same_folder(a: &Path, b: &Path) -> bool {
    crate::app_paths::overlaps(a, b) && a.components().count() == b.components().count()
}
fn writable(library: &Library) -> Result<()> {
    if library.frozen() {
        return Err(Error::Invalid("测试图库是冻结的快照，不能修改来源".into()));
    }
    Ok(())
}

/// Add a folder (with the origin the user chose) or the Photos library. Returns the source id and whether a
/// previously removed source came back.
pub(crate) fn add(library: &mut Library, kind: &str, path: &str, preset: &str) -> Result<Json> {
    writable(library)?;
    let mut config = read(library)?;
    let (active, removed) = (entries(&config, "sources"), entries(&config, "removed_sources"));
    let next = active
        .iter()
        .chain(&removed)
        .filter_map(|s| s["id"].as_str()?.strip_prefix('s')?.parse::<u64>().ok())
        .max()
        .unwrap_or(0)
        + 1;
    let (entry, back) = match kind {
        "folder" => {
            if path.is_empty() {
                return Err(Error::Invalid("请选择文件夹".into()));
            }
            let folders: Vec<PathBuf> = active
                .iter()
                .filter(|s| s["kind"] == "folder")
                .filter_map(|s| s["path"].as_str().map(PathBuf::from))
                .collect();
            let canonical = crate::app_paths::canonical(Path::new(path)).ok();
            let back = removed.iter().position(|s| {
                s["kind"] == "folder"
                    && canonical.as_ref().zip(s["path"].as_str()).is_some_and(|(c, old)| same_folder(c, Path::new(old)))
            });
            let id = back.map(|i| removed[i]["id"].as_str().unwrap_or("").to_owned()).unwrap_or(format!("s{next}"));
            let (_, entry) = crate::app_create::folder_source(&id, Path::new(path), preset, &folders)?;
            (entry, back)
        }
        "photos" => {
            if !cfg!(target_os = "macos") {
                return Err(Error::Invalid("Unsupported：照片图库只在 macOS 上可用".into()));
            }
            if active.iter().any(|s| s["kind"] == "photos") {
                return Err(Error::Invalid("照片图库已经是这个图库的来源".into()));
            }
            let back = removed.iter().position(|s| s["kind"] == "photos");
            let entry = match back {
                Some(i) => {
                    let mut old = removed[i].clone();
                    old.as_object_mut().expect("validated source object").remove("removed_at");
                    old
                }
                None => json!({"id":format!("s{next}"),"kind":"photos",
                    "path":crate::app_libraries::photos_library()?.to_string_lossy(),"apps":crate::origin::photos_defaults()}),
            };
            (entry, back)
        }
        _ => return Err(Error::Invalid("请选择来源".into())),
    };
    let id = entry["id"].as_str().unwrap_or("").to_owned();
    if let Some(i) = back {
        list(&mut config, "removed_sources").remove(i);
    }
    let setting: SettingValue = serde_json::from_value(entry).map_err(|e| Error::Invalid(e.to_string()))?;
    list(&mut config, "sources").push(setting);
    write_config(library.path(), &config)?;
    library.reload_config()?;
    if kind == "folder" {
        // A folder that comes back may come back with another origin.
        crate::app_settings::set_asset_preset(library, &id, preset)?;
    }
    Ok(Json::object([("id", id.as_str().into()), ("returned", back.is_some().into())]))
}

/// Without `confirm`, only report how many pictures would leave the library. With it, retire the source and mark
/// its pictures deleted; a later sync carries that into the browsing database.
pub(crate) fn remove(library: &mut Library, source_id: &str, confirm: bool) -> Result<Json> {
    writable(library)?;
    let mut config = read(library)?;
    let active = entries(&config, "sources");
    let index = active
        .iter()
        .position(|s| s["id"].as_str() == Some(source_id))
        .ok_or_else(|| Error::Invalid("没有这个来源".into()))?;
    if active.len() == 1 {
        return Err(Error::Invalid("图库至少要保留一个来源；可以先添加新来源再移除这个".into()));
    }
    let mut conn = crate::sqlite::open_with_flags(library.source(), OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    conn.busy_timeout(Duration::from_secs(30))?;
    let count: i64 =
        conn.query_row("SELECT count(*) FROM assets WHERE source_id=? AND coalesce(trashed,0)=0", [source_id], |r| {
            r.get(0)
        })?;
    if !confirm {
        return Ok(Json::object([("confirm", true.into()), ("count", count.into())]));
    }
    // Pictures first: if writing library.json then fails, the source is still listed and its next scan
    // finds every file present again and restores them.
    let tx = conn.transaction()?;
    tx.execute("UPDATE assets SET trashed=1 WHERE source_id=? AND coalesce(trashed,0)=0", [source_id])?;
    tx.commit()?;
    let mut retired = list(&mut config, "sources").remove(index);
    let stamp = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%:z").to_string();
    retired.set("removed_at".into(), SettingValue::Scalar(stamp.into()));
    list(&mut config, "removed_sources").push(retired);
    write_config(library.path(), &config)?;
    library.reload_config()?;
    Ok(Json::object([("removed", count.into())]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("illustash-sources-{}", crate::store::new_key().unwrap()));
            for folder in ["a", "b", "b/inner"] {
                fs::create_dir_all(root.join(folder)).unwrap();
            }
            image::RgbImage::new(4, 3).save(root.join("a/one.png")).unwrap();
            image::RgbImage::new(5, 3).save(root.join("b/two.png")).unwrap();
            let sources = vec![
                crate::app_create::NewSource::Folder { path: root.join("a"), preset: "pixiv".into() },
                crate::app_create::NewSource::Folder { path: root.join("b"), preset: "x".into() },
            ];
            crate::app_create::create_sources(&root.join("合成.pilib"), "合成", sources, None).unwrap();
            Self(root)
        }
        fn library(&self) -> Library {
            Library::open(&self.0.join("合成.pilib")).unwrap()
        }
        fn scan_all(&self, library: &Library) {
            for source in library.config()["sources"].as_array().unwrap() {
                crate::app_folder::scan(
                    source,
                    &library.source(),
                    &|| crate::app_sync::check(&AtomicBool::new(false)),
                    &|_, _| {},
                )
                .unwrap();
            }
        }
        fn live(&self, library: &Library, sid: &str) -> i64 {
            let c = crate::sqlite::open(library.source()).unwrap();
            c.query_row("SELECT count(*) FROM assets WHERE source_id=? AND coalesce(trashed,0)=0", [sid], |r| r.get(0))
                .unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn create_rejects_nested_or_repeated_folders_and_missing_origins() {
        let f = Fixture::new();
        for (sources, message) in [
            (vec![(f.0.join("b"), "x"), (f.0.join("b/inner"), "pixiv")], "重叠"),
            (vec![(f.0.join("b/inner"), "x"), (f.0.join("b"), "pixiv")], "重叠"),
            (vec![(f.0.join("a"), "x"), (f.0.join("a"), "x")], "重叠"),
            (vec![(f.0.join("a"), "guess")], "来源预设"),
        ] {
            let sources = sources
                .into_iter()
                .map(|(path, preset)| crate::app_create::NewSource::Folder { path, preset: preset.into() })
                .collect();
            let error = crate::app_create::create_sources(&f.0.join("bad.pilib"), "bad", sources, None).unwrap_err();
            assert!(error.to_string().contains(message), "{error}");
            assert!(!f.0.join("bad.pilib").exists());
        }
        let config: Value =
            serde_json::from_str(&fs::read_to_string(f.0.join("合成.pilib/library.json")).unwrap()).unwrap();
        assert_eq!(config["sources"][1]["id"], "s2");
        assert_eq!(config["sources"][1]["preset"], "x");
        assert_eq!(config["sources"][1]["preset_origin"], "user");
    }

    #[test]
    fn remove_asks_first_then_retires_and_readding_restores_the_same_pictures() {
        let f = Fixture::new();
        let mut library = f.library();
        f.scan_all(&library);
        let c = crate::sqlite::open(library.source()).unwrap();
        let before: String = c.query_row("SELECT uuid FROM assets WHERE source_id='s1'", [], |r| r.get(0)).unwrap();
        drop(c);

        let asked = remove(&mut library, "s1", false).unwrap();
        assert_eq!(asked.dump(), r#"{"confirm": true, "count": 1}"#);
        assert_eq!(f.live(&library, "s1"), 1);

        assert_eq!(remove(&mut library, "s1", true).unwrap().dump(), r#"{"removed": 1}"#);
        assert_eq!(f.live(&library, "s1"), 0);
        assert_eq!(library.config()["sources"].as_array().unwrap().len(), 1);
        assert_eq!(library.config()["removed_sources"][0]["id"], "s1");
        assert!(library.config()["removed_sources"][0]["removed_at"].is_string());
        assert!(remove(&mut library, "s2", false).unwrap_err().to_string().contains("至少要保留一个来源"));

        // Nested in an active source: refused. The same folder: comes back as s1, now with another origin.
        let nested = add(&mut library, "folder", f.0.join("b/inner").to_str().unwrap(), "other").unwrap_err();
        assert!(nested.to_string().contains("重叠"), "{nested}");
        let back = add(&mut library, "folder", f.0.join("a").to_str().unwrap(), "other").unwrap();
        assert_eq!(back.dump(), r#"{"id": "s1", "returned": true}"#);
        assert!(library.config()["removed_sources"].as_array().unwrap().is_empty());
        f.scan_all(&library);
        let c = crate::sqlite::open(library.source()).unwrap();
        let (after, preset): (String, String) = c
            .query_row("SELECT uuid,preset FROM assets WHERE source_id='s1' AND trashed=0", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!((after.as_str(), preset.as_str()), (before.as_str(), "other"));
    }

    #[test]
    fn new_folders_take_the_next_id_and_frozen_libraries_refuse_changes() {
        let f = Fixture::new();
        fs::create_dir(f.0.join("c")).unwrap();
        let mut library = f.library();
        assert_eq!(
            add(&mut library, "folder", f.0.join("c").to_str().unwrap(), "pixiv").unwrap().dump(),
            r#"{"id": "s3", "returned": false}"#
        );
        assert!(add(&mut library, "folder", f.0.join("missing").to_str().unwrap(), "x").is_err());
        assert!(add(&mut library, "folder", f.0.join("c").to_str().unwrap(), "guess").is_err());
        let path = library.path().join("library.json");
        let mut config: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        config["test"] = json!({"persona":"合成"});
        fs::write(&path, config.to_string()).unwrap();
        let mut frozen = f.library();
        assert!(remove(&mut frozen, "s3", true).unwrap_err().to_string().contains("冻结"));
    }
}
