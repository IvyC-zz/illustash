//! 对实际可读输入做完整 SHA-256；版本签名只用于失效，不能代替内容摘要。
use super::*;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(super) struct Binding {
    pub(super) asset: String,
    path: Option<PathBuf>,
    kind: Option<String>,
    added_at: Option<String>,
    source_token: Option<String>,
}

impl Binding {
    pub(super) fn representation(&self) -> Option<&'static str> {
        match self.kind.as_deref() {
            Some("orig") => Some("original"),
            Some("deriv") => Some("derived"),
            _ => None,
        }
    }
    fn signature(&self) -> String {
        crate::strict_json::sha256_hex(&serde_json::to_vec(self).expect("来源绑定可序列化"))
    }
}

pub(super) fn bindings(c: &Connection) -> Result<Vec<Binding>> {
    Ok(c.prepare("SELECT uuid,src_path,src_kind,added_at,(SELECT value FROM meta WHERE key='folder_hash:'||uuid) FROM assets ORDER BY uuid")?
        .query_map([], |r| {
            Ok(Binding {
                asset: r.get(0)?,
                path: r.get::<_, Option<String>>(1)?.filter(|p| !p.is_empty()).map(PathBuf::from),
                kind: r.get(2)?,
                added_at: r.get(3)?,
                source_token: r.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(super) struct Stamp {
    pub(super) bytes: u64,
    modified: SystemTime,
}

fn stamp(path: &Path) -> Option<Stamp> {
    let metadata = fs::metadata(path).ok()?;
    metadata.is_file().then(|| Some(Stamp { bytes: metadata.len(), modified: metadata.modified().ok()? }))?
}

pub(super) struct Observed {
    pub(super) binding: Binding,
    pub(super) stamp: Option<Stamp>,
    pub(super) content: Option<Content>,
    cached: Option<Cached>,
}

#[derive(Clone)]
struct Cached {
    content: Content,
    signature: String,
    stamp: Stamp,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Mode {
    Cached,
    Fill,
}

fn cached(c: &Connection, key: &str) -> Result<Option<Cached>> {
    let row: Option<(String,u64,String,String,String)> = c.query_row(
        "SELECT digest,byte_length,representation,source_signature,file_stamp FROM human_asset_hashes WHERE asset_key=?1 AND algorithm='sha256'",
        [key], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
    row.map(|(sha256, byte_length, representation, signature, stamp)| {
        Ok(Cached {
            content: Content { sha256, byte_length, representation },
            signature,
            stamp: serde_json::from_str(&stamp).map_err(|_| Error::invalid("人工结论哈希缓存不完整"))?,
        })
    })
    .transpose()
}

fn version(cached: &Cached) -> String {
    crate::strict_json::sha256_hex(
        format!("{}:{}", cached.signature, serde_json::to_string(&cached.stamp).expect("文件状态可序列化")).as_bytes(),
    )
}

impl Observed {
    pub(super) fn source_hash(&self) -> Option<SourceHash> {
        if let (Some(content), Some(stamp)) = (&self.content, &self.stamp) {
            let cached = Cached { content: content.clone(), signature: self.binding.signature(), stamp: stamp.clone() };
            Some(SourceHash::Verified { content: content.clone(), version: version(&cached) })
        } else if self.stamp.is_none() {
            self.cached
                .as_ref()
                .filter(|c| c.signature == self.binding.signature())
                .map(|c| SourceHash::LastKnown { content: c.content.clone(), version: version(c) })
        } else {
            None
        }
    }
}

pub(super) fn removed(c: &Connection, key: &str) -> Result<Option<SourceHash>> {
    Ok(cached(c, key)?.map(|c| SourceHash::LastKnown { content: c.content.clone(), version: version(&c) }))
}

// 只有来源版本与 size/mtime 全部匹配才复用已经完整读取的摘要；离线目标始终不可核验。
pub(super) fn observe(
    c: &Connection,
    binding: Binding,
    mode: Mode,
    check: &dyn Fn() -> Result<()>,
) -> Result<Observed> {
    check()?;
    let initial = binding.path.as_deref().and_then(stamp);
    let cache = cached(c, &binding.asset)?;
    let mut observed = Observed { binding, stamp: initial, content: None, cached: cache };
    let (Some(path), Some(before), Some(representation)) =
        (observed.binding.path.as_deref(), &observed.stamp, observed.binding.representation())
    else {
        return Ok(observed);
    };
    let Ok(mut file) = File::open(path) else { return Ok(observed) };
    let Ok(opened) = file.metadata() else { return Ok(observed) };
    if !opened.is_file() || opened.len() != before.bytes || opened.modified().ok() != Some(before.modified) {
        return Err(Error::Conflict("读取图片时内容版本发生变化".into()));
    }
    if let Some(cached) =
        observed.cached.as_ref().filter(|c| c.signature == observed.binding.signature() && c.stamp == *before)
    {
        observed.content = Some(cached.content.clone());
        return Ok(observed);
    }
    if mode == Mode::Cached {
        return Ok(observed);
    }
    let mut hash = Sha256::new();
    let mut length = 0;
    let mut buffer = vec![0; 1 << 20];
    loop {
        check()?;
        let Ok(n) = file.read(&mut buffer) else { return Ok(observed) };
        if n == 0 {
            break;
        }
        length += n as u64;
        hash.update(&buffer[..n]);
    }
    if length != before.bytes || stamp(path).as_ref() != Some(before) {
        return Err(Error::Conflict("读取图片时内容版本发生变化".into()));
    }
    observed.content = Some(Content {
        sha256: format!("{:x}", hash.finalize()),
        byte_length: length,
        representation: representation.into(),
    });
    Ok(observed)
}

pub(super) fn verify(
    c: &Connection,
    observations: &[Observed],
    complete: bool,
    check: &dyn Fn() -> Result<()>,
) -> Result<()> {
    check()?;
    let current: BTreeMap<_, _> = bindings(c)?.into_iter().map(|b| (b.asset.clone(), b)).collect();
    if (complete && current.len() != observations.len())
        || observations.iter().any(|o| current.get(&o.binding.asset) != Some(&o.binding))
    {
        return Err(Error::Conflict("核验后图片来源已变化，请重试".into()));
    }
    for observation in observations {
        check()?;
        if observation.binding.path.as_deref().and_then(stamp) != observation.stamp {
            return Err(Error::Conflict("核验后图片内容已变化，请重试".into()));
        }
    }
    Ok(())
}

/// 发布前由宿主重新核对来源和文件；只改变摘要、版本或覆盖时写入，避免轮询自激修订号。
pub(super) fn publish(c: &Connection, observations: &[Observed]) -> Result<()> {
    for o in observations {
        if let (Some(content), Some(stamp)) = (&o.content, &o.stamp) {
            c.execute("INSERT INTO human_asset_hashes(asset_key,algorithm,digest,byte_length,representation,source_signature,file_stamp,observed_at)
                VALUES (?1,'sha256',?2,?3,?4,?5,?6,?7) ON CONFLICT(asset_key) DO UPDATE SET
                digest=excluded.digest,byte_length=excluded.byte_length,representation=excluded.representation,
                source_signature=excluded.source_signature,file_stamp=excluded.file_stamp,observed_at=excluded.observed_at
                WHERE digest<>excluded.digest OR byte_length<>excluded.byte_length OR representation<>excluded.representation
                OR source_signature<>excluded.source_signature OR file_stamp<>excluded.file_stamp",
                params![o.binding.asset,content.sha256,content.byte_length,content.representation,o.binding.signature(),
                    serde_json::to_string(stamp).expect("文件状态可序列化"),chrono::Utc::now().to_rfc3339()])?;
        } else if o
            .cached
            .as_ref()
            .is_some_and(|c| c.signature != o.binding.signature() || o.stamp.as_ref().is_some_and(|s| s != &c.stamp))
        {
            c.execute("DELETE FROM human_asset_hashes WHERE asset_key=?1", [&o.binding.asset])?;
        }
    }
    Ok(())
}
