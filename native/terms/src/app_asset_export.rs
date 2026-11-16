//! 选中的图导出到文件夹（#450）：复制原图（照片来源本机有原图就复制原图，否则复制派生图），可按角色或画师建子文件夹。
//!
//! 只读原图，用 `create_new` 写到用户选的文件夹，从不覆盖已有文件；写到一半出错或取消，删掉这个写了一半的文件。
//! 同名文件内容相同就跳过（算「已存在」），不同就在扩展名前加编号。一个会话同一时间只跑一个导出，换库时停掉。
use crate::{
    app_asset_files::{safe_component, Located, Lookup},
    app_library::Library,
    http_json::Json,
    Error, Result,
};
use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Value};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
};

/// 子文件夹怎么分。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Group {
    None,
    Character,
    Artist,
}
impl Group {
    pub(crate) fn parse(value: &str) -> Result<Self> {
        match value {
            "" | "none" => Ok(Self::None),
            "character" => Ok(Self::Character),
            "artist" => Ok(Self::Artist),
            _ => Err(Error::invalid("子文件夹的分法不正确")),
        }
    }
    /// 没有角色（画师）的图放进哪个子文件夹：中文原文，调用方按页面语言翻译后传给 `Exports::start`。
    pub(crate) fn fallback(self) -> &'static str {
        match self {
            Self::None => "",
            Self::Character => "未识别角色",
            Self::Artist => "画师未知",
        }
    }
}
/// 子文件夹名最多几个字符。
const FOLDER_CHARS: usize = 80;
/// 文件名最多几个字符（含扩展名）。
const FILE_CHARS: usize = 150;

struct Task {
    state: Arc<Mutex<Value>>,
    cancel: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    generation: u64,
    destination: PathBuf,
}

#[derive(Default)]
pub(crate) struct Exports(Mutex<Option<Task>>);
impl Exports {
    /// 这个会话最近一次导出的状态；没有时是 `{"status":"idle"}`。
    pub(crate) fn state(&self, generation: u64) -> Json {
        let task = self.0.lock().expect("app_asset_export task lock poisoned");
        let value = task
            .as_ref()
            .filter(|t| t.generation == generation)
            .map(|t| t.state.lock().expect("app_asset_export state lock poisoned").clone())
            .unwrap_or_else(|| json!({"status": "idle"}));
        value.into()
    }
    /// 导出到的文件夹（任务自己记下的，不来自页面）。
    pub(crate) fn destination(&self, generation: u64) -> Option<PathBuf> {
        let task = self.0.lock().expect("app_asset_export task lock poisoned");
        task.as_ref().filter(|t| t.generation == generation).map(|t| t.destination.clone())
    }
    pub(crate) fn running(&self) -> bool {
        let task = self.0.lock().expect("app_asset_export task lock poisoned");
        task.as_ref().is_some_and(|t| t.handle.as_ref().is_some_and(|h| !h.is_finished()))
    }
    pub(crate) fn start(
        &self,
        library: &Library,
        generation: u64,
        ids: Vec<String>,
        group: Group,
        fallback: String,
        destination: PathBuf,
    ) -> Result<Json> {
        if ids.is_empty() {
            return Err(Error::invalid("请选择图片"));
        }
        if !destination.is_absolute() || !destination.is_dir() {
            return Err(Error::invalid("导出到的文件夹不在了"));
        }
        let mut task = self.0.lock().expect("app_asset_export task lock poisoned");
        if task.as_ref().is_some_and(|t| t.handle.as_ref().is_some_and(|h| !h.is_finished())) {
            return Err(Error::invalid("正在导出，完成后再开始下一次"));
        }
        if let Some(mut old) = task.take() {
            if let Some(handle) = old.handle.take() {
                let _ = handle.join();
            }
        }
        let initial = json!({"status": "running", "done": 0, "total": ids.len(), "group": group_name(group)});
        let state = Arc::new(Mutex::new(initial.clone()));
        let cancel = Arc::new(AtomicBool::new(false));
        let (s, c, lib, dest) = (state.clone(), cancel.clone(), library.clone(), destination.clone());
        let handle = thread::Builder::new().name("asset-export".into()).spawn(move || {
            let report = |done: usize, counts: &Counts| {
                let mut state = s.lock().expect("app_asset_export state lock poisoned");
                state["done"] = done.into();
                counts.write(&mut state);
            };
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run(&lib, &ids, (group, &fallback), &dest, &c, &report)
            }));
            let result = result.unwrap_or_else(|_| Err((Error::invalid("导出任务异常退出"), Counts::default())));
            let mut state = s.lock().expect("app_asset_export state lock poisoned");
            match result {
                Ok(counts) => {
                    state["status"] = if c.load(Ordering::SeqCst) { "cancelled" } else { "done" }.into();
                    counts.write(&mut state);
                }
                Err((e, counts)) => {
                    state["status"] = "error".into();
                    state["error"] = e.reason().chars().take(200).collect::<String>().into();
                    counts.write(&mut state);
                }
            }
        })?;
        *task = Some(Task { state, cancel, handle: Some(handle), generation, destination });
        Ok(initial.into())
    }
    pub(crate) fn cancel(&self) -> Json {
        if let Some(task) = self.0.lock().expect("app_asset_export task lock poisoned").as_ref() {
            task.cancel.store(true, Ordering::SeqCst);
        }
        json!({"cancelled": true}).into()
    }
    /// 换库、关闭或发现损坏时：停下并等它收尾（只差手上这一个文件）。
    pub(crate) fn stop(&self) {
        let taken = self.0.lock().expect("app_asset_export task lock poisoned").take();
        if let Some(mut task) = taken {
            task.cancel.store(true, Ordering::SeqCst);
            if let Some(handle) = task.handle.take() {
                let _ = handle.join();
            }
        }
    }
}
impl Drop for Exports {
    fn drop(&mut self) {
        self.stop();
    }
}
fn group_name(group: Group) -> &'static str {
    match group {
        Group::None => "none",
        Group::Character => "character",
        Group::Artist => "artist",
    }
}

