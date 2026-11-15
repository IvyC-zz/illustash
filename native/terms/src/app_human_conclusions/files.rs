//! 私有临时文件、完整性回读和不覆盖的原子发布；仅轮换本功能可识别的有效快照。
use super::*;
use illustash_core::human_conclusions::format::Coverage;
use serde::Serialize;
use std::io::{BufWriter, Write};

const SUFFIX: &str = ".illustash-human.jsonl";

#[derive(Clone, Serialize)]
pub(super) struct Item {
    pub backup_id: String,
    pub created_at: String,
    pub state_sha256: String,
    pub hash_coverage: Coverage,
    pub portable_complete: bool,
}
impl Item {
    pub(super) fn from_snapshot(snapshot: &Snapshot) -> Result<Self> {
        let hash_coverage = snapshot.coverage();
        Ok(Self {
            backup_id: snapshot.header().snapshot_id.clone(),
            created_at: snapshot.header().exported_at.clone(),
            state_sha256: snapshot.semantic_sha256()?,
            portable_complete: hash_coverage.missing == 0,
            hash_coverage,
        })
    }
}

pub(super) fn directory(db: &Path) -> PathBuf {
    db.parent().expect("图库数据库有包目录").join("backups/human-conclusions")
}
fn id_valid(id: &str) -> bool {
    (1..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
}
pub(super) fn path(directory: &Path, id: &str) -> Result<PathBuf> {
    if !id_valid(id) {
        return Err(Error::invalid("备份编号不正确"));
    }
    Ok(directory.join(format!("human-{id}{SUFFIX}")))
}
pub(super) fn read(path: &Path, check: &dyn Fn() -> Result<()>) -> Result<Snapshot> {
    if !fs::symlink_metadata(path)?.file_type().is_file() {
        return Err(Error::invalid("快照须为普通文件"));
    }
    let file = crate::platform_filesystem::read_nofollow_options().open(path)?;
    format::read_checked(&mut BufReader::new(file), check)
}

pub(super) struct Catalog {
    pub items: Vec<Item>,
    pub unreadable: usize,
}
pub(super) fn list(directory: &Path, library: &str, check: &dyn Fn() -> Result<()>) -> Result<Catalog> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Catalog { items: Vec::new(), unreadable: 0 }),
        Err(e) => return Err(e.into()),
    };
    let mut catalog = Catalog { items: Vec::new(), unreadable: 0 };
    for entry in entries {
        check()?;
        let entry = entry?;
        let name = entry.file_name();
        let Some(id) = name
            .to_str()
            .and_then(|n| n.strip_prefix("human-"))
            .and_then(|n| n.strip_suffix(SUFFIX))
            .filter(|id| id_valid(id))
        else {
            continue;
        };
        if !entry.file_type()?.is_file() {
            continue;
        }
        match read(&entry.path(), check) {
            Ok(snapshot) if snapshot.header().library_id == library && snapshot.header().snapshot_id == id => {
                catalog.items.push(Item::from_snapshot(&snapshot)?);
            }
            _ => {
                check()?;
                catalog.unreadable += 1;
            }
        }
    }
    catalog.items.sort_by(|a, b| (&b.created_at, &b.backup_id).cmp(&(&a.created_at, &a.backup_id)));
    Ok(catalog)
}

pub(super) struct Prepared {
    temporary: PathBuf,
    destination: PathBuf,
}
impl Prepared {
    pub(super) fn new(snapshot: &Snapshot, destination: &Path, check: &dyn Fn() -> Result<()>) -> Result<Self> {
        let parent = destination.parent().ok_or_else(|| Error::invalid("导出目录不存在"))?;
        fs::create_dir_all(parent)?;
        let temporary = parent.join(format!(".human-{}.tmp", crate::store::new_key()?));
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        crate::platform_filesystem::private_file_options(&mut options);
        let file = options.open(&temporary)?;
        let prepared = Self { temporary, destination: destination.to_owned() };
        let mut out = BufWriter::new(file);
        snapshot.write_checked(&mut out, check)?;
        out.flush()?;
        out.get_ref().sync_all()?;
        drop(out);
        let reread = read(&prepared.temporary, check)?;
        if reread.header() != snapshot.header() || reread.semantic_sha256()? != snapshot.semantic_sha256()? {
            return Err(Error::invalid("临时快照回读校验失败"));
        }
        check()?;
        Ok(prepared)
    }
    pub(super) fn publish(self) -> Result<PathBuf> {
        crate::platform_filesystem::publish_new(&self.temporary, &self.destination)?;
        Ok(self.destination.clone())
    }
}
impl Drop for Prepared {
    fn drop(&mut self) {
        if let Err(e) = fs::remove_file(&self.temporary) {
            if e.kind() != std::io::ErrorKind::NotFound {
                eprintln!("人工快照临时文件未能清理：{e}");
            }
        }
    }
}

pub(super) fn prune(
    directory: &Path,
    catalog: &mut Catalog,
    keep: usize,
    check: &dyn Fn() -> Result<()>,
) -> Option<String> {
    let mut error = None;
    // 清理失败保留在清单中，下次成功快照再尝试；不能把刚发布成功改称失败。
    let mut retained = catalog.items.iter().take(keep).cloned().collect::<Vec<_>>();
    for item in catalog.items.iter().skip(keep) {
        match path(directory, &item.backup_id).and_then(|p| {
            let snapshot = read(&p, check)?;
            if snapshot.header().snapshot_id != item.backup_id || snapshot.semantic_sha256()? != item.state_sha256 {
                return Err(Error::Conflict("旧快照被独立修改，未清理".into()));
            }
            check()?;
            fs::remove_file(p).map_err(Into::into)
        }) {
            Ok(()) => (),
            Err(e) => {
                error = Some(e.reason());
                retained.push(item.clone());
            }
        }
    }
    catalog.items = retained;
    error
}
