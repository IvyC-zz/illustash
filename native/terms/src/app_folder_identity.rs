//! 文件夹资产的身份依据。路径只用于区分相同内容的副本。
use std::collections::{HashMap, HashSet};

#[derive(Default)]
pub(crate) struct Earlier {
    paths: HashMap<String, HashSet<String>>,
    hashes: HashMap<String, HashSet<String>>,
    by_asset: HashMap<String, HashSet<String>>,
}

impl Earlier {
    pub(crate) fn record(&mut self, id: &str, path: Option<&str>, hash: Option<&str>) {
        if let Some(path) = path.filter(|s| !s.is_empty()) {
            self.paths.entry(path.into()).or_default().insert(id.into());
        }
        if let Some(hash) = hash.filter(|s| !s.is_empty()) {
            self.hashes.entry(hash.into()).or_default().insert(id.into());
            self.by_asset.entry(id.into()).or_default().insert(hash.into());
        }
    }

    /// 先成批认出路径和内容都一致的记录，再配对剩下的一对一内容。整个批次与遍历顺序无关。
    pub(crate) fn matches(&self, files: &[(String, String)], taken: &HashSet<String>) -> HashMap<String, String> {
        let mut matches = HashMap::new();
        let mut used = taken.clone();
        let mut exact = HashMap::<&str, Vec<&str>>::new();
        let valid =
            |id: &str, hash: &str| self.by_asset.get(id).is_some_and(|known| known.len() == 1 && known.contains(hash));
        for (path, hash) in files {
            if let Some(ids) = self.paths.get(path) {
                let mut candidates = ids.iter().filter(|id| !taken.contains(*id) && valid(id, hash));
                if let (Some(id), None) = (candidates.next(), candidates.next()) {
                    exact.entry(id).or_default().push(path);
                }
            }
        }
        for (id, paths) in exact {
            if let [path] = paths.as_slice() {
                matches.insert((*path).to_owned(), id.to_owned());
                used.insert(id.to_owned());
            }
        }
        let mut remaining = HashMap::<&str, Vec<&str>>::new();
        for (path, hash) in files {
            if !matches.contains_key(path) {
                remaining.entry(hash).or_default().push(path);
            }
        }
        for (hash, paths) in remaining {
            let ([path], Some(ids)) = (paths.as_slice(), self.hashes.get(hash)) else { continue };
            let mut candidates = ids.iter().filter(|id| !used.contains(*id));
            if let (Some(id), None) = (candidates.next(), candidates.next()) {
                if valid(id, hash) {
                    matches.insert((*path).to_owned(), id.clone());
                }
            }
        }
        matches
    }
}