#[derive(Default, Debug, PartialEq, Eq)]
struct Counts {
    /// 写出的文件数（一张图进了几个子文件夹就算几个）。
    copied: usize,
    /// 目标里已有同样内容的文件，没再写。
    existed: usize,
    /// 原文件读不到的图（被移走、在 iCloud 里没下载）。
    failed: usize,
    /// 照片来源里本机没有原图、导出了派生图的张数。
    derived: usize,
}
impl Counts {
    fn write(&self, state: &mut Value) {
        state["copied"] = self.copied.into();
        state["existed"] = self.existed.into();
        state["failed"] = self.failed.into();
        state["derived"] = self.derived.into();
    }
}

/// 名字查询用的只读连接：应用库，挂上溯源库（画师）。
fn names_connection(library: &Library) -> Result<Connection> {
    let c =
        crate::sqlite::open_with_flags(library.db(), OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI)?;
    c.busy_timeout(std::time::Duration::from_secs(10))?;
    crate::http_read::register_names(&c)?;
    let provenance = library.instance_path().join("provenance.sqlite");
    if provenance.exists() {
        c.execute("ATTACH DATABASE ? AS prov", [crate::http_read::sqlite_uri(&provenance)])?;
    }
    Ok(c)
}
/// 一张图要放进哪些子文件夹（不分时是图片所在的文件夹本身）。
/// `fallback` 是没有角色（画师）时的子文件夹名（已按页面语言翻译）。
fn folders(c: &Connection, (group, fallback): (Group, &str), id: &str) -> Result<Vec<String>> {
    let (names, none) = match group {
        Group::None => return Ok(vec![String::new()]),
        Group::Character => (crate::http_browse::character_names(c, id)?, fallback),
        Group::Artist => {
            let artists = crate::http_artists::for_asset(c, true, id, None, false)?;
            let names = match artists.get("artists") {
                Some(Json::Array(list)) => list.iter().map(|a| a.field("name").text().to_owned()).collect(),
                _ => Vec::new(),
            };
            (names, fallback)
        }
    };
    let mut out: Vec<String> = Vec::new();
    for name in names {
        let folder = safe_component(&name, none, FOLDER_CHARS);
        // 不同的名字清理后可能相同（如 a/b 和 a:b），同一张图在一个文件夹里只放一份。
        if !out.iter().any(|f| f.eq_ignore_ascii_case(&folder)) {
            out.push(folder);
        }
    }
    if out.is_empty() {
        out.push(none.to_owned());
    }
    Ok(out)
}

