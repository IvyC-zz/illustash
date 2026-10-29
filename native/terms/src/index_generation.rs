//! 共享角色索引（锚点和每个角色的文件）的发布 generation。
//!
//! 索引任务要替换好几个文件，读取方必须把它们当作同一个版本看到。任务在这些文件旁边的 `index-generation`
//! 里计数：替换文件期间为奇数，一组文件齐全时为偶数。读取方在读取前后各取一次 generation，只有两次是同一个
//! 偶数时才保留结果，否则重读。发布中途崩溃会让计数停在奇数，读取方一直拒绝，直到任务按日志恢复、把这组
//! 文件补齐。没有这个文件表示 generation 为 0。
use crate::{Error, Result};
use std::{path::Path, time::Duration};

const FILE: &str = "index-generation";
/// 大约 5 秒的重试：发布本身只是给已经准备好的文件改名。
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

/// 标记一次发布开始；未完成的发布（奇数）沿用同一个编号继续。
pub(crate) fn start(shared: &Path) -> Result<u64> {
    let generation = current(shared)?;
    if generation % 2 == 1 {
        return Ok(generation);
    }
    std::fs::create_dir_all(shared)?;
    store(shared, generation + 1)?;
    Ok(generation + 1)
}

/// 把 `start` 开始的发布标记为完成。
pub(crate) fn finish(shared: &Path, started: u64) -> Result<()> {
    store(shared, started + 1)
}

/// 读取一组一致的文件：有发布正在进行、或在读取期间完成时重试。
pub(crate) fn stable<T>(shared: &Path, mut read: impl FnMut() -> Result<T>) -> Result<T> {
    // 没有配置共享索引：那里不会发布任何东西。
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
        // 崩溃的发布沿用同一个编号继续，读取方一直拒绝，直到它结束。
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
        // 读取期间完成的发布会让读取方重读。
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
        // 替换了一半的一对文件：id 列出两个锚点，向量文件却来自另一个版本。
        std::fs::write(d.join("char-anchors.txt"), "a\nb\n").unwrap();
        std::fs::write(d.join("char-anchors.npy"), b"other version").unwrap();
        start(&d).unwrap();
        let loaded = crate::recognition_compute::anchor_snapshot::Snapshot::load(&d, &d.join("source.sqlite"));
        assert!(matches!(loaded, Err(Error::Invalid(m)) if m.contains("正在更新")));
        std::fs::remove_dir_all(d).unwrap();
    }
}
