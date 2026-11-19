//! 角色资料包的 WD 分片缓存和完整文件组装。经过校验的完整文件沿用旧包的安装路径。
use super::{
    activate, agent, check_space, fetch_entry, has, part, partial_dir, usable, Entry, Meter, Models, Place, Source,
    Stop,
};
use crate::app_packs::{Shards, WD_COEFFICIENT_BYTES};
use crate::{Error, Result};
use serde_json::{json, Value};
use std::{
    collections::BTreeSet,
    fs,
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

#[derive(Clone)]
pub(super) struct Download {
    files: Vec<Entry>,
}

impl Download {
    pub(super) fn new(shards: &Shards) -> Result<Self> {
        let files = shards
            .files()
            .map(|file| {
                let url = crate::app_updates::asset_url(&file.url).map_err(Error::invalid)?;
                Ok(Entry {
                    path: file.name.clone(),
                    size: file.size,
                    sha256: file.sha256.clone(),
                    source: Source::Project { url },
                    optional: false,
                })
            })
            .collect::<Result<_>>()?;
        Ok(Self { files })
    }

    fn cache(place: &Place) -> PathBuf {
        place.root.with_file_name("wd-shards")
    }

    pub(super) fn progress(&self, place: &Place) -> (u64, bool) {
        let cache = Self::cache(place);
        let mut have = 0;
        let mut started = false;
        for file in &self.files {
            if has(&cache, file) {
                have += file.size;
            } else if let Ok(m) = fs::metadata(part(&cache, file)) {
                have += m.len().min(file.size);
                started |= m.len() != 0;
            }
        }
        (have, started)
    }

    pub(super) fn download_size(&self, place: &Place) -> u64 {
        let cache = Self::cache(place);
        let mut seen = BTreeSet::new();
        self.files.iter().filter(|file| seen.insert(&file.sha256) && !has(&cache, file)).map(|file| file.size).sum()
    }

    pub(super) fn discard(&self, place: &Place) -> io::Result<()> {
        let cache = Self::cache(place);
        for file in &self.files {
            match fs::remove_file(part(&cache, file)) {
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                other => other?,
            }
        }
        Ok(())
    }

    pub(super) fn download(&self, models: &Models, place: &Place) -> std::result::Result<Value, Stop> {
        let cache = Self::cache(place);
        fs::create_dir_all(&cache)?;
        let mut seen = BTreeSet::new();
        let mut needed = Vec::new();
        for file in &self.files {
            check_stopped(models)?;
            if seen.insert(&file.sha256) && !usable(&cache, file) {
                needed.push(file);
            }
        }
        let total: u64 = needed.iter().map(|file| file.size).sum();
        let missing = needed
            .iter()
            .map(|file| file.size.saturating_sub(fs::metadata(part(&cache, file)).map_or(0, |m| m.len())))
            .sum::<u64>();
        let target = &place.manifest.files[0];
        check_space(&cache, missing + target.size)?;
        models.set(|job| job["total"] = total.into());
        let (direct, proxied) = (agent(None), agent(place.proxy.as_deref()));
        let mut meter = Meter::new(models, 0);
        for file in needed {
            check_stopped(models)?;
            if let Err(error) = fetch_entry(place, file, &cache, &direct, &proxied, &mut meter) {
                let path = part(&cache, file);
                if fs::symlink_metadata(&path).is_ok_and(|m| m.is_file() && m.len() == 0) {
                    let _ = fs::remove_file(path);
                }
                return Err(error);
            }
            meter.finish(file.size);
        }
        models.set(|job| job["speed"] = 0.into());
        self.assemble(models, place, &cache, target)?;
        Ok(json!({"message": null}))
    }

    fn assemble(&self, models: &Models, place: &Place, cache: &Path, target: &Entry) -> std::result::Result<(), Stop> {
        let partial = partial_dir(&place.root);
        fs::create_dir_all(&partial)?;
        let temporary = part(&partial, target);
        let result = (|| {
            let mut merged = fs::File::create(&temporary)?;
            let (header, coefficients) = self.files.split_first().expect("分片清单包含文件头");
            io::copy(&mut fs::File::open(cache.join(&header.path))?, &mut merged)?;
            // 旧的 WD 文件格式先排列所有头的权重，然后排列所有头的偏置。
            for bias in [false, true] {
                for file in coefficients {
                    check_stopped(models)?;
                    let mut input = fs::File::open(cache.join(&file.path))?;
                    let weights = file.size - file.size / WD_COEFFICIENT_BYTES * 4;
                    if bias {
                        input.seek(SeekFrom::Start(weights))?;
                        io::copy(&mut input, &mut merged)?;
                    } else {
                        io::copy(&mut input.take(weights), &mut merged)?;
                    }
                }
            }
            merged.sync_all()?;
            drop(merged);
            check_stopped(models)?;
            let actual = crate::recognition_compute::models::hash(&temporary, &|| {
                if models.stopped() {
                    Err(Error::invalid("已取消"))
                } else {
                    Ok(())
                }
            });
            check_stopped(models)?;
            if !has_candidate(&temporary, target) || !actual.is_ok_and(|digest| digest == target.sha256) {
                return Err(Stop::Failed("WD 扩展包校验失败".into()));
            }
            fs::rename(&temporary, partial.join(&target.path))?;
            activate(&place.root, &partial, place.manifest)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(temporary);
            let _ = fs::remove_dir(partial);
        }
        result
    }
}

fn check_stopped(models: &Models) -> std::result::Result<(), Stop> {
    if models.stopped() {
        Err(Stop::Cancelled)
    } else {
        Ok(())
    }
}

fn has_candidate(path: &Path, entry: &Entry) -> bool {
    fs::metadata(path).is_ok_and(|m| m.len() == entry.size)
}