type Failure = (Error, Counts);
fn run(
    library: &Library,
    ids: &[String],
    group: (Group, &str),
    destination: &Path,
    cancel: &AtomicBool,
    report: &dyn Fn(usize, &Counts),
) -> std::result::Result<Counts, Failure> {
    let mut counts = Counts::default();
    let setup = (|| Ok::<_, Error>((Lookup::new(library)?, names_connection(library)?)))();
    let (lookup, names) = match setup {
        Ok(v) => v,
        Err(e) => return Err((e, counts)),
    };
    for (i, id) in ids.iter().enumerate() {
        if cancel.load(Ordering::SeqCst) {
            break;
        }
        let located = match lookup.locate(id) {
            Ok(located) => located,
            Err(_) => {
                counts.failed += 1;
                report(i + 1, &counts);
                continue;
            }
        };
        let targets = match folders(&names, group, id) {
            Ok(targets) => targets,
            Err(e) => return Err((e, counts)),
        };
        if let Err(e) = export_one(&located, &targets, destination, cancel, &mut counts) {
            return Err((e, counts));
        }
        report(i + 1, &counts);
    }
    Ok(counts)
}
/// 一张图复制进它的每个子文件夹。原图读不到算失败、接着做下一张；写目标出错（没有权限、磁盘满）整个导出停下。
fn export_one(
    located: &Located,
    folders: &[String],
    destination: &Path,
    cancel: &AtomicBool,
    counts: &mut Counts,
) -> Result<()> {
    let (source, name, derived) = located.original();
    let name = safe_component(&name, "image", FILE_CHARS);
    let mut readable = File::open(&source).is_ok();
    for folder in folders {
        if !readable || cancel.load(Ordering::SeqCst) {
            break;
        }
        let directory = if folder.is_empty() { destination.to_owned() } else { destination.join(folder) };
        fs::create_dir_all(&directory).map_err(|e| Error::invalid(format!("无法建立文件夹：{e}")))?;
        match place(&source, &directory, &name, cancel) {
            Ok(Placed::Copied) => counts.copied += 1,
            Ok(Placed::Existed) => counts.existed += 1,
            Ok(Placed::Cancelled) => return Ok(()),
            Err(Problem::Source) => readable = false,
            Err(Problem::Target(e)) => return Err(Error::invalid(format!("写入导出文件夹失败：{e}"))),
        }
    }
    if !readable {
        counts.failed += 1;
    } else if derived {
        counts.derived += 1;
    }
    Ok(())
}

