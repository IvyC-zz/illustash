//! 角色数据包的签名目录（#602）：WD 角色扩展包和 CCIP 长尾包不跟软件版本发布，新包列在一份签过名的目录里。
//!
//! - 目录是 `packs` 这个 Release 里的 `packs.json`，签名在旁边的 `packs.json.sig`。签名和应用更新一样是 Tauri 写的
//!   minisign 格式，但用另一把专签数据包的密钥；公钥在 `native/terms/pack-key.pub`，还没配置时整个目录功能不启用。
//! - 只跟着「检查更新」读（`app_updates` 的 check）：关掉检查更新、也没手动检查过，就从不联网读目录。读到的目录验过签、
//!   序号不比本机已有的小，才整份存进本机缓存（`extensions/catalog.json`）。下载、启用和换包仍然只在用户点了以后发生。
//! - 离线时，用包 Release 里的 `-offline.zip`（包文件加上签过名的目录）从文件导入，见 `adopt_archives`。
//! - 应用认的包 = 内置的两个（`wd-extension-v1`、`ccip-longtail-v1`，离线也能用）加上最近一份验过签的目录里、本版本能用的
//!   那些：种类认识、包格式支持、基础模型和这个构建的模型对得上、满足最低应用版本。别的条目原样留在目录里，但不提供。
use crate::app_updates::Version;
use crate::{Error, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};

/// 目录文件自己的格式。
pub(crate) const CATALOG_FORMAT: &str = "illustash-packs/1";
/// Release 里的两个文件名，也是离线包里的成员名。
pub(crate) const CATALOG_NAME: &str = "packs.json";
pub(crate) const SIGNATURE_NAME: &str = "packs.json.sig";
/// 目录所在的 Release。
const CATALOG_TAG: &str = "packs";
/// 目录里的下载地址只能指向本项目的 Release 资源；测试时前缀换成 ILLUSTASH_UPDATE_BASE（`app_updates::asset_url`）。
pub(crate) const RELEASE_PREFIX: &str = "https://github.com/shio-vinyl/illustash/releases/download/";
const CATALOG_LIMIT: u64 = 1 << 20;
const SIGNATURE_LIMIT: u64 = 4096;
const FILE_LIMIT: u64 = 512 << 20;
const ENTRY_LIMIT: usize = 64;
/// 专签数据包的公钥（Tauri 格式：minisign 公钥文本整体再 base64 一次）。`#` 开头的行是说明；只有说明时目录不启用。
const PUBKEY_FILE: &str = include_str!("../pack-key.pub");
/// 这个构建随带的 WD 标签表 `resources/tags/tags_v3.csv`（见 docs/THIRD_PARTY.md）。
pub(crate) const WD_TAGS_SHA256: &str = "298633d94d0031d2081c0893f29c82eab7f0df00b08483ba8f29d1e979441217";
/// 本机缓存的信封格式：原样保存目录字节和签名，读的时候重新验签。
const CACHE_FORMAT: &str = "illustash-packs-cache/1";
const CACHE_NAME: &str = "catalog.json";

/// 包的种类。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Wd,
    Ccip,
}

impl Kind {
    /// 目录里的种类名。
    pub(crate) fn id(self) -> &'static str {
        match self {
            Self::Wd => "wd-extension",
            Self::Ccip => "ccip-longtail",
        }
    }
    /// 页面和接口用的短名。
    pub(crate) fn key(self) -> &'static str {
        match self {
            Self::Wd => "wd",
            Self::Ccip => "ccip",
        }
    }
    fn parse(id: &str) -> Option<Self> {
        match id {
            "wd-extension" => Some(Self::Wd),
            "ccip-longtail" => Some(Self::Ccip),
            _ => None,
        }
    }
    /// 这个版本读得了的包格式。旧格式 `illustash-wd-extension/1` 的包全部算一级；`/2` 给每个头标级别。
    fn formats(self) -> &'static [&'static str] {
        match self {
            Self::Wd => &[WD_FORMAT_V1, WD_FORMAT_V2],
            Self::Ccip => &[CCIP_FORMAT],
        }
    }
}

pub(crate) const WD_FORMAT_V1: &str = "illustash-wd-extension/1";
pub(crate) const WD_FORMAT_V2: &str = "illustash-wd-extension/2";
pub(crate) const CCIP_FORMAT: &str = "illustash-557-ccip-offline/1";

