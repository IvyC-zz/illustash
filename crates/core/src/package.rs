//! 证据包 `illustash-evidence/1` 的解析与校验（格式见 docs/EVIDENCE_PACKAGES.md）。
//!
//! 只做结构校验和文件核对，不读写数据库；属于哪个图库、资产是否存在、词条能否新建由导入（store::import_package）核对。
//! 校验通过不代表授权执行、联网或上传任何东西。

use crate::error::invalid;
use crate::schema::{Kind, RelationKind, RESERVED_SOURCES};
use crate::strict_json::{self, canonical, sha256_hex};
use crate::{Error, Result};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;

pub const FORMAT: &str = "illustash-evidence/1";
pub const MANIFEST_FILE: &str = "manifest.json";
pub const EVIDENCE_FILE: &str = "evidence.jsonl";
/// 可选：产出者对词条本身的描述（外部链接、显示名），每行一个词条。
pub const TERMS_FILE: &str = "terms.jsonl";
pub const MAX_MANIFEST_BYTES: u64 = 64 << 20;
pub const MAX_FILE_BYTES: u64 = 1 << 30;
pub const MAX_ASSETS: usize = 100_000;
pub const MAX_LINE_BYTES: usize = 1 << 20;
pub const MAX_DATA_BYTES: usize = 64 << 10;

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AssetRef {
    pub namespace: String,
    pub id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Completed,
    Failed,
    Cancelled,
}

/// 每张图在这个包里的终态。完成且没有证据是「查无结果」；失败、取消不能发布证据，也不能记成查无结果。
#[derive(Clone, Debug)]
pub struct AssetOutcome {
    pub asset: AssetRef,
    pub task_id: Option<String>,
    pub status: Status,
    pub error: Option<(String, String)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileSpec {
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Clone, Debug)]
pub struct Manifest {
    pub library_id: String,
    pub producer: String,
    pub producer_version: String,
    pub created_at: String,
    pub models: Map<String, Value>,
    pub assets: Vec<AssetOutcome>,
    pub files: BTreeMap<String, FileSpec>,
    /// 原样保留，入账本。
    pub raw: Value,
}

/// 词条引用：核心里已有的 key，或外部链接（找不到时按类型策略决定能否新建）。
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum TermRef {
    Key { kind: Kind, key: String },
    Link { kind: Kind, system: String, key: String, url: String, name: Option<String> },
}

impl TermRef {
    pub fn kind(&self) -> Kind {
        match self {
            TermRef::Key { kind, .. } | TermRef::Link { kind, .. } => *kind,
        }
    }