enum Placed {
    Copied,
    Existed,
    Cancelled,
}
enum Problem {
    Source,
    Target(io::Error),
}
/// `name (n).ext`：重名时加的编号。
fn numbered(name: &str, n: usize) -> String {
    if n == 1 {
        return name.to_owned();
    }
    match name.rfind('.').filter(|&i| i > 0) {
        Some(i) => format!("{} ({n}){}", &name[..i], &name[i..]),
        None => format!("{name} ({n})"),
    }
}
/// 把 `source` 复制进 `directory`，起名 `name`；已有同名文件时内容相同就跳过，不同就换个编号。从不覆盖。
fn place(source: &Path, directory: &Path, name: &str, cancel: &AtomicBool) -> std::result::Result<Placed, Problem> {
    let size = fs::metadata(source).map_err(|_| Problem::Source)?.len();
    for n in 1..10_000 {
        let target = directory.join(numbered(name, n));
        match OpenOptions::new().write(true).create_new(true).open(&target) {
            Ok(file) => return copy_into(source, file, &target, cancel),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                if same_content(source, &target, size).map_err(Problem::Target)? {
                    return Ok(Placed::Existed);
                }
            }
            Err(e) => return Err(Problem::Target(e)),
        }
    }
    Err(Problem::Target(io::Error::other("同名文件太多")))
}
/// 复制完把修改时间设成和原图一样。失败或取消时删掉写了一半的文件。
fn copy_into(
    source: &Path,
    mut file: File,
    target: &Path,
    cancel: &AtomicBool,
) -> std::result::Result<Placed, Problem> {
    let result = (|| {
        let mut from = File::open(source).map_err(|_| Problem::Source)?;
        let mut buffer = vec![0u8; 1 << 20];
        loop {
            if cancel.load(Ordering::SeqCst) {
                return Ok(Placed::Cancelled);
            }
            let n = from.read(&mut buffer).map_err(|_| Problem::Source)?;
            if n == 0 {
                break;
            }
            file.write_all(&buffer[..n]).map_err(Problem::Target)?;
        }
        file.flush().map_err(Problem::Target)?;
        if let Ok(modified) = fs::metadata(source).and_then(|m| m.modified()) {
            let _ = file.set_modified(modified);
        }
        Ok(Placed::Copied)
    })();
    drop(file);
    if !matches!(result, Ok(Placed::Copied)) {
        let _ = fs::remove_file(target);
    }
    result
}
/// `target` 是普通文件，大小和内容都与 `source` 相同。
fn same_content(source: &Path, target: &Path, size: u64) -> io::Result<bool> {
    let meta = fs::metadata(target)?;
    if !meta.is_file() || meta.len() != size {
        return Ok(false);
    }
    let (Ok(mut a), mut b) = (File::open(source), File::open(target)?) else { return Ok(false) };
    let (mut x, mut y) = (vec![0u8; 1 << 16], vec![0u8; 1 << 16]);
    loop {
        let n = a.read(&mut x)?;
        if n == 0 {
            return Ok(true);
        }
        b.read_exact(&mut y[..n])?;
        if x[..n] != y[..n] {
            return Ok(false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("illustash-export-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn placing_never_overwrites_and_skips_identical_files() {
        let dir = scratch("place");
        let (source, other) = (dir.join("a.jpg"), dir.join("b.jpg"));
        fs::write(&source, b"original bytes").unwrap();
        fs::write(&other, b"different bytes").unwrap();
        let out = dir.join("out");
        fs::create_dir_all(&out).unwrap();
        let cancel = AtomicBool::new(false);
        assert!(matches!(place(&source, &out, "a.jpg", &cancel), Ok(Placed::Copied)));
        assert!(matches!(place(&source, &out, "a.jpg", &cancel), Ok(Placed::Existed)));
        // 同名、内容不同：加编号，原来那个不动。
        assert!(matches!(place(&other, &out, "a.jpg", &cancel), Ok(Placed::Copied)));
        assert_eq!(fs::read(out.join("a.jpg")).unwrap(), b"original bytes");
        assert_eq!(fs::read(out.join("a (2).jpg")).unwrap(), b"different bytes");
        assert!(matches!(place(&other, &out, "a.jpg", &cancel), Ok(Placed::Existed)));
        // 原图不变，修改时间跟原图一样。
        assert_eq!(fs::read(&source).unwrap(), b"original bytes");
        let modified = |p: &Path| fs::metadata(p).unwrap().modified().unwrap();
        assert_eq!(modified(&out.join("a.jpg")), modified(&source));
        // 取消时不留下写了一半的文件。
        cancel.store(true, Ordering::SeqCst);
        assert!(matches!(place(&source, &out, "c.jpg", &cancel), Ok(Placed::Cancelled)));
        assert!(!out.join("c.jpg").exists());
        assert!(matches!(place(&dir.join("missing.jpg"), &out, "m.jpg", &cancel), Err(Problem::Source)));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn numbering_goes_before_the_extension() {
        assert_eq!(numbered("a.jpg", 1), "a.jpg");
        assert_eq!(numbered("a.b.jpg", 3), "a.b (3).jpg");
        assert_eq!(numbered(".hidden", 2), ".hidden (2)");
        assert_eq!(numbered("noext", 2), "noext (2)");
    }

    /// 合成图库：文件夹来源的 a、b（b 的文件不在了）和照片来源的 p（没有 Photos.sqlite，只能用派生图）。
    fn library(dir: &Path) -> Library {
        let package = dir.join("synthetic.pilib");
        fs::create_dir_all(&package).unwrap();
        let photos = dir.join("Synthetic.photoslibrary");
        fs::write(
            package.join("library.json"),
            serde_json::to_string(&json!({"format": 1, "id": "synthetic", "name": "合成",
                "sources": [{"id": "s1", "kind": "folder", "path": dir.join("art")}, {"id": "s2", "kind": "photos", "path": photos}]}))
            .unwrap(),
        )
        .unwrap();
        fs::create_dir_all(dir.join("art")).unwrap();
        fs::write(dir.join("art/a.png"), b"synthetic a").unwrap();
        fs::create_dir_all(photos.join("resources/derivatives/P")).unwrap();
        let derivative = photos.join("resources/derivatives/P/P_1_105_c.jpeg");
        fs::write(&derivative, b"synthetic p").unwrap();
        let rows = [
            ("a", "a.png", dir.join("art/a.png"), "s1"),
            ("b", "b.png", dir.join("art/b.png"), "s1"),
            ("p", "P.heic", derivative, "s2"),
        ];
        let db = crate::sqlite::open(package.join("library.sqlite")).unwrap();
        db.execute_batch(&format!("CREATE TABLE meta(key TEXT PRIMARY KEY,value TEXT); INSERT INTO meta VALUES ('schema_version','36'),('terms_schema','{v}'),('terms_artists','core'),('terms_labels','core'),('terms_characters','core'),('terms_names','core'); CREATE TABLE prefs(key TEXT PRIMARY KEY,value TEXT NOT NULL); CREATE TABLE actions(id INTEGER PRIMARY KEY,label TEXT); CREATE TABLE evidence(value TEXT); CREATE TABLE assets(uuid TEXT PRIMARY KEY,filename TEXT,src_path TEXT,width INTEGER,height INTEGER,added_at TEXT,src_kind TEXT);", v = crate::SCHEMA_VERSION)).unwrap();
        let source = crate::sqlite::open(package.join("source.sqlite")).unwrap();
        source.execute_batch("CREATE TABLE assets(uuid TEXT,filename TEXT,src_path TEXT,width INTEGER,height INTEGER,added_at TEXT,src_kind TEXT,in_scope INTEGER,trashed INTEGER,source_id TEXT);CREATE TABLE labels(uuid TEXT,dim TEXT,value TEXT,conf REAL);").unwrap();
        for (id, name, path, sid) in rows {
            let path = path.to_string_lossy().into_owned();
            db.execute("INSERT INTO assets(uuid,filename,src_path) VALUES (?,?,?)", [id, name, &path]).unwrap();
            source
                .execute(
                    "INSERT INTO assets(uuid,filename,src_path,source_id) VALUES (?,?,?,?)",
                    [id, name, &path, sid],
                )
                .unwrap();
        }
        drop((db, source));
        Library::open(&package).unwrap()
    }

    #[test]
    fn export_copies_by_asset_id_and_leaves_originals_alone() {
        let dir = scratch("run");
        let lib = library(&dir);
        let out = dir.join("out");
        fs::create_dir_all(&out).unwrap();
        let ids: Vec<String> = ["a", "b", "p", "missing"].map(String::from).to_vec();
        let cancel = AtomicBool::new(false);
        let counts = run(&lib, &ids, (Group::None, ""), &out, &cancel, &|_, _| {}).map_err(|(e, _)| e).unwrap();
        assert_eq!(counts, Counts { copied: 2, existed: 0, failed: 2, derived: 1 });
        assert_eq!(fs::read(out.join("a.png")).unwrap(), b"synthetic a");
        // 照片的派生图用「照片」里的文件名、派生图的扩展名。
        assert_eq!(fs::read(out.join("P.jpeg")).unwrap(), b"synthetic p");
        assert_eq!(fs::read(dir.join("art/a.png")).unwrap(), b"synthetic a");
        // 再导出一次：都已存在，不多出带编号的副本。
        let again = run(&lib, &ids, (Group::None, ""), &out, &cancel, &|_, _| {}).map_err(|(e, _)| e).unwrap();
        assert_eq!(again, Counts { copied: 0, existed: 2, failed: 2, derived: 1 });
        assert_eq!(fs::read_dir(&out).unwrap().count(), 2);
        // 照片来源的图不显示位置；路径只按资产 ID 查。
        let lookup = Lookup::new(&lib).unwrap();
        assert!(lookup.locate("p").unwrap().photos.is_some() && lookup.locate("a").unwrap().photos.is_none());
        assert!(crate::app_asset_files::reveal(&lib, "p").is_err());
        assert!(lookup.locate("../a.png").is_err() && lookup.locate("b").is_err());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn groups_parse() {
        assert_eq!(Group::parse("character").unwrap(), Group::Character);
        assert_eq!(Group::parse("").unwrap(), Group::None);
        assert!(Group::parse("../x").is_err());
    }
}
