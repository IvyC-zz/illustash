//! Existing provenance.verify local drawing comparison. Only machine cache is writable.
use super::{check_owner, write, Request};
use crate::{app_provenance::cache, research_compute::relation_image, Error, Result};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

const GRID: usize = 256;
const COLOUR: usize = 24 * 24 * 3;
const CHUNK: usize = 4096;

pub(super) struct Signature {
    pub ratio: f64,
    pub grid: Vec<u8>,
    pub colour: Vec<u8>,
}

fn invalid(message: &str) -> Error {
    Error::Invalid(message.into())
}

fn normalise(grid: &[u8]) -> Result<(Vec<f32>, f32)> {
    if grid.len() != GRID {
        return Err(invalid("溯源灰度签名长度无效"));
    }
    let mut x: Vec<_> = grid.iter().map(|&v| f32::from(v)).collect();
    let mean = crate::research_compute::contiguous_sum32(&x) / GRID as f32;
    for v in &mut x {
        *v -= mean;
    }
    let squares: Vec<_> = x.iter().map(|v| v * v).collect();
    let sum = crate::research_compute::contiguous_sum32(&squares);
    let spread = (sum / GRID as f32).sqrt();
    let norm = sum.sqrt() + 1e-6f32;
    for v in &mut x {
        *v /= norm;
    }
    Ok((x, spread))
}

pub(super) struct Library {
    ids: Vec<String>,
    paths: HashMap<String, Option<String>>,
    x: Vec<f32>,
    logr: Vec<f32>,
    spread: Vec<f32>,
    colours: HashMap<String, Vec<u8>>,
}
impl Library {
    pub fn open(path: &Path) -> Result<Self> {
        let c = crate::app_library::read_connection(&path.join("library.sqlite"))?;
        let paths: HashMap<String, Option<String>> = c
            .prepare("SELECT uuid,src_path FROM assets")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let mut sigs: Vec<(String, f64, Vec<u8>)> = c
            .prepare("SELECT uuid,ratio,grid FROM image_sigs WHERE ok=1")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?;
        sigs.retain(|(id, _, _)| paths.contains_key(id));
        let known: HashSet<_> = sigs.iter().map(|(id, _, _)| id.clone()).collect();
        for (id, path) in &paths {
            if known.contains(id) {
                continue;
            }
            let path = path.as_deref().ok_or_else(|| invalid("原图路径缺失"))?;
            {
                // Legacy Library skips unreadable/missing paths, never fabricates a signature.
                if let Ok((ratio, grid)) = relation_image::signature(Path::new(path)) {
                    sigs.push((id.clone(), ratio, grid));
                }
            }
        }
        let mut lib = Self { ids: vec![], paths, x: vec![], logr: vec![], spread: vec![], colours: HashMap::new() };
        for (id, ratio, grid) in sigs {
            let (x, spread) = normalise(&grid)?;
            lib.ids.push(id);
            lib.x.extend(x);
            lib.logr.push((ratio as f32).ln());
            lib.spread.push(spread);
        }
        Ok(lib)
    }
    fn colour(&mut self, asset: &str) -> Result<&[u8]> {
        if !self.colours.contains_key(asset) {
            let path = self.paths.get(asset).and_then(|p| p.as_deref()).ok_or_else(|| invalid("原图路径缺失"))?;
            let colour = relation_image::reference_colour(Path::new(path))?;
            self.colours.insert(asset.to_owned(), colour);
        }
        Ok(self.colours.get(asset).ok_or_else(|| invalid("颜色签名缺失"))?)
    }
    fn colour_diff(&mut self, asset: &str, colour: &[u8]) -> Result<Option<f32>> {
        if colour.len() != COLOUR {
            return Err(invalid("溯源颜色签名长度无效"));
        }
        if self.paths.get(asset).and_then(|p| p.as_deref()).is_none() {
            return Err(invalid("原图路径缺失"));
        }
        let reference = match self.colour(asset) {
            Ok(v) => v,
            Err(_) => return Ok(None),
        };
        if reference.len() != COLOUR {
            return Err(invalid("溯源颜色签名长度无效"));
        }
        let diffs: Vec<_> = colour.iter().zip(reference).map(|(&a, &b)| (f32::from(a) - f32::from(b)).abs()).collect();
        Ok(Some(crate::research_compute::contiguous_sum32(&diffs) / COLOUR as f32))
    }
    pub fn match_sig(&mut self, sig: &Signature) -> Result<Vec<(String, f32)>> {
        let (x, spread) = normalise(&sig.grid)?;
        if spread < 6. || self.ids.is_empty() {
            return Ok(vec![]);
        }
        if sig.colour.len() != COLOUR {
            return Err(invalid("溯源颜色签名长度无效"));
        }
        let scores = illustash_platform::platform_research_math::cross(&self.x, self.ids.len(), &x, 1, GRID)
            .map_err(Error::Invalid)?;
        let mut hits = vec![];
        // Python scalar np.log(ratio) is f64 here, unlike local_run's f32 array.
        let lr = sig.ratio.ln();
        for (i, score) in scores.into_iter().enumerate() {
            // NumPy 2.x promotes the scalar result to the array's float32 dtype.
            if score >= 0.9 && (self.logr[i] - lr as f32).abs() <= 0.03 && self.spread[i] >= 6. {
                let asset = self.ids[i].clone();
                if self.colour_diff(&asset, &sig.colour)?.is_some_and(|diff| diff < 12.) {
                    hits.push((asset, score));
                }
            }
        }
        Ok(hits)
    }
}

