//! 从缩略图派生的颜色。不写入图库数据库，不作为同步、备份或导入的输入。
mod palette;

use crate::{app_library::Library, http::LibraryContext, http_json::Json, Result};
use image::RgbImage;
use rusqlite::{functions::FunctionFlags, params, Connection, OpenFlags};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError},
    time::{Duration, Instant},
};

const CACHE: &str = "colors-v1.sqlite";
const BATCH: usize = 128;
const POLL: Duration = Duration::from_secs(2);
const RETRY: Duration = Duration::from_secs(300);
const HASH: &str = "coalesce((SELECT value FROM meta WHERE key='folder_hash:' || a.uuid),'')";
pub(crate) const ASSET_FIELDS: &str =
    "a.uuid,a.src_path,coalesce((SELECT value FROM meta WHERE key='folder_hash:' || a.uuid),'')";

#[derive(Debug)]
pub(crate) struct Asset {
    pub id: String,
    pub source: PathBuf,
    hash: String,
}

impl Asset {
    pub(crate) fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        let path: Option<String> = row.get(1)?;
        Ok(Self { id: row.get(0)?, source: PathBuf::from(path.unwrap_or_default()), hash: row.get(2)? })
    }

    fn stamp(&self) -> Option<String> {
        crate::app_files::cache_key(&self.id, &fs::metadata(&self.source).ok()?, "thumb")
    }
}

fn cache_path(package: &Path) -> PathBuf {
    package.join("thumbnails").join(CACHE)
}
fn reader(package: &Path) -> Result<Connection> {
    let c = crate::sqlite::open_with_flags(cache_path(package), OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    c.busy_timeout(Duration::from_millis(100))?;
    Ok(c)
}
fn writer(package: &Path) -> Result<Connection> {
    let path = cache_path(package);
    fs::create_dir_all(path.parent().expect("color cache has a parent"))?;
    let c = crate::sqlite::open(path)?;
    c.busy_timeout(Duration::from_millis(250))?;
    c.execute_batch(
        "CREATE TABLE IF NOT EXISTS colors(
          id TEXT PRIMARY KEY,source TEXT NOT NULL,hash TEXT NOT NULL,stamp TEXT NOT NULL,
          color TEXT NOT NULL,rank INTEGER NOT NULL,palette TEXT NOT NULL
         ) WITHOUT ROWID;",
    )?;
    Ok(c)
}

struct Record {
    asset: Asset,
    stamp: String,
    color: String,
    rank: i64,
    palette: String,
}
impl Record {
    fn new(asset: Asset, stamp: String, rgb: &RgbImage) -> Option<Self> {
        let colors = palette::extract(rgb);
        let main = colors.first()?.rgb;
        let palette: Vec<_> = colors
            .into_iter()
            .map(|swatch| {
                Json::object([("color", palette::hex(swatch.rgb).into()), ("share", Json::Float(swatch.share))])
            })
            .collect();
        Some(Self {
            asset,
            stamp,
            color: palette::hex(main),
            rank: palette::rank(main),
            palette: Json::Array(palette).dump_reply(),
        })
    }
}
fn write(c: &mut Connection, records: &[Record]) -> Result<()> {
    if records.is_empty() {
        return Ok(());
    }
    let tx = c.transaction()?;
    {
        let mut insert = tx.prepare(
            "INSERT INTO colors VALUES(?1,?2,?3,?4,?5,?6,?7)
             ON CONFLICT(id) DO UPDATE
             SET source=excluded.source,hash=excluded.hash,stamp=excluded.stamp,color=excluded.color,
                 rank=excluded.rank,palette=excluded.palette
             WHERE colors.source<>excluded.source OR colors.hash<>excluded.hash OR colors.stamp<>excluded.stamp",
        )?;
        for record in records {
            // 解码期间原文件换过就丢掉结果。数据库侧另有路径和同步哈希，读取时按当前图库快照校验。
            if record.asset.stamp().as_deref() == Some(&record.stamp) {
                insert.execute(params![
                    record.asset.id,
                    record.asset.source.to_string_lossy(),
                    record.asset.hash,
                    record.stamp,
                    record.color,
                    record.rank,
                    record.palette,
                ])?;
            }
        }
    }
    tx.commit()?;
    Ok(())
}

pub(crate) fn generated(package: &Path, asset: Asset, stamp: String, rgb: &RgbImage) {
    let Some(record) = Record::new(asset, stamp, rgb) else { return };
    if let Err(e) = writer(package).and_then(|mut c| write(&mut c, &[record])) {
        eprintln!("图片颜色缓存写入失败：{e}");
    }
}