    /// 判重用的身份：不含 url 和名字。
    fn ident(&self) -> String {
        match self {
            TermRef::Key { key, .. } => format!("key\n{key}"),
            TermRef::Link { system, key, .. } => format!("link\n{system}\n{key}"),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Score {
    pub value: f64,
    pub metric: String,
}

#[derive(Clone, Debug)]
pub enum Target {
    Image { asset: AssetRef, term: TermRef },
    Relation { kind: RelationKind, a: TermRef, b: TermRef },
}

#[derive(Clone, Debug)]
pub struct Row {
    pub line: usize,
    pub id: String,
    pub target: Target,
    pub source: String,
    pub method: String,
    pub version: String,
    pub score: Option<Score>,
    pub observed_at: String,
    pub data: Map<String, Value>,
}

/// terms.jsonl 的一行：产出者声明这个词条有哪些外部链接、叫什么。
#[derive(Clone, Debug)]
pub struct TermInfo {
    pub line: usize,
    pub term: TermRef,
    /// (system, key, url)
    pub links: Vec<(String, String, String)>,
    /// (lang, value, source)：同一来源的显示名跟随产出者，旧的被替换。
    pub names: Vec<(String, String, String)>,
}

#[derive(Clone, Debug)]
pub struct Package {
    pub manifest: Manifest,
    pub terms: Vec<TermInfo>,
    pub rows: Vec<Row>,
    /// 去掉 created_at 后的清单的 SHA-256；清单含全部文件的 SHA-256，所以它覆盖整个包的内容。
    pub fingerprint: String,
}

impl Package {
    /// 读一个包目录：核对清单、每个声明文件的大小和 SHA-256，再逐行校验证据。
    /// 只读取清单里声明的文件，不跟随符号链接；附件（例如 wd.npy）只核对，不解释。
    pub fn open(dir: &Path) -> Result<Package> {
        let manifest_text = read_small(&dir.join(MANIFEST_FILE), MAX_MANIFEST_BYTES)?;
        let manifest = parse_manifest(&manifest_text)?;
        for (name, spec) in &manifest.files {
            let path = dir.join(name);
            let meta = fs::symlink_metadata(&path).map_err(|e| Error::Invalid(format!("{name}：{e}")))?;
            if !meta.file_type().is_file() {
                return invalid(format!("{name} 不是普通文件"));
            }
            if meta.len() != spec.bytes {
                return invalid(format!(
                    "{name} 大小是 {size}，清单写的是 {listed}",
                    size = meta.len(),
                    listed = spec.bytes
                ));
            }
            let mut hasher = Sha256::new();
            let mut file = fs::File::open(&path)?;
            let mut buf = vec![0u8; 1 << 16];
            loop {
                let n = file.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                hasher.update(&buf[..n]);
            }
            if strict_json::hex(&hasher.finalize()) != spec.sha256 {
                return invalid(format!("{name} 的 SHA-256 与清单不符"));
            }
        }
        let terms = if manifest.files.contains_key(TERMS_FILE) {
            parse_terms(&manifest, BufReader::new(fs::File::open(dir.join(TERMS_FILE))?))?
        } else {
            Vec::new()
        };
        let reader = BufReader::new(fs::File::open(dir.join(EVIDENCE_FILE))?);
        let rows = parse_rows(&manifest, reader)?;
        Ok(Package { fingerprint: fingerprint(&manifest), manifest, terms, rows })
    }

    /// 从内存里的清单和证据文本组包（worker 走标准输出时用）；核对 evidence.jsonl 的大小和 SHA-256，不核对附件。
    pub fn from_parts(manifest_text: &str, evidence_text: &str) -> Result<Package> {
        Package::from_texts(manifest_text, evidence_text, None)
    }

    /// 同上，另带 terms.jsonl 的文本（清单声明了它就必须给）。
    pub fn from_texts(manifest_text: &str, evidence_text: &str, terms_text: Option<&str>) -> Result<Package> {
        let manifest = parse_manifest(manifest_text)?;
        let check = |name: &str, text: &str| {
            let spec = &manifest.files[name];
            if spec.bytes != text.len() as u64 || spec.sha256 != sha256_hex(text.as_bytes()) {
                return invalid(format!("{name} 与清单里的大小或 SHA-256 不符"));
            }
            Ok(())
        };
        check(EVIDENCE_FILE, evidence_text)?;
        let terms = match (manifest.files.contains_key(TERMS_FILE), terms_text) {
            (true, Some(text)) => {
                check(TERMS_FILE, text)?;
                parse_terms(&manifest, text.as_bytes())?
            }
            (false, None) => Vec::new(),
            _ => return invalid("terms.jsonl 与清单的声明不符"),
        };
        let rows = parse_rows(&manifest, evidence_text.as_bytes())?;
        Ok(Package { fingerprint: fingerprint(&manifest), manifest, terms, rows })
    }
}

fn fingerprint(manifest: &Manifest) -> String {
    let mut raw = manifest.raw.clone();
    raw.as_object_mut().expect("清单已校验为对象").remove("created_at");
    sha256_hex(canonical(&raw).as_bytes())
}

fn read_small(path: &Path, limit: u64) -> Result<String> {
    let meta = fs::symlink_metadata(path).map_err(|e| Error::Invalid(format!("{}：{e}", path.display())))?;
    if !meta.file_type().is_file() || meta.len() > limit {
        return invalid(format!("{path} 不是普通文件或超过 {limit} 字节", path = path.display()));
    }
    Ok(fs::read_to_string(path)?)
}

// ---- 字段工具 ----

fn object<'a>(v: &'a Value, at: &str, required: &[&str], optional: &[&str]) -> Result<&'a Map<String, Value>> {
    let Some(map) = v.as_object() else {
        return invalid(format!("{at} 应为对象"));
    };
    let missing: Vec<_> = required.iter().filter(|k| !map.contains_key(**k)).collect();
    let unknown: Vec<_> =
        map.keys().filter(|k| !required.contains(&k.as_str()) && !optional.contains(&k.as_str())).collect();
    if !missing.is_empty() || !unknown.is_empty() {
        return invalid(format!(
            "{at} 缺少字段 {missing}，未知字段 {unknown}",
            missing = format!("{missing:?}"),
            unknown = format!("{unknown:?}")
        ));
    }
    Ok(map)
}

pub fn valid_text(s: &str, max: usize) -> bool {
    !s.trim().is_empty() && s.len() <= max && !s.chars().any(char::is_control)
}

pub fn valid_token(s: &str) -> bool {
    (1..=64).contains(&s.len())
        && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_.-".contains(&b))
}

/// 非空、不全是空白、没有控制字符的文本，原样保留（资产 ID 的大小写和空格都不改）。
fn text(v: &Value, at: &str, max: usize) -> Result<String> {
    match v.as_str() {
        Some(s) if valid_text(s, max) => Ok(s.to_owned()),
        _ => invalid(format!("{at} 应为 1–{max} 字节的非空文本")),
    }
}

/// 小写标识：来源、方法、量纲、命名空间、链接系统。
fn token(v: &Value, at: &str) -> Result<String> {
    match v.as_str() {
        Some(s) if valid_token(s) => Ok(s.to_owned()),
        _ => invalid(format!("{at} 应为 1–64 位小写字母、数字或 _ . -")),
    }
}

fn opt_text(v: Option<&Value>, at: &str, max: usize) -> Result<Option<String>> {
    match v {
        None | Some(Value::Null) => Ok(None),
        Some(v) => text(v, at, max).map(Some),
    }
}

/// 带时区的 ISO 8601：YYYY-MM-DDTHH:MM:SS[.fff](Z|±HH:MM)。
pub fn is_timestamp(s: &str) -> bool {
    let b = s.as_bytes();
    let num = |r: std::ops::Range<usize>| -> Option<u32> {
        let part = b.get(r)?;
        if part.iter().all(u8::is_ascii_digit) {
            std::str::from_utf8(part).ok()?.parse().ok()
        } else {
            None
        }
    };
    let date_ok = b.len() >= 20
        && b[4] == b'-'
        && b[7] == b'-'
        && b[10] == b'T'
        && b[13] == b':'
        && b[16] == b':'
        && num(0..4).is_some()
        && matches!(num(5..7), Some(1..=12))
        && matches!(num(8..10), Some(1..=31))
        && matches!(num(11..13), Some(0..=23))
        && matches!(num(14..16), Some(0..=59))
        && matches!(num(17..19), Some(0..=60));
    if !date_ok {
        return false;
    }
    let mut rest = &b[19..];
    if rest.first() == Some(&b'.') {
        let digits = rest[1..].iter().take_while(|c| c.is_ascii_digit()).count();
        if digits == 0 {
            return false;
        }
        rest = &rest[1 + digits..];
    }
    match rest {
        [b'Z'] => true,
        [sign, h1, h2, b':', m1, m2] if (*sign == b'+' || *sign == b'-') => {
            let d = |c: &u8| c.is_ascii_digit();
            d(h1) && d(h2) && d(m1) && d(m2) && (h1 - b'0') * 10 + (h2 - b'0') <= 23 && (m1 - b'0') <= 5
        }
        _ => false,
    }
}

fn asset_ref(v: &Value, at: &str) -> Result<AssetRef> {
    let m = object(v, at, &["namespace", "id"], &[])?;
    Ok(AssetRef {
        namespace: token(&m["namespace"], &format!("{at}.namespace"))?,
        id: text(&m["id"], &format!("{at}.id"), 256)?,
    })
}

fn term_ref(v: &Value, at: &str) -> Result<TermRef> {
    let m = object(v, at, &["kind"], &["key", "link", "name"])?;
    let kind = match m["kind"].as_str().and_then(Kind::parse) {
        Some(k) => k,
        None => return invalid(format!("{at}.kind 不是已知的词条类型")),
    };
    match (m.get("key"), m.get("link")) {
        (Some(key), None) => {
            if m.contains_key("name") {
                return invalid(format!("{at}.name 只能和 link 一起给"));
            }
            Ok(TermRef::Key { kind, key: text(key, &format!("{at}.key"), 128)? })
        }
        (None, Some(link)) => {
            let at_link = format!("{at}.link");
            let l = object(link, &at_link, &["system", "key"], &["url"])?;
            Ok(TermRef::Link {
                kind,
                system: token(&l["system"], &format!("{at_link}.system"))?,
                key: text(&l["key"], &format!("{at_link}.key"), 256)?,
                url: opt_text(l.get("url"), &format!("{at_link}.url"), 2048)?.unwrap_or_default(),
                name: opt_text(m.get("name"), &format!("{at}.name"), 200)?,
            })
        }
        _ => invalid(format!("{at} 需要 key 或 link 二者之一")),
    }
}

pub fn parse_manifest(text_in: &str) -> Result<Manifest> {
    let raw = strict_json::parse(text_in).map_err(|e| Error::Invalid(format!("manifest.json：{e}")))?;
    let m =
        object(&raw, "manifest", &["format", "library_id", "producer", "created_at", "assets", "files"], &["models"])?;
    if m["format"] != FORMAT {
        return invalid(format!("manifest.format 应为 {FORMAT}"));
    }
    let producer = object(&m["producer"], "manifest.producer", &["name", "version"], &[])?;
    let created_at = text(&m["created_at"], "manifest.created_at", 64)?;
    if !is_timestamp(&created_at) {
        return invalid("manifest.created_at 应为带时区的 ISO 8601 时间");
    }
    let models = match m.get("models") {
        None => Map::new(),
        Some(Value::Object(models)) => {
            for (name, spec) in models {
                if !spec.is_object() {
                    return invalid(format!("manifest.models.{name} 应为对象"));
                }
            }
            models.clone()
        }
        Some(_) => return invalid("manifest.models 应为对象"),
    };
    let Some(list) = m["assets"].as_array() else {
        return invalid("manifest.assets 应为数组");
    };
    if list.len() > MAX_ASSETS {
        return invalid(format!("一个包最多 {MAX_ASSETS} 张图"));
    }
    let mut seen = HashSet::new();
    let mut assets = Vec::with_capacity(list.len());
    for (i, item) in list.iter().enumerate() {
        let at = format!("manifest.assets[{i}]");
        let o = object(item, &at, &["asset", "status", "error"], &["task_id"])?;
        let asset = asset_ref(&o["asset"], &format!("{at}.asset"))?;
        if !seen.insert(asset.clone()) {
            return invalid(format!("{at} 重复：{namespace}:{id}", namespace = asset.namespace, id = asset.id));
        }
        let status = match o["status"].as_str() {
            Some("completed") => Status::Completed,
            Some("failed") => Status::Failed,
            Some("cancelled") => Status::Cancelled,
            _ => return invalid(format!("{at}.status 应为 completed、failed 或 cancelled")),
        };
        let error = match (&o["error"], status) {
            (Value::Null, Status::Completed) => None,
            (_, Status::Completed) => return invalid(format!("{at} 已完成就不能带 error")),
            (e, _) => {
                let e = object(e, &format!("{at}.error"), &["code", "message"], &[])?;
                Some((
                    token(&e["code"], &format!("{at}.error.code"))?,
                    text(&e["message"], &format!("{at}.error.message"), 2000)?,
                ))
            }
        };
        let task_id = opt_text(o.get("task_id"), &format!("{at}.task_id"), 128)?;
        assets.push(AssetOutcome { asset, task_id, status, error });
    }
    let Some(file_map) = m["files"].as_object() else {
        return invalid("manifest.files 应为对象");
    };
    let mut files = BTreeMap::new();
    for (name, spec) in file_map {
        let ok_name = (1..=64).contains(&name.len())
            && !name.starts_with('.')
            && name != MANIFEST_FILE
            && name.bytes().all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b));
        if !ok_name {
            return invalid(format!("manifest.files 里的文件名不合法：{name}"));
        }
        let at = format!("manifest.files.{name}");
        let s = object(spec, &at, &["bytes", "sha256"], &[])?;
        let bytes = match s["bytes"].as_u64() {
            Some(n) if n <= MAX_FILE_BYTES => n,
            _ => return invalid(format!("{at}.bytes 应为不超过 1 GiB 的非负整数")),
        };
        let sha256 = match s["sha256"].as_str() {
            Some(h) if h.len() == 64 && h.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) => {
                h.to_owned()
            }
            _ => return invalid(format!("{at}.sha256 应为 64 位小写十六进制")),
        };
        files.insert(name.clone(), FileSpec { bytes, sha256 });
    }
    if !files.contains_key(EVIDENCE_FILE) {
        return invalid("manifest.files 必须声明 evidence.jsonl");
    }
    Ok(Manifest {
        library_id: text(&m["library_id"], "manifest.library_id", 128)?,
        producer: token(&producer["name"], "manifest.producer.name")?,
        producer_version: text(&producer["version"], "manifest.producer.version", 128)?,
        created_at,
        models,
        assets,
        files,
        raw: raw.clone(),
    })
}