pub(super) fn record(
    c: &Connection,
    artist: &str,
    asset: &str,
    site: &str,
    post_id: &str,
    url: &str,
    score: f32,
) -> Result<()> {
    // Python round(float(score)*100, 1) uses decimal round-to-even, not f32 round.
    let similarity =
        format!("{:.1}", f64::from(score) * 100.).parse::<f64>().map_err(|e| Error::Invalid(e.to_string()))?;
    c.execute("INSERT OR IGNORE INTO image_sources(asset,site,post_id,url,artist_id,method,similarity) VALUES (?,?,?,?,?,'gallery_match',?)",
        params![asset,site,post_id,url,artist,similarity])?;
    let proof = format!(
        "{{{}: {}}}",
        crate::character_json::dump(&json!(site), true),
        crate::character_json::dump(&json!(post_id), true)
    );
    c.execute("INSERT INTO image_artists(asset,artist_id,method,score,evidence,status) VALUES (?,?,'gallery_match',?,?,'auto') ON CONFLICT(asset,artist_id) DO NOTHING",
        params![asset,artist,score,proof])?;
    Ok(())
}
pub(super) fn url(site: &str, post: &str) -> String {
    if site == "danbooru" {
        format!("https://danbooru.donmai.us/posts/{post}")
    } else {
        format!("https://www.pixiv.net/artworks/{post}")
    }
}
pub(super) fn progress(request: &Request, sequence: &mut u64, mut value: Value) -> Result<()> {
    check_owner(request.owner_pid)?;
    *sequence += 1;
    value["sequence"] = (*sequence).into();
    write(&request.mailbox.join("progress.json"), &value)
}
pub(super) fn run(request: &Request, sequence: &mut u64) -> Result<Value> {
    let mut lib = Library::open(&request.library)?;
    let mut c = cache::connect(&request.cache.join("provenance.sqlite"))?;
    let seen: HashSet<String> = c
        .prepare("SELECT target FROM lookups WHERE backend='ref_compare'")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let new: Vec<_> = lib.ids.iter().enumerate().filter(|(_, u)| !seen.contains(*u)).map(|(i, _)| i).collect();
    if new.is_empty() {
        return Ok(json!({"pictures":0,"matches":0,"stopped":""}));
    }
    let total: usize = c.query_row("SELECT count(*) FROM ref_sigs WHERE colour IS NOT NULL", [], |r| r.get(0))?;
    progress(request, sequence, json!({"log":format!("本地重比新图：{} 张新图 × {total} 个已存签名",new.len())}))?;
    let x: Vec<_> = new.iter().flat_map(|&i| lib.x[i * GRID..(i + 1) * GRID].iter().copied()).collect();
    let (mut matches, mut done) = (0usize, 0usize);
    let mut statement =
        c.prepare("SELECT site,post_id,artist_id,ratio,grid,colour FROM ref_sigs WHERE colour IS NOT NULL")?;
    let mut rows = statement.query([])?;
    loop {
        let mut batch = vec![];
        while batch.len() < CHUNK {
            let Some(row) = rows.next()? else {
                break;
            };
            batch.push((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, f64>(3)?,
                row.get::<_, Vec<u8>>(4)?,
                row.get::<_, Vec<u8>>(5)?,
            ));
        }
        if batch.is_empty() {
            break;
        }
        let mut r = vec![];
        let mut rspread = vec![];
        for row in &batch {
            let (normal, spread) = normalise(&row.4)?;
            r.extend(normal);
            rspread.push(spread);
        }
        for start in (0..new.len()).step_by(CHUNK) {
            check_owner(request.owner_pid)?;
            let n = CHUNK.min(new.len() - start);
            let scores = illustash_platform::platform_research_math::cross(
                &r,
                batch.len(),
                &x[start * GRID..(start + n) * GRID],
                n,
                GRID,
            )
            .map_err(Error::Invalid)?;
            for (i, row) in batch.iter().enumerate() {
                for j in 0..n {
                    let k = new[start + j];
                    let score = scores[i * n + j];
                    if !(score >= 0.9
                        && ((row.3 as f32).ln() - lib.logr[k]).abs() <= 0.03
                        && rspread[i] >= 6.
                        && lib.spread[k] >= 6.)
                    {
                        continue;
                    }
                    let asset = lib.ids[k].clone();
                    // Legacy local_run catches ValueError from its per-hit colour reshape.
                    if row.5.len() != COLOUR {
                        continue;
                    }
                    if lib.colour_diff(&asset, &row.5)?.is_some_and(|diff| diff < 12.) {
                        if let Some(artist) = row.2.as_deref().filter(|s| !s.is_empty()) {
                            let before = c.total_changes();
                            let tx = c.unchecked_transaction()?;
                            record(&tx, artist, &asset, &row.0, &row.1, &url(&row.0, &row.1), score)?;
                            tx.commit()?;
                            matches += usize::from(c.total_changes() > before);
                        }
                    }
                }
            }
        }
        done += batch.len();
        progress(
            request,
            sequence,
            json!({"log":format!("本地重比新图：签名 {done}/{total}，比对上 {matches} 张"),"progress":{"label":"签名","done":done,"total":total,"found":matches}}),
        )?;
    }
    drop(rows);
    drop(statement);
    check_owner(request.owner_pid)?;
    let tx = c.transaction()?;
    for &i in &new {
        tx.execute("INSERT INTO lookups(target,backend,attempts,last_at,last_result) VALUES (?,'ref_compare',1,datetime('now'),'miss') ON CONFLICT(target,backend) DO NOTHING", [&lib.ids[i]])?;
    }
    tx.commit()?;
    Ok(json!({"pictures":new.len(),"matches":matches,"stopped":""}))
}