/// 包文件。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct File {
    pub(crate) name: String,
    pub(crate) size: u64,
    pub(crate) sha256: String,
    pub(crate) url: String,
}

/// 包依赖的基础模型身份。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Base {
    Wd { model: String, tags: String },
    Ccip { model: String },
}

/// 一个可用的包：内置的，或目录里本版本能用的一条。
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Pack {
    pub(crate) kind: Kind,
    /// 同一种类里的先后；越大越新。
    pub(crate) revision: u32,
    /// 也是共享数据里 `extensions/` 下的文件夹名。
    pub(crate) version: String,
    pub(crate) format: String,
    /// WD 的头数或 CCIP 的角色数。
    pub(crate) heads: usize,
    /// 其中只出候选、永不补主归属的二级头数（只有 WD 的 `/2` 格式有）。
    pub(crate) candidate_only: usize,
    pub(crate) file: File,
    pub(crate) base: Base,
    pub(crate) license: String,
    /// 一行说明（中文、英文）；内置包没有。
    pub(crate) note: Option<(String, String)>,
    pub(crate) builtin: bool,
}

impl Pack {
    pub(crate) fn view(&self) -> Value {
        json!({
            "kind":self.kind.id(),"version":self.version,"revision":self.revision,"format":self.format,"heads":self.heads,
            "candidate_only":self.candidate_only,"license":self.license,"builtin":self.builtin,
            "note":self.note.as_ref().map(|(zh, en)| json!({"zh-CN":zh,"en":en})),
            "file":{"path":self.file.name,"size":self.file.size,"sha256":self.file.sha256},
        })
    }
}

// ---------- 目录格式 ----------

#[derive(Deserialize)]
struct RawCatalog {
    format: String,
    serial: u64,
    packs: Vec<RawEntry>,
}

#[derive(Deserialize)]
struct RawEntry {
    kind: String,
    revision: u32,
    version: String,
    format: String,
    heads: usize,
    #[serde(default)]
    candidate_only: usize,
    file: RawFile,
    base: RawBase,
    min_app: String,
    license: String,
    #[serde(default)]
    note: Option<RawNote>,
}

#[derive(Deserialize)]
struct RawFile {
    name: String,
    size: u64,
    sha256: String,
    url: String,
}

#[derive(Deserialize, Default)]
struct RawBase {
    wd_model_sha256: Option<String>,
    wd_tags_sha256: Option<String>,
    ccip_model_sha256: Option<String>,
}

#[derive(Deserialize)]
struct RawNote {
    #[serde(rename = "zh-CN")]
    zh: String,
    en: String,
}

/// 一条结构合法的目录条目（还没看本版本能不能用）。
#[derive(Clone, Debug)]
pub(crate) struct Entry {
    kind: String,
    pack: Pack,
    min_app: Version,
}

/// 验过签、结构合法的一份目录。
#[derive(Clone, Debug)]
pub(crate) struct Catalog {
    pub(crate) serial: u64,
    entries: Vec<Entry>,
}

fn bad(why: &str) -> Error {
    Error::invalid(format!("包目录无效：{why}"))
}