/// 逐行读 JSONL：限长、UTF-8、不允许空行；回调拿到 (行号, 文本)，错误带上文件名和行号。
fn each_line(file: &str, reader: impl BufRead, mut f: impl FnMut(usize, &str) -> Result<()>) -> Result<()> {
    for (i, line) in reader.split(b'\n').enumerate() {
        let n = i + 1;
        let line = line?;
        if line.len() > MAX_LINE_BYTES {
            return invalid(format!("{file} 第 {n} 行超过 {MAX_LINE_BYTES} 字节"));
        }
        let Ok(line) = std::str::from_utf8(&line) else {
            return invalid(format!("{file} 第 {n} 行不是 UTF-8"));
        };
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() {
            return invalid(format!("{file} 第 {n} 行是空行"));
        }
        f(n, line).map_err(|e| match e {
            Error::Invalid(m) if !m.starts_with(file) => Error::Invalid(format!("{file} 第 {n} 行：{m}")),
            other => other,
        })?;
    }
    Ok(())
}

fn parse_terms(_manifest: &Manifest, reader: impl BufRead) -> Result<Vec<TermInfo>> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    each_line(TERMS_FILE, reader, |n, line| {
        let v = strict_json::parse(line).map_err(Error::Invalid)?;
        let m = object(&v, "词条", &["term"], &["links", "names"])?;
        let term = term_ref(&m["term"], "term")?;
        if !seen.insert(term.ident()) {
            return invalid("同一个词条描述了两次");
        }
        let mut links = Vec::new();
        for (i, l) in m
            .get("links")
            .map(|x| x.as_array().ok_or(()))
            .transpose()
            .map_err(|_| Error::Invalid("links 应为数组".into()))?
            .into_iter()
            .flatten()
            .enumerate()
        {
            let at = format!("links[{i}]");
            let o = object(l, &at, &["system", "key"], &["url"])?;
            links.push((
                token(&o["system"], &format!("{at}.system"))?,
                text(&o["key"], &format!("{at}.key"), 256)?,
                opt_text(o.get("url"), &format!("{at}.url"), 2048)?.unwrap_or_default(),
            ));
        }
        let mut names = Vec::new();
        for (i, x) in m
            .get("names")
            .map(|x| x.as_array().ok_or(()))
            .transpose()
            .map_err(|_| Error::Invalid("names 应为数组".into()))?
            .into_iter()
            .flatten()
            .enumerate()
        {
            let at = format!("names[{i}]");
            let o = object(x, &at, &["value"], &["lang", "source"])?;
            let lang = match o.get("lang").and_then(Value::as_str) {
                None => String::new(),
                Some(l @ ("" | "zh" | "ja" | "en")) => l.to_owned(),
                Some(_) => return invalid(format!("{at}.lang 只能是空、zh、ja、en")),
            };
            let source = match o.get("source") {
                None => String::new(),
                Some(s) => token(s, &format!("{at}.source"))?,
            };
            names.push((lang, text(&o["value"], &format!("{at}.value"), 200)?, source));
        }
        out.push(TermInfo { line: n, term, links, names });
        Ok(())
    })?;
    Ok(out)
}