#[derive(Default)]
pub(crate) struct Worker(Option<Sender<()>>);
impl Worker {
    pub(crate) fn start(&mut self, library: &Library) {
        if self.0.is_some() {
            return;
        }
        let (stop, receiver) = mpsc::channel();
        let library = library.clone();
        match std::thread::Builder::new().name("image-colors".into()).spawn(move || {
            if let Err(e) = maintain(&library, &receiver) {
                eprintln!("图片颜色补算已停止：{e}");
            }
        }) {
            Ok(_) => self.0 = Some(stop),
            Err(e) => eprintln!("图片颜色补算未能启动：{e}"),
        }
    }
    pub(crate) fn stop(&mut self) {
        self.0 = None;
    }
}

fn maintain(library: &Library, stop: &Receiver<()>) -> Result<()> {
    let c = crate::app_library::read_connection(&library.db())?;
    let mut previous = None;
    let mut retry_at = Instant::now();
    while matches!(stop.try_recv(), Err(TryRecvError::Empty)) {
        let version: i64 = c.pragma_query_value(None, "data_version", |row| row.get(0))?;
        if previous != Some(version) || Instant::now() >= retry_at {
            if let Err(e) = backfill(&c, library.instance_path(), stop) {
                eprintln!("图片颜色补算失败：{e}");
            }
            previous = Some(version);
            retry_at = Instant::now() + RETRY;
        }
        if stop.recv_timeout(POLL) != Err(RecvTimeoutError::Timeout) {
            return Ok(());
        }
    }
    Ok(())
}

fn backfill(c: &Connection, package: &Path, stop: &Receiver<()>) -> Result<()> {
    let thumbnails = package.join("thumbnails");
    let mut cache = None;
    let mut after = String::new();
    let sql =
        format!("SELECT {ASSET_FIELDS} FROM assets a WHERE a.uuid>?1 AND a.src_path<>'' ORDER BY a.uuid LIMIT ?2");
    let mut batch = c.prepare(&sql)?;
    loop {
        let assets = batch.query_map(params![after, BATCH], Asset::from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
        let Some(last) = assets.last() else { break };
        after.clone_from(&last.id);
        let mut records = Vec::new();
        for asset in assets {
            if !matches!(stop.try_recv(), Err(TryRecvError::Empty)) {
                return Ok(());
            }
            let Some(stamp) = asset.stamp() else { continue };
            let conn = match &cache {
                Some(conn) => conn,
                None => cache.insert(writer(package)?),
            };
            let done: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM colors WHERE id=?1 AND source=?2 AND hash=?3 AND stamp=?4)",
                params![asset.id, asset.source.to_string_lossy(), asset.hash, stamp],
                |row| row.get(0),
            )?;
            if done {
                continue;
            }
            let thumbnail = thumbnails.join(format!("{stamp}.jpg"));
            if let Ok(rgb) = crate::app_files::color_input(&asset.source, &thumbnail) {
                records.extend(Record::new(asset, stamp, &rgb));
            }
        }
        if !matches!(stop.try_recv(), Err(TryRecvError::Empty)) {
            return Ok(());
        }
        if let Some(conn) = &mut cache {
            write(conn, &records)?;
        }
    }
    Ok(())
}

pub(crate) fn ordering(c: &Connection, context: &LibraryContext) -> Result<String> {
    let ranks = context.library.as_deref().and_then(|p| load_ranks(p).ok()).unwrap_or_default();
    c.create_scalar_function("asset_color_rank", 3, FunctionFlags::SQLITE_DETERMINISTIC, move |args| {
        let Some((source, hash, rank)) = ranks.get(args.get_raw(0).as_str()?) else { return Ok(None) };
        let path = args.get_raw(1).as_str().ok();
        let current_hash = args.get_raw(2).as_str().ok();
        Ok((path == Some(source.as_str()) && current_hash == Some(hash.as_str())).then_some(*rank))
    })?;
    Ok(format!("coalesce(asset_color_rank(a.uuid,a.src_path,{HASH}),4096),a.added_at DESC,a.uuid"))
}
type Ranks = HashMap<String, (String, String, i64)>;
fn load_ranks(package: &Path) -> Result<Ranks> {
    let c = reader(package)?;
    let mut statement = c.prepare("SELECT id,source,hash,rank FROM colors")?;
    let rows = statement.query_map([], |row| Ok((row.get(0)?, (row.get(1)?, row.get(2)?, row.get(3)?))))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub(crate) fn requested(q: &HashMap<String, String>) -> bool {
    q.get("marks").is_some_and(|marks| marks.split(',').any(|mark| mark == "color"))
}

fn field_array<'a>(body: &'a mut Json, key: &str) -> Option<&'a mut Vec<Json>> {
    let Json::Object(fields) = body else { return None };
    let (_, Json::Array(items)) = fields.iter_mut().find(|(name, _)| name == key)? else { return None };
    Some(items)
}