pub(crate) fn hex64(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// 用作文件夹名、文件名的一段：字母数字开头，只含字母数字和 `.`、`-`、`_`，不含 `..`。
fn segment(text: &str, limit: usize) -> bool {
    !text.is_empty()
        && text.len() <= limit
        && text.bytes().next().is_some_and(|b| b.is_ascii_alphanumeric())
        && text.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
        && !text.contains("..")
}

fn short(text: &str, limit: usize) -> bool {
    !text.trim().is_empty() && text.chars().count() <= limit && !text.chars().any(char::is_control)
}

impl Catalog {
    /// 解析目录并逐项检查结构。任何一条结构不对，整份不用；本版本不认识的种类、格式等留到 `packs` 再筛。
    pub(crate) fn parse(bytes: &[u8]) -> Result<Self> {
        let raw: RawCatalog = serde_json::from_slice(bytes).map_err(|e| bad(&e.to_string()))?;
        if raw.format != CATALOG_FORMAT {
            return Err(bad("目录格式不是这个版本认识的"));
        }
        if raw.serial == 0 {
            return Err(bad("序号须为正整数"));
        }
        if raw.packs.len() > ENTRY_LIMIT {
            return Err(bad("条目过多"));
        }
        let mut entries = Vec::new();
        for raw in raw.packs {
            let entry = Self::entry(raw)?;
            let clash = entries.iter().any(|e: &Entry| {
                e.pack.version == entry.pack.version
                    || e.pack.file.sha256 == entry.pack.file.sha256
                    || (e.kind == entry.kind && e.pack.revision == entry.pack.revision)
            });
            if clash {
                return Err(bad("版本、修订号或 SHA-256 重复"));
            }
            entries.push(entry);
        }
        Ok(Self { serial: raw.serial, entries })
    }

    fn entry(raw: RawEntry) -> Result<Entry> {
        let f = &raw.file;
        let tag = f.url.strip_prefix(RELEASE_PREFIX).and_then(|rest| rest.split_once('/'));
        let valid = segment(&raw.kind, 32)
            && segment(&raw.version, 64)
            && raw.revision >= 1
            && (1..=64).contains(&raw.format.len())
            && raw.format.bytes().all(|b| b.is_ascii_graphic())
            && (1..=1_000_000).contains(&raw.heads)
            && raw.candidate_only <= raw.heads
            && segment(&f.name, 128)
            && (1..=FILE_LIMIT).contains(&f.size)
            && hex64(&f.sha256)
            && tag.is_some_and(|(tag, name)| segment(tag, 64) && name == f.name)
            && short(&raw.license, 200)
            && raw.note.as_ref().is_none_or(|n| short(&n.zh, 200) && short(&n.en, 200));
        if !valid {
            let version = raw.version.chars().take(64).collect::<String>();
            return Err(bad(&format!("条目 {version} 的字段不合法")));
        }
        let min_app = Version::parse(&raw.min_app).ok_or_else(|| bad("最低应用版本不是 X.Y.Z"))?;
        let hex = |v: &Option<String>| v.as_deref().filter(|s| hex64(s)).map(str::to_owned);
        let base = match Kind::parse(&raw.kind) {
            Some(Kind::Wd) => Base::Wd {
                model: hex(&raw.base.wd_model_sha256).ok_or_else(|| bad("缺少 WD 模型的 SHA-256"))?,
                tags: hex(&raw.base.wd_tags_sha256).ok_or_else(|| bad("缺少 WD 标签表的 SHA-256"))?,
            },
            Some(Kind::Ccip) => Base::Ccip {
                model: hex(&raw.base.ccip_model_sha256).ok_or_else(|| bad("缺少 CCIP 模型的 SHA-256"))?,
            },
            // 以后的种类：结构照查，本版本不提供。
            None => Base::Ccip { model: String::new() },
        };
        Ok(Entry {
            pack: Pack {
                kind: Kind::parse(&raw.kind).unwrap_or(Kind::Ccip),
                revision: raw.revision,
                version: raw.version,
                format: raw.format,
                heads: raw.heads,
                candidate_only: raw.candidate_only,
                file: File { name: raw.file.name, size: raw.file.size, sha256: raw.file.sha256, url: raw.file.url },
                base,
                license: raw.license,
                note: raw.note.map(|n| (n.zh, n.en)),
                builtin: false,
            },
            kind: raw.kind,
            min_app,
        })
    }

    /// 本版本能用的条目：种类认识、格式支持、基础模型对得上、满足最低应用版本；与内置包同版本或同修订号的，
    /// 只在就是内置包本身时算数（交给内置的那条）。
    pub(crate) fn packs(&self, kind: Kind, context: &Context) -> Vec<Pack> {
        let builtin = context.builtin(kind);
        self.entries
            .iter()
            .filter(|e| Kind::parse(&e.kind) == Some(kind))
            .filter(|e| kind.formats().contains(&e.pack.format.as_str()))
            .filter(|e| e.pack.format != WD_FORMAT_V1 || e.pack.candidate_only == 0)
            .filter(|e| e.pack.base == builtin.base)
            .filter(|e| context.app.core() >= e.min_app.core())
            .filter(|e| {
                e.pack.file.sha256 != builtin.file.sha256
                    && e.pack.version != builtin.version
                    && e.pack.revision != builtin.revision
            })
            .map(|e| e.pack.clone())
            .collect()
    }
}

/// 判断条目能不能用时对照的东西：这个构建的版本和内置包（内置包的基础模型就是这个构建的模型）。
pub(crate) struct Context {
    pub(crate) app: Version,
    pub(crate) wd: Pack,
    pub(crate) ccip: Pack,
}

impl Context {
    pub(crate) fn current() -> Result<Self> {
        Ok(Self { app: crate::app_updates::current(), wd: builtin_wd()?, ccip: builtin_ccip()? })
    }
    fn builtin(&self, kind: Kind) -> &Pack {
        match kind {
            Kind::Wd => &self.wd,
            Kind::Ccip => &self.ccip,
        }
    }
}

/// 内置的 WD 角色扩展包（models.json 的 `wd_extension`，走查时可换成合成包，见 `app_models::wd_extension`）。
pub(crate) fn builtin_wd() -> Result<Pack> {
    let spec = crate::app_models::wd_extension()?;
    let (name, size, sha256, url) = crate::app_models::entry_parts(&spec.file);
    Ok(Pack {
        kind: Kind::Wd,
        revision: spec.revision,
        version: format!("wd-extension-v{}", spec.revision),
        format: spec.format.clone(),
        heads: spec.heads,
        candidate_only: 0,
        file: File { name, size, sha256, url },
        base: Base::Wd {
            model: spec.base_sha256.clone(),
            tags: spec.tags_sha256.clone().unwrap_or_else(|| WD_TAGS_SHA256.into()),
        },
        license: spec.license.clone(),
        note: None,
        builtin: true,
    })
}

/// 内置的 CCIP 长尾包（app_ccip_longtail 的冻结常量，走查时可换成合成包）。
pub(crate) fn builtin_ccip() -> Result<Pack> {
    let release = crate::app_ccip_longtail::release()?;
    Ok(Pack {
        kind: Kind::Ccip,
        revision: 1,
        version: crate::app_ccip_longtail::PACKAGE_VERSION.into(),
        format: CCIP_FORMAT.into(),
        heads: release.roles,
        candidate_only: 0,
        file: File {
            name: crate::app_ccip_longtail::PACKAGE_NAME.into(),
            size: release.bytes,
            sha256: release.sha256.clone(),
            url: release.url.clone(),
        },
        base: Base::Ccip { model: release.ccip_sha256.clone() },
        license: "MIT".into(),
        note: None,
        builtin: true,
    })
}

// ---------- 签名 ----------

/// 专签数据包的公钥。仓库里的 `pack-key.pub` 还是占位时为 None，目录功能不启用。
/// ILLUSTASH_UPDATE_BASE 指向本机测试服务时（测试和走查），只认 `ILLUSTASH_TEST_PACKS_PUBKEY` 给的测试公钥，
/// 没给就不启用：本机测试服务拿不出用正式密钥签的目录，检查更新的测试也不会因为配了正式公钥而多出请求。
pub(crate) fn pubkey() -> Option<String> {
    if std::env::var_os("ILLUSTASH_UPDATE_BASE").is_some() {
        return std::env::var("ILLUSTASH_TEST_PACKS_PUBKEY")
            .ok()
            .map(|k| k.trim().to_owned())
            .filter(|k| !k.is_empty());
    }
    configured(PUBKEY_FILE)
}

fn configured(text: &str) -> Option<String> {
    text.lines().map(str::trim).find(|line| !line.is_empty() && !line.starts_with('#')).map(str::to_owned)
}

/// 用 Tauri 格式的公钥核对 Tauri 格式的签名（`cargo tauri signer sign` 写的 `.sig`）。只认预哈希的签名（minisign 的
/// `ED`），和 `cargo tauri signer` 写出的一样。
pub(crate) fn verify(bytes: &[u8], signature: &[u8], pubkey: &str) -> Result<()> {
    let decode = |text: &[u8]| -> Option<String> {
        let raw =
            STANDARD.decode(text.iter().copied().filter(|b| !b.is_ascii_whitespace()).collect::<Vec<_>>()).ok()?;
        String::from_utf8(raw).ok()
    };
    let key = decode(pubkey.as_bytes())
        .and_then(|text| minisign_verify::PublicKey::decode(&text).ok())
        .ok_or_else(|| Error::invalid("包目录的公钥无效"))?;
    let signature = decode(signature)
        .and_then(|text| minisign_verify::Signature::decode(&text).ok())
        .ok_or_else(|| Error::invalid("包目录的签名无效"))?;
    key.verify(bytes, &signature, false).map_err(|error| match error {
        minisign_verify::Error::UnexpectedKeyId => Error::invalid("包目录不是用数据包的密钥签的"),
        _ => Error::invalid("包目录的签名对不上"),
    })
}

// ---------- 本机缓存 ----------

/// 共享数据里放数据包的文件夹（和 WD 角色扩展包同一处）。
pub(crate) fn extensions_root(resources: &Path) -> Result<PathBuf> {
    crate::app_models::extensions_root(resources)
}

fn cache_path(resources: &Path) -> Result<PathBuf> {
    Ok(extensions_root(resources)?.join(CACHE_NAME))
}

#[derive(Deserialize)]
struct Cache {
    format: String,
    catalog: String,
    signature: String,
}

/// 本机最近一份验过签的目录。没有公钥、没有缓存，或缓存验不过签（被改过、换了密钥）时为 None。
pub(crate) fn cached(resources: &Path) -> Result<Option<Catalog>> {
    let Some(key) = pubkey() else { return Ok(None) };
    Ok(read_cache(&cache_path(resources)?, &key).map(|(catalog, _)| catalog))
}

fn read_cache(path: &Path, key: &str) -> Option<(Catalog, Vec<u8>)> {
    let text = fs::read(path).ok()?;
    let cache: Cache = serde_json::from_slice(&text).ok()?;
    let bytes = STANDARD.decode(cache.catalog).ok()?;
    let signature = STANDARD.decode(cache.signature).ok()?;
    if cache.format != CACHE_FORMAT || verify(&bytes, &signature, key).is_err() {
        return None;
    }
    Some((Catalog::parse(&bytes).ok()?, bytes))
}

/// 采用一份目录的结果。
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Adopted {
    /// 比本机的新（或本机还没有），已存下。
    Newer,
    /// 和本机的是同一份。
    Same,
    /// 序号比本机的小，没有采用。
    Older,
}

/// 验签、检查结构，序号不比本机的小才存下（同一个序号只认同一份）。存的是原样的字节和签名，写临时文件再改名。
pub(crate) fn adopt(resources: &Path, bytes: &[u8], signature: &[u8]) -> Result<Adopted> {
    let key = pubkey().ok_or_else(|| Error::Conflict("这个版本还没有配置数据包目录的公钥".into()))?;
    adopt_at(&cache_path(resources)?, bytes, signature, &key)
}

fn adopt_at(path: &Path, bytes: &[u8], signature: &[u8], key: &str) -> Result<Adopted> {
    verify(bytes, signature, key)?;
    let catalog = Catalog::parse(bytes)?;
    if let Some((current, existing)) = read_cache(path, key) {
        if existing == bytes {
            return Ok(Adopted::Same);
        }
        if catalog.serial < current.serial {
            return Ok(Adopted::Older);
        }
        if catalog.serial == current.serial {
            return Err(bad("和本机已有的目录序号相同、内容不同"));
        }
    }
    let envelope =
        json!({"format":CACHE_FORMAT,"catalog":STANDARD.encode(bytes),"signature":STANDARD.encode(signature)});
    fs::create_dir_all(path.parent().expect("目录缓存有上级文件夹"))?;
    let tmp = path.with_extension(format!("{}.tmp", crate::store::new_key()?));
    let written = fs::write(&tmp, envelope.to_string() + "\n").and_then(|()| fs::rename(&tmp, path));
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written?;
    Ok(Adopted::Newer)
}

// ---------- 联网读取 ----------

fn get(agent: &ureq::Agent, address: &str, limit: u64) -> std::result::Result<Vec<u8>, String> {
    let reply = agent.get(address).set("User-Agent", "Illustash-pack-catalog").call().map_err(|e| match e {
        ureq::Error::Status(code, _) => format!("包目录的地址返回了错误（HTTP {code}）"),
        ureq::Error::Transport(_) => "连不上 GitHub，没能读取包目录".into(),
    })?;
    let mut bytes = Vec::new();
    reply.into_reader().take(limit + 1).read_to_end(&mut bytes).map_err(|_| "读取包目录失败".to_string())?;
    if bytes.len() as u64 > limit {
        return Err("包目录过大".into());
    }
    Ok(bytes)
}

/// 检查更新时顺带读一次目录（`app_updates` 的 check）。没有公钥时什么都不做；失败变成一句话，不影响检查更新本身。
/// 目录是普通文件，GitHub 的下载地址会跳转到存储服务，所以这里跟随跳转；内容靠签名把关。
pub(crate) fn refresh(resources: &Path) -> Option<std::result::Result<Adopted, String>> {
    pubkey()?;
    Some((|| {
        let prefix = crate::app_updates::asset_url(&format!("{RELEASE_PREFIX}{CATALOG_TAG}/"))?;
        let agent = crate::app_models::download_agent(Duration::from_secs(15), &prefix);
        let bytes = get(&agent, &format!("{prefix}{CATALOG_NAME}"), CATALOG_LIMIT)?;
        let signature = get(&agent, &format!("{prefix}{SIGNATURE_NAME}"), SIGNATURE_LIMIT)?;
        adopt(resources, &bytes, &signature).map_err(|e| e.reason())
    })())
}

// ---------- 离线导入 ----------

/// 所选文件里的离线包（ZIP，根上有 `packs.json` 和 `packs.json.sig`）：先采用其中的目录，再按它认包文件。
/// 返回每个目录成员的导入结果（「名字 → 结果」，名字的写法和导入结果列表一致：`包.zip/packs.json`）。
pub(crate) fn adopt_archives(resources: &Path, paths: &[PathBuf]) -> Vec<(String, &'static str)> {
    let mut results = Vec::new();
    for path in paths {
        if !path.extension().is_some_and(|x| x.eq_ignore_ascii_case("zip")) {
            continue;
        }
        let Ok(file) = fs::File::open(path) else { continue };
        let Ok(mut archive) = zip::ZipArchive::new(file) else { continue };
        let mut member = |name: &str, limit: u64| -> Option<Vec<u8>> {
            let item = archive.by_name(name).ok()?;
            if !item.is_file() || item.size() > limit {
                return None;
            }
            let mut bytes = Vec::new();
            item.take(limit + 1).read_to_end(&mut bytes).ok()?;
            (bytes.len() as u64 <= limit).then_some(bytes)
        };
        let (Some(bytes), Some(signature)) =
            (member(CATALOG_NAME, CATALOG_LIMIT), member(SIGNATURE_NAME, SIGNATURE_LIMIT))
        else {
            continue;
        };
        let result = match adopt(resources, &bytes, &signature) {
            Ok(Adopted::Newer | Adopted::Same) => "catalog",
            Ok(Adopted::Older) => "catalog_old",
            Err(_) => "catalog_invalid",
        };
        let zip = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        results.push((format!("{zip}/{CATALOG_NAME}"), result));
        results.push((format!("{zip}/{SIGNATURE_NAME}"), result));
    }
    results
}

// ---------- 可用的包 ----------

/// 某一种类可用的包，内置的在前，其余按修订号从旧到新。
pub(crate) fn known(resources: &Path, kind: Kind) -> Result<Vec<Pack>> {
    let context = Context::current()?;
    let mut packs = vec![context.builtin(kind).clone()];
    if let Some(catalog) = cached(resources)? {
        packs.extend(catalog.packs(kind, &context));
    }
    packs.sort_by_key(|p| (!p.builtin, p.revision));
    Ok(packs)
}

/// 按包文件的 SHA-256 找可用的包（图库里记的就是它）。
pub(crate) fn by_sha(resources: &Path, kind: Kind, sha256: &str) -> Result<Option<Pack>> {
    Ok(known(resources, kind)?.into_iter().find(|p| p.file.sha256 == sha256))
}

/// 按版本名找可用的包（页面请求下载、启用或换包时带的）。
pub(crate) fn by_version(resources: &Path, kind: Kind, version: &str) -> Result<Pack> {
    known(resources, kind)?
        .into_iter()
        .find(|p| p.version == version)
        .ok_or_else(|| Error::Conflict("这个版本的数据包不在可用的列表里".into()))
}

/// 卡片上显示的包：本图库启用的那个；没启用时是本机已有的最新的一个，都没有就是最新的一个。
pub(crate) fn current(packs: &[Pack], enabled: Option<&str>, present: impl Fn(&Pack) -> bool) -> Pack {
    if let Some(pack) = enabled.and_then(|sha| packs.iter().find(|p| p.file.sha256 == sha)) {
        return pack.clone();
    }
    let newest = |list: Vec<&Pack>| list.into_iter().max_by_key(|p| p.revision).cloned();
    newest(packs.iter().filter(|p| present(p)).collect())
        .or_else(|| newest(packs.iter().collect()))
        .expect("至少有内置的包")
}

/// 比卡片上的包新的最新一个（「有新版本」）。
pub(crate) fn offer(packs: &[Pack], current: &Pack) -> Option<Pack> {
    packs.iter().filter(|p| p.revision > current.revision).max_by_key(|p| p.revision).cloned()
}

#[cfg(test)]
mod tests;