fn parse_rows(manifest: &Manifest, reader: impl BufRead) -> Result<Vec<Row>> {
    let status: HashMap<&AssetRef, Status> = manifest.assets.iter().map(|a| (&a.asset, a.status)).collect();
    let mut ids = HashSet::new();
    let mut claims = HashSet::new();
    let mut rows = Vec::new();
    each_line(EVIDENCE_FILE, reader, |n, line| {
        let row = parse_row(line, n).map_err(|e| match e {
            Error::Invalid(m) => Error::Invalid(format!("evidence.jsonl 第 {n} 行：{m}")),
            other => other,
        })?;
        if !ids.insert(row.id.clone()) {
            return invalid(format!("evidence.jsonl 第 {n} 行：证据 id 重复：{id}", id = row.id));
        }
        let target = match &row.target {
            Target::Image { asset, term } => {
                match status.get(asset) {
                    Some(Status::Completed) => {}
                    Some(_) => return invalid(format!("evidence.jsonl 第 {n} 行：失败或取消的图不能发布证据")),
                    None => return invalid(format!("evidence.jsonl 第 {n} 行：图不在 manifest.assets 里")),
                }
                format!("image\n{}\n{}\n{}", asset.namespace, asset.id, term.ident())
            }
            Target::Relation { kind, a, b } => {
                let (a, b) = (a.ident(), b.ident());
                if a == b {
                    return invalid(format!("evidence.jsonl 第 {n} 行：关系两端是同一个词条"));
                }
                let (a, b) = if kind.symmetric() && b < a { (b, a) } else { (a, b) };
                format!("relation\n{}\n{a}\n{b}", kind.as_str())
            }
        };
        if !claims.insert(format!("{target}\n{}\n{}\n{}", row.source, row.method, row.version)) {
            return invalid(format!("evidence.jsonl 第 {n} 行：同一目标、来源、方法、版本出现两次"));
        }
        rows.push(row);
        Ok(())
    })?;
    Ok(rows)
}

