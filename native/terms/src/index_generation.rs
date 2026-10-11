//! Publication generation of the shared character index (anchors and per-character files).
//!
//! The index job replaces several files that readers must see as one version. It counts in
//! `index-generation` next to them: odd while files are being replaced, even when a set is complete.
//! A reader takes the generation before and after reading and keeps the result only if both are the
//! same even number; otherwise it reads again. A crash during publication leaves the number odd, so
//! readers refuse until the job's journal resume completes the set. No file means generation 0.
use crate::{Error, Result};
use std::{path::Path, time::Duration};

const FILE: &str = "index-generation";
/// About five seconds of retries: a publication itself only renames files that are already staged.
const ATTEMPTS: usize = 50;
const PAUSE: Duration = Duration::from_millis(100);

pub(crate) fn current(shared: &Path) -> Result<u64> {
    match std::fs::read_to_string(shared.join(FILE)) {
        Ok(text) => text.trim().parse().map_err(|_| Error::Invalid("角色索引代号文件无效".into())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(e) => Err(e.into()),
    }
}

fn store(shared: &Path, generation: u64) -> Result<()> {
    let tmp = shared.join(format!("{FILE}.{}.tmp", crate::store::new_key()?));
    std::fs::write(&tmp, generation.to_string())?;
    std::fs::rename(tmp, shared.join(FILE))?;
    Ok(())
}

/// Mark a publication as started; an unfinished one (odd) is resumed under the same number.
pub(crate) fn start(shared: &Path) -> Result<u64> {
    let generation = current(shared)?;
    if generation % 2 == 1 {
        return Ok(generation);
    }
    std::fs::create_dir_all(shared)?;
    store(shared, generation + 1)?;
    Ok(generation + 1)
}

/// Mark the publication started by `start` as complete.
pub(crate) fn finish(shared: &Path, started: u64) -> Result<()> {
    store(shared, started + 1)
}

/// Read a consistent set: retried while a publication is in progress or completes during the read.
pub(crate) fn stable<T>(shared: &Path, mut read: impl FnMut() -> Result<T>) -> Result<T> {
    // No shared index configured: nothing is published there.
    if shared.as_os_str().is_empty() {
        return read();
    }
    for _ in 0..ATTEMPTS {
        let before = current(shared)?;
        if before % 2 == 0 {
            let value = read();
            if current(shared)? == before {
                return value;
            }
        }
        std::thread::sleep(PAUSE);
    }
    Err(Error::Invalid("角色索引正在更新，请稍后重试".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("illustash-index-generation-{}", crate::store::new_key().unwrap()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn readers_keep_only_reads_within_one_complete_generation() {
        let d = dir();
        assert_eq!(current(&d).unwrap(), 0);
        assert_eq!(stable(&d, || Ok(1)).unwrap(), 1);
        let started = start(&d).unwrap();
        assert_eq!((started, current(&d).unwrap()), (1, 1));
        // A crashed publication is resumed under the same number, and readers refuse until it ends.
        assert_eq!(start(&d).unwrap(), 1);
        let calls = std::cell::Cell::new(0);
        let refused = stable(&d, || {
            calls.set(calls.get() + 1);
            Ok(())
        });
        assert!(matches!(refused, Err(Error::Invalid(m)) if m.contains("正在更新")));
        assert_eq!(calls.get(), 0);
        finish(&d, started).unwrap();
        assert_eq!(current(&d).unwrap(), 2);
        // A publication that completes during a read makes the reader read again.
        let reads = std::cell::Cell::new(0);
        let value = stable(&d, || {
            reads.set(reads.get() + 1);
            if reads.get() == 1 {
                let g = start(&d)?;
                finish(&d, g)?;
            }
            Ok(reads.get())
        })
        .unwrap();
        assert_eq!((value, current(&d).unwrap()), (2, 4));
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn recognition_snapshot_refuses_an_unfinished_publication() {
        let d = dir();
        // A half-replaced pair: the ids name two anchors, the vectors file is from another version.
        std::fs::write(d.join("char-anchors.txt"), "a\nb\n").unwrap();
        std::fs::write(d.join("char-anchors.npy"), b"other version").unwrap();
        start(&d).unwrap();
        let loaded = crate::recognition_compute::anchor_snapshot::Snapshot::load(&d, &d.join("source.sqlite"));
        assert!(matches!(loaded, Err(Error::Invalid(m)) if m.contains("正在更新")));
        std::fs::remove_dir_all(d).unwrap();
    }
}