struct Color {
    color: String,
    palette: Option<Json>,
}
fn load_colors(c: &Connection, package: Option<&Path>, ids: &[String], detail: bool) -> Result<HashMap<String, Color>> {
    let Some(package) = package.filter(|_| !ids.is_empty()) else { return Ok(HashMap::new()) };
    let ids = serde_json::to_string(&ids).expect("asset IDs are serializable");
    let cached = (|| -> Result<_> {
        let cache = reader(package)?;
        let palette = if detail { "palette" } else { "NULL" };
        let sql =
            format!("SELECT id,source,hash,color,{palette} FROM colors WHERE id IN (SELECT value FROM json_each(?))");
        let mut stmt = cache.prepare(&sql)?;
        let rows = stmt.query_map([&ids], |r| {
            let palette: Option<String> = r.get(4)?;
            let color =
                Color { color: r.get(3)?, palette: palette.and_then(|s| crate::http_json::parse_python(&s).ok()) };
            Ok((r.get::<_, String>(0)?, (r.get::<_, String>(1)?, r.get::<_, String>(2)?, color)))
        })?;
        Ok(rows.collect::<rusqlite::Result<HashMap<_, _>>>()?)
    })();
    let Ok(mut cached) = cached else { return Ok(HashMap::new()) };
    if cached.is_empty() {
        return Ok(HashMap::new());
    }
    let sql = format!("SELECT {ASSET_FIELDS} FROM assets a WHERE a.uuid IN (SELECT value FROM json_each(?))");
    let mut stmt = c.prepare(&sql)?;
    let mut result = HashMap::new();
    for source in stmt.query_map([&ids], Asset::from_row)? {
        let asset = source?;
        if let Some((path, hash, color)) = cached.remove(&asset.id) {
            if asset.source == Path::new(&path) && asset.hash == hash {
                result.insert(asset.id, color);
            }
        }
    }
    Ok(result)
}

pub(crate) fn decorate_response(
    c: &Connection,
    context: &LibraryContext,
    route: &str,
    q: &HashMap<String, String>,
    body: &mut Json,
) -> Result<()> {
    let mark = requested(q);
    if !mark && route != "/api/calendar" {
        return Ok(());
    }
    let mut groups = Vec::new();
    let mut image_rows: Vec<&mut Json> = Vec::new();
    let id_key = match route {
        "/api/detail" => {
            image_rows.push(body);
            "uuid"
        }
        "/api/assets" | "/api/artist-directory" => {
            image_rows.extend(field_array(body, "items").into_iter().flatten());
            if route == "/api/assets" {
                "uuid"
            } else {
                "cover"
            }
        }
        "/api/galleries/covers" | "/api/characters" => {
            if let Json::Array(items) = body {
                image_rows.extend(items);
            }
            "cover"
        }
        "/api/works" => {
            if let Json::Array(works) = body {
                for work in works {
                    image_rows.extend(field_array(work, "covers").into_iter().flatten());
                }
            }
            "uuid"
        }
        "/api/home" | "/api/calendar" => {
            if let Some(days) = field_array(body, "days") {
                groups = days.iter_mut().collect();
            }
            "uuid"
        }
        _ => return Ok(()),
    };
    let samples = if route == "/api/calendar" { "samples" } else { "items" };
    let id = |row: &Json| row.get(id_key).map(Json::text).unwrap_or("").to_owned();
    let mut ids: Vec<String> = image_rows.iter().map(|row| id(row)).collect();
    for day in &mut groups {
        ids.extend(field_array(day, samples).into_iter().flatten().map(|row| id(row)));
    }
    ids.retain(|id| !id.is_empty());
    let colors = load_colors(c, context.library.as_deref(), &ids, route == "/api/detail")?;
    let color_for = |row: &Json| colors.get(&id(row));
    let color_value = |color: Option<&Color>| color.map(|entry| entry.color.as_str()).into();
    for row in image_rows {
        let color = color_for(row);
        row.set("color", color_value(color));
        if route == "/api/detail" {
            let palette = color.and_then(|v| v.palette.clone());
            row.set("palette", palette.unwrap_or(Json::Array(Vec::new())));
        }
    }
    for day in groups {
        let Some(items) = field_array(day, samples) else { continue };
        let day_color = (route == "/api/calendar").then(|| color_value(items.iter().find_map(&color_for)));
        if mark {
            for item in items {
                item.set("color", color_value(color_for(item)));
            }
        }
        if let Some(color) = day_color {
            day.set("color", color);
        }
    }
    Ok(())
}