/// 解析一行证据；不核对它属于哪张图（由 parse_rows 按清单核对）。
pub fn parse_row(line: &str, n: usize) -> Result<Row> {
    let v = strict_json::parse(line).map_err(Error::Invalid)?;
    let m = object(
        &v,
        "证据",
        &["id", "source", "method", "version", "score", "observed_at", "data"],
        &["asset", "term", "relation"],
    )?;
    let target = match (m.get("asset"), m.get("term"), m.get("relation")) {
        (Some(asset), Some(term), None) => {
            Target::Image { asset: asset_ref(asset, "asset")?, term: term_ref(term, "term")? }
        }
        (None, None, Some(rel)) => {
            let r = object(rel, "relation", &["kind", "a", "b"], &[])?;
            let Some(kind) = r["kind"].as_str().and_then(RelationKind::parse) else {
                return invalid("relation.kind 应为 contains、same_as、different_from 或 excludes");
            };
            Target::Relation { kind, a: term_ref(&r["a"], "relation.a")?, b: term_ref(&r["b"], "relation.b")? }
        }
        _ => return invalid("需要 asset+term，或 relation，二者之一"),
    };
    let source = token(&m["source"], "source")?;
    if RESERVED_SOURCES.contains(&source.as_str()) {
        return invalid(format!("来源 {source} 保留给人工裁决，证据包不能使用"));
    }
    let score = match &m["score"] {
        Value::Null => None,
        s => {
            let s = object(s, "score", &["value", "metric"], &[])?;
            let Some(value) = s["value"].as_f64() else {
                return invalid("score.value 应为有限数值");
            };
            Some(Score { value, metric: token(&s["metric"], "score.metric")? })
        }
    };
    let observed_at = text(&m["observed_at"], "observed_at", 64)?;
    if !is_timestamp(&observed_at) {
        return invalid("observed_at 应为带时区的 ISO 8601 时间");
    }
    let Some(data) = m["data"].as_object() else {
        return invalid("data 应为对象");
    };
    if canonical(&m["data"]).len() > MAX_DATA_BYTES {
        return invalid(format!("data 超过 {MAX_DATA_BYTES} 字节"));
    }
    Ok(Row {
        line: n,
        id: text(&m["id"], "id", 128)?,
        target,
        source,
        method: token(&m["method"], "method")?,
        version: text(&m["version"], "version", 128)?,
        score,
        observed_at,
        data: data.clone(),
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;

    /// 按内容补齐 files 里 evidence.jsonl 的大小和摘要，返回 (清单文本, 证据文本)。
    pub fn build(manifest: Value, rows: &[Value]) -> (String, String) {
        let evidence: String = rows.iter().map(|r| canonical(r) + "\n").collect();
        (seal(manifest, &evidence), evidence)
    }

    pub fn seal(mut manifest: Value, evidence: &str) -> String {
        manifest["files"]["evidence.jsonl"] =
            json!({"bytes": evidence.len(), "sha256": sha256_hex(evidence.as_bytes())});
        canonical(&manifest)
    }

    pub fn manifest() -> Value {
        json!({
            "format": FORMAT,
            "library_id": "lib-1",
            "producer": {"name": "provenance", "version": "0.4"},
            "created_at": "2026-09-28T10:00:00+08:00",
            "assets": [
                {"asset": {"namespace": "photos", "id": "A-1"}, "task_id": "t1", "status": "completed", "error": null},
                {"asset": {"namespace": "photos", "id": "A-2"}, "status": "failed", "error": {"code": "network_refused", "message": "403"}}
            ],
            "files": {}
        })
    }

    pub fn row(id: &str, method: &str) -> Value {
        json!({
            "id": id,
            "asset": {"namespace": "photos", "id": "A-1"},
            "term": {"kind": "artist", "link": {"system": "danbooru", "key": "some_artist"}, "name": "Some Artist"},
            "source": "provenance", "method": method, "version": "1",
            "score": {"value": 0, "metric": "hamming_distance"},
            "observed_at": "2026-09-28T02:00:00Z",
            "data": {"post_id": "123"}
        })
    }

    fn check(manifest: Value, rows: &[Value]) -> Result<Package> {
        let (m, e) = build(manifest, rows);
        Package::from_parts(&m, &e)
    }

    fn err(manifest: Value, rows: &[Value]) -> String {
        check(manifest, rows).unwrap_err().to_string()
    }

    #[test]
    fn accepts_a_well_formed_package() {
        let relation = json!({
            "id": "r1",
            "relation": {"kind": "contains",
                         "a": {"kind": "character", "link": {"system": "danbooru", "key": "mika_(blue_archive)"}},
                         "b": {"kind": "skin", "link": {"system": "danbooru", "key": "mika_(swimsuit)_(blue_archive)"}}},
            "source": "danbooru", "method": "implication", "version": "2026-09", "score": null,
            "observed_at": "2026-09-28T02:00:00.123+09:00", "data": {}
        });
        let p = check(manifest(), &[row("e1", "pixel_hash"), row("e2", "ocr"), relation]).unwrap();
        assert_eq!(p.rows.len(), 3);
        assert_eq!(p.manifest.assets[1].status, Status::Failed);
        assert_eq!(p.fingerprint.len(), 64);
    }

    #[test]
    fn fingerprint_ignores_created_at_only() {
        let a = check(manifest(), &[row("e1", "pixel_hash")]).unwrap();
        let mut m = manifest();
        m["created_at"] = json!("2026-09-29T00:00:00Z");
        assert_eq!(a.fingerprint, check(m, &[row("e1", "pixel_hash")]).unwrap().fingerprint);
        assert_ne!(a.fingerprint, check(manifest(), &[row("e1", "gallery_match")]).unwrap().fingerprint);
    }

    #[test]
    fn rejects_structural_problems() {
        let mut m = manifest();
        m["format"] = json!("illustash-recognition/1");
        assert!(err(m, &[]).contains("format"));
        let mut m = manifest();
        m["extra"] = json!(1);
        assert!(err(m, &[]).contains("未知字段"));
        let mut m = manifest();
        m["assets"][1]["asset"]["id"] = json!("A-1");
        assert!(err(m, &[]).contains("重复"));
        let mut m = manifest();
        m["assets"][1]["error"] = json!(null);
        assert!(check(m, &[]).is_err(), "失败必须带错误");
        let mut m = manifest();
        m["assets"][0]["error"] = json!({"code": "x", "message": "y"});
        assert!(check(m, &[]).is_err(), "完成不能带错误");
        let mut m = manifest();
        m["created_at"] = json!("2026-09-28 10:00:00");
        assert!(check(m, &[]).is_err(), "时间要带时区");
    }

    #[test]
    fn rejects_bad_rows() {
        let mut r = row("e1", "pixel_hash");
        r["source"] = json!("user");
        assert!(err(manifest(), &[r]).contains("保留给人工"));
        let mut r = row("e1", "pixel_hash");
        r["verdict"] = json!("yes");
        assert!(err(manifest(), &[r]).contains("未知字段"));
        let mut r = row("e1", "pixel_hash");
        r["asset"]["id"] = json!("A-2");
        assert!(err(manifest(), &[r]).contains("失败或取消"));
        let mut r = row("e1", "pixel_hash");
        r["asset"]["id"] = json!("a-1");
        assert!(err(manifest(), &[r]).contains("不在 manifest"), "资产 ID 区分大小写");
        assert!(err(manifest(), &[row("e1", "pixel_hash"), row("e1", "ocr")]).contains("id 重复"));
        assert!(err(manifest(), &[row("e1", "pixel_hash"), row("e2", "pixel_hash")]).contains("出现两次"));
        let mut r = row("e1", "pixel_hash");
        r["term"]["kind"] = json!("tag");
        assert!(err(manifest(), &[r]).contains("词条类型"));
        let mut r = row("e1", "pixel_hash");
        r["term"]["key"] = json!("k");
        assert!(err(manifest(), &[r]).contains("二者之一"));
        let mut r = row("e1", "pixel_hash");
        r["score"] = json!({"value": "0.9", "metric": "p"});
        assert!(check(manifest(), &[r]).is_err());
        let mut r = row("e1", "pixel_hash");
        r["relation"] =
            json!({"kind": "contains", "a": {"kind": "work", "key": "w"}, "b": {"kind": "work", "key": "w"}});
        assert!(check(manifest(), &[r]).is_err(), "不能同时有 asset 和 relation");
    }

    #[test]
    fn rejects_relation_to_itself_and_symmetric_duplicates() {
        let rel = |a: &str, b: &str, id: &str| {
            json!({"id": id, "relation": {"kind": "different_from", "a": {"kind": "character", "key": a}, "b": {"kind": "character", "key": b}},
                   "source": "ccip", "method": "pair", "version": "1", "score": null, "observed_at": "2026-09-28T00:00:00Z", "data": {}})
        };
        assert!(err(manifest(), &[rel("x", "x", "1")]).contains("同一个词条"));
        assert!(err(manifest(), &[rel("x", "y", "1"), rel("y", "x", "2")]).contains("出现两次"));
    }

    #[test]
    fn rejects_duplicate_json_keys_and_tampered_evidence() {
        let (m, e) = build(manifest(), &[row("e1", "pixel_hash")]);
        let dup = e.replacen("\"id\":\"e1\"", "\"id\":\"e1\",\"id\":\"e2\"", 1);
        let err = Package::from_parts(&seal(manifest(), &dup), &dup).unwrap_err().to_string();
        assert!(err.contains("重复字段") && err.contains("第 1 行"), "{err}");
        let tampered = e.replace("123", "124");
        assert!(Package::from_parts(&m, &tampered).unwrap_err().to_string().contains("SHA-256"));
        let blank = format!("{e}\n");
        assert!(Package::from_parts(&seal(manifest(), &blank), &blank).unwrap_err().to_string().contains("空行"));
    }

    #[test]
    fn timestamps() {
        for ok in ["2026-09-28T00:00:00Z", "2026-09-28T23:59:60.5+08:00", "2026-01-01T00:00:00-05:30"] {
            assert!(is_timestamp(ok), "{ok}");
        }
        for bad in [
            "2026-09-28",
            "2026-09-28T00:00:00",
            "2026-13-01T00:00:00Z",
            "2026-09-28T00:00:00.Z",
            "2026-09-28T00:00:00+8:00",
        ] {
            assert!(!is_timestamp(bad), "{bad}");
        }
    }
}
