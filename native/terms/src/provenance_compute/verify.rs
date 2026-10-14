//! Public thumbnail comparison through the App's existing authorized mailbox.
//! Credentials, pacing, budgets and formal evidence publication stay in the parent.
use super::{
    check_owner, groups,
    local_match::{self, Library, Signature},
    write, Request,
};
use crate::{research_compute::relation_image, Error};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque},
    fs,
    time::{Duration, Instant},
};

enum Failure {
    Stop(String),
    Fatal(Error),
}
type Result<T> = std::result::Result<T, Failure>;
impl From<Error> for Failure {
    fn from(e: Error) -> Self {
        Self::Fatal(e)
    }
}
impl From<rusqlite::Error> for Failure {
    fn from(e: rusqlite::Error) -> Self {
        Self::Fatal(e.into())
    }
}
impl From<std::io::Error> for Failure {
    fn from(e: std::io::Error) -> Self {
        Self::Fatal(e.into())
    }
}
impl From<serde_json::Error> for Failure {
    fn from(e: serde_json::Error) -> Self {
        Self::Fatal(Error::Invalid(e.to_string()))
    }
}
fn invalid(s: impl Into<String>) -> Failure {
    Failure::Fatal(Error::Invalid(s.into()))
}

struct Network<'a> {
    request: &'a Request,
    used: std::sync::atomic::AtomicU64,
    gate: std::sync::Mutex<()>,
}
impl Network<'_> {
    fn bytes(&self, url: &str, query: &[(&str, String)]) -> Result<Option<Vec<u8>>> {
        let _guard = self.gate.lock().map_err(|_| invalid("网络 mailbox 锁已损坏"))?;
        // Scope/consent is enforced again by App. This calculator cannot open a socket.
        let mut target = url.to_owned();
        if !query.is_empty() {
            let encoded = url::form_urlencoded::Serializer::new(String::new())
                .extend_pairs(query.iter().map(|(k, v)| (*k, v)))
                .finish();
            target.push(if url.contains('?') { '&' } else { '?' });
            target.push_str(&encoded);
        }
        check_owner(self.request.owner_pid)?;
        let response = self.request.mailbox.join("net-response.json");
        match fs::remove_file(&response) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        write(&self.request.mailbox.join("net-request.json"), &json!({"identity":self.request.identity,"url":target}))?;
        while !response.is_file() {
            check_owner(self.request.owner_pid)?;
            std::thread::sleep(Duration::from_millis(10));
        }
        let answer = crate::strict_json::parse(&fs::read_to_string(&response)?).map_err(Error::Invalid)?;
        self.used.store(
            answer["used"].as_u64().ok_or_else(|| invalid("网络响应缺少流量"))?,
            std::sync::atomic::Ordering::Relaxed,
        );
        if let Some(e) = answer["error"].as_str() {
            return Err(Failure::Stop(e.into()));
        }
        let status = answer["status"].as_u64().ok_or_else(|| invalid("网络响应缺少状态"))?;
        if matches!(status, 401 | 404) {
            return Ok(None);
        }
        Ok(Some(fs::read(self.request.mailbox.join("net-body.bin"))?))
    }
    fn json(&self, url: &str, query: &[(&str, String)]) -> Result<Value> {
        match self.bytes(url, query)? {
            Some(body) => Ok(serde_json::from_slice(&body)?),
            None => Ok(Value::Null),
        }
    }
}
struct Progress {
    artists: usize,
    done: usize,
    found: usize,
    works: usize,
    planned: usize,
    fetched: usize,
    last: Instant,
}
impl Progress {
    fn new(artists: usize) -> Self {
        Self {
            artists,
            done: 0,
            found: 0,
            works: 0,
            planned: 0,
            fetched: 0,
            last: Instant::now() - Duration::from_secs(5),
        }
    }
    fn show(&mut self, request: &Request, sequence: &mut u64, used: u64, force: bool) -> Result<()> {
        check_owner(request.owner_pid)?;
        if !force && self.last.elapsed() < Duration::from_secs(5) {
            return Ok(());
        }
        self.last = Instant::now();
        let mb = used as f64 / 1048576.;
        local_match::progress(
            request,
            sequence,
            json!({"log":format!("本地比对：第 {}/{} 位画师，作品 {}/{}，已下载小图 {} 张，比对上 {} 张，流量 {:.0} MB",(self.done+1).min(self.artists),self.artists,self.works,self.planned,self.fetched,self.found,mb),
            "progress":{"label":"画师","done":self.done,"total":self.artists,"found":self.found,"mb":(mb*10.).round_ties_even()/10.,"sub_label":"作品","sub_done":self.works,"sub_total":self.planned}}),
        )?;
        Ok(())
    }
    fn work(&mut self, n: usize, net: &Network, seq: &mut u64) -> Result<()> {
        self.works += 1;
        self.found += n;
        self.show(net.request, seq, net.used.load(std::sync::atomic::Ordering::Relaxed), false)
    }
}
fn matched(
    c: &mut Connection,
    lib: &mut Library,
    artist: &str,
    sig: &Signature,
    site: &str,
    post: &str,
) -> Result<usize> {
    let hits = lib.match_sig(sig)?;
    let tx = c.transaction()?;
    for (asset, score) in &hits {
        local_match::record(&tx, artist, asset, site, post, &local_match::url(site, post), *score)?;
    }
    tx.commit()?;
    Ok(hits.len())
}
#[allow(clippy::too_many_arguments)]
fn reference(
    c: &mut Connection,
    net: &Network,
    artist: &str,
    site: &str,
    post: &str,
    page: i64,
    url: &str,
    ratio: Option<f64>,
    progress: &mut Progress,
) -> Result<Option<Signature>> {
    let saved = c
        .query_row(
            "SELECT ratio,grid,colour FROM ref_sigs WHERE site=? AND post_id=? AND page=? AND colour IS NOT NULL",
            params![site, post, page],
            |r| Ok(Signature { ratio: r.get(0)?, grid: r.get(1)?, colour: r.get(2)? }),
        )
        .optional()?;
    if saved.is_some() {
        return Ok(saved);
    }
    let bytes = net.bytes(url, &[])?;
    progress.fetched += 1;
    let Some(bytes) = bytes.filter(|b| !b.is_empty()) else { return Ok(None) };
    let sig = (|| -> crate::Result<Signature> {
        let (r, grid) = relation_image::signature_bytes(&bytes)?;
        Ok(Signature {
            ratio: ratio.filter(|v| *v != 0.).unwrap_or(r),
            grid,
            colour: relation_image::reference_colour_bytes(&bytes)?,
        })
    })();
    let Ok(sig) = sig else { return Ok(None) };
    let tx = c.transaction()?;
    tx.execute(
        "INSERT OR REPLACE INTO ref_sigs(site,post_id,page,artist_id,ratio,grid,colour,url) VALUES (?,?,?,?,?,?,?,?)",
        params![site, post, page, artist, sig.ratio, sig.grid, sig.colour, url],
    )?;
    tx.commit()?;
    Ok(Some(sig))
}
fn text(v: &Value) -> Result<String> {
    match v {
        Value::String(s) => Ok(s.clone()),
        Value::Number(n) => Ok(n.to_string()),
        _ => Err(invalid("作品身份无效")),
    }
}
fn links(c: &Connection, artist: &str) -> Result<Vec<(String, String)>> {
    Ok(c.prepare("SELECT system,key FROM artist_links WHERE artist_id=? ORDER BY system,key")?
        .query_map([artist], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?)
}
fn posts(c: &mut Connection, net: &Network, artist: &str, tag: &str) -> Result<Vec<Value>> {
    let posts = net.json(
        "https://danbooru.donmai.us/posts.json",
        &[
            ("tags", tag.into()),
            ("limit", "200".into()),
            ("only", "id,image_width,image_height,preview_file_url,source".into()),
        ],
    )?;
    let page = net.json(
        "https://danbooru.donmai.us/artists.json",
        &[("search[name]", tag.into()), ("only", "name,urls".into())],
    )?;
    let re = regex::Regex::new(r"pixiv\.net/(?:en/)?(?:users/|member\.php\?id=)(\d+)")
        .map_err(|e| invalid(e.to_string()))?;
    let tx = c.transaction()?;
    if let Some(urls) = page.get(0).and_then(|p| p["urls"].as_array()) {
        for u in urls {
            if let Some(m) = u["url"].as_str().and_then(|u| re.captures(u)) {
                tx.execute("INSERT OR IGNORE INTO artist_links(system,key,artist_id,url,source) VALUES ('pixiv',?,?,?,'danbooru_artist')",params![&m[1],artist,format!("https://www.pixiv.net/users/{}",&m[1])])?;
            }
        }
    }
    tx.commit()?;
    if posts.is_null() {
        Ok(vec![])
    } else {
        posts.as_array().cloned().ok_or_else(|| invalid("Danbooru 作品列表无效"))
    }
}
#[allow(clippy::too_many_arguments)]
fn danbooru(
    c: &mut Connection,
    lib: &mut Library,
    net: &Network,
    artist: &str,
    posts: &[Value],
    known: &HashSet<String>,
    covered: &HashSet<String>,
    progress: &mut Progress,
    seq: &mut u64,
    probe: bool,
) -> Result<usize> {
    let mut found = 0;
    let source = regex::Regex::new(r"pixiv\.net/(?:en/)?artworks/(\d+)|illust_id=(\d+)|/(\d+)_p(\d+)")
        .map_err(|e| invalid(e.to_string()))?;
    for (n, p) in posts.iter().enumerate() {
        if probe && n == 30 && found == 0 {
            for _ in &posts[n..] {
                progress.work(0, net, seq)?;
            }
            break;
        }
        let post = text(&p["id"])?;
        let seen = p["source"].as_str().and_then(|s| source.captures(s)).is_some_and(|m| {
            let id = m.get(1).or_else(|| m.get(2)).or_else(|| m.get(3)).map(|m| m.as_str()).unwrap_or("");
            covered.contains(id) && m.get(4).map_or(0, |m| m.as_str().parse::<u64>().unwrap_or(u64::MAX)) < 3
        });
        let mut here = 0;
        if let Some(url) = p["preview_file_url"].as_str().filter(|u| !u.is_empty()) {
            if p["image_width"].as_f64().is_some_and(|w| w != 0.) && !known.contains(&post) && !seen {
                let ratio = p["image_width"].as_f64().ok_or_else(|| invalid("作品宽度无效"))?
                    / p["image_height"].as_f64().filter(|v| *v != 0.).ok_or_else(|| invalid("作品高度无效"))?;
                if let Some(sig) = reference(c, net, artist, "danbooru", &post, 0, url, Some(ratio), progress)? {
                    here = matched(c, lib, artist, &sig, "danbooru", &post)?;
                }
            }
        }
        found += here;
        progress.work(here, net, seq)?;
    }
    Ok(found)
}
struct Plan {
    all: Vec<String>,
    pos: HashMap<String, usize>,
    newest: HashSet<String>,
    seen: HashSet<String>,
    todo: Vec<String>,
    extra: usize,
}
impl Plan {
    fn add(&mut self, works: impl IntoIterator<Item = String>, progress: &mut Progress) {
        for work in works {
            if self.seen.contains(&work) || (!self.newest.contains(&work) && self.extra == 0) {
                continue;
            }
            if !self.newest.contains(&work) {
                self.extra -= 1;
            }
            self.seen.insert(work.clone());
            self.todo.push(work);
            progress.planned += 1;
        }
    }
    fn around(&self, work: &str) -> Vec<String> {
        self.pos
            .get(work)
            .map(|&i| self.all[i.saturating_sub(20)..(i + 21).min(self.all.len())].to_vec())
            .unwrap_or_default()
    }
    fn matches(&mut self, hits: Vec<(String, usize)>, progress: &mut Progress) -> usize {
        let mut found = 0;
        for (work, n) in hits {
            found += n;
            if n > 0 {
                self.add(self.around(&work), progress);
            }
        }
        found
    }
}
type WorkMatches = Vec<(String, usize)>;

#[allow(clippy::too_many_arguments)]
fn prepare(
    c: &mut Connection,
    lib: &mut Library,
    artist: &str,
    batch: &[String],
    known: &HashSet<String>,
    progress: &mut Progress,
    net: &Network,
    seq: &mut u64,
) -> Result<(WorkMatches, Vec<String>)> {
    let (mut found, mut todo) = (vec![], vec![]);
    for work in batch {
        let saved: Vec<Signature> = c
            .prepare("SELECT ratio,grid,colour FROM ref_sigs WHERE site='pixiv' AND post_id=? AND colour IS NOT NULL")?
            .query_map([work], |r| Ok(Signature { ratio: r.get(0)?, grid: r.get(1)?, colour: r.get(2)? }))?
            .collect::<rusqlite::Result<_>>()?;
        if !saved.is_empty() {
            let mut n = 0;
            for sig in saved {
                n += matched(c, lib, artist, &sig, "pixiv", work)?;
            }
            found.push((work.clone(), n));
            progress.work(n, net, seq)?;
        } else if known.contains(work) {
            progress.work(0, net, seq)?;
        } else {
            todo.push(work.clone());
        }
    }
    Ok((found, todo))
}
// Public work objects retain wire insertion order: expansion's traffic cap can
// depend on which matching work is handled first. Do not route this map through
// serde_json::Value's sorted object representation.
#[derive(serde::Deserialize)]
struct Listing {
    #[serde(default)]
    body: Option<ListingBody>,
}
#[derive(serde::Deserialize)]
struct ListingBody {
    #[serde(default)]
    works: WorkRows,
}
#[derive(Default)]
struct WorkRows(Vec<Value>);
impl<'de> serde::Deserialize<'de> for WorkRows {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct Rows;
        impl<'de> serde::de::Visitor<'de> for Rows {
            type Value = WorkRows;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("public work array or ordered object")
            }
            fn visit_unit<E: serde::de::Error>(self) -> std::result::Result<WorkRows, E> {
                Ok(WorkRows::default())
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> std::result::Result<WorkRows, A::Error> {
                let mut rows = vec![];
                while let Some(value) = seq.next_element()? {
                    rows.push(value);
                }
                Ok(WorkRows(rows))
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> std::result::Result<WorkRows, A::Error> {
                let mut rows = vec![];
                while let Some((_, value)) = map.next_entry::<String, Value>()? {
                    rows.push(value);
                }
                Ok(WorkRows(rows))
            }
        }
        deserializer.deserialize_any(Rows)
    }
}
fn listing(net: &Network, uid: &str, todo: &[String]) -> Result<Vec<Value>> {
    let mut query: Vec<_> = todo.iter().map(|w| ("ids[]", w.clone())).collect();
    query.extend([("work_category", "illustManga".into()), ("is_first_page", "0".into())]);
    let Some(bytes) = net.bytes(&format!("https://www.pixiv.net/ajax/user/{uid}/profile/illusts"), &query)? else {
        return Ok(vec![]);
    };
    let answer: Listing = serde_json::from_slice(&bytes)?;
    Ok(answer.body.map(|body| body.works.0).unwrap_or_default())
}
#[allow(clippy::too_many_arguments)]
fn download(
    c: &mut Connection,
    lib: &mut Library,
    net: &Network,
    artist: &str,
    todo: &[String],
    listed: &[Value],
    progress: &mut Progress,
    seq: &mut u64,
) -> Result<Vec<(String, usize)>> {
    let re = regex::Regex::new(r"/img/(\d{4}/\d\d/\d\d/\d\d/\d\d/\d\d)/").map_err(|e| invalid(e.to_string()))?;
    let mut found = vec![];
    for work in listed {
        let post = text(&work["id"])?;
        let mut here = 0;
        if let Some(m) = work["url"].as_str().and_then(|s| re.captures(s)) {
            let pages = match &work["pageCount"] {
                Value::Null => 1,
                Value::Number(n) => n.as_i64().ok_or_else(|| invalid("Pixiv 页数无效"))?,
                Value::String(s) => s.parse::<i64>().map_err(|e| invalid(e.to_string()))?,
                _ => return Err(invalid("Pixiv 页数无效")),
            };
            for page in 0..pages.min(3) {
                let url = format!(
                    "https://i.pximg.net/c/150x150_80/img-master/img/{}/{}_p{page}_master1200.jpg",
                    &m[1], post
                );
                if let Some(sig) = reference(c, net, artist, "pixiv", &post, page, &url, None, progress)? {
                    here += matched(c, lib, artist, &sig, "pixiv", &post)?;
                }
            }
        }
        found.push((post, here));
        progress.work(here, net, seq)?;
    }
    for _ in listed.len()..todo.len() {
        progress.work(0, net, seq)?;
    }
    Ok(found)
}
#[allow(clippy::too_many_arguments)]
fn pixiv(
    c: &mut Connection,
    lib: &mut Library,
    net: &Network,
    artist: &str,
    uid: &str,
    known: &HashSet<String>,
    progress: &mut Progress,
    seq: &mut u64,
    covered: &mut HashSet<String>,
    probe: bool,
) -> Result<usize> {
    let answer = net.json(&format!("https://www.pixiv.net/ajax/user/{uid}/profile/all"), &[])?;
    let mut all = vec![];
    for key in ["illusts", "manga"] {
        if let Some(o) = answer["body"][key].as_object() {
            all.extend(o.keys().cloned());
        }
    }
    if all.iter().any(|w| w.is_empty() || !w.bytes().all(|b| b.is_ascii_digit())) {
        return Err(invalid("Pixiv 作品身份非数字"));
    }
    all.sort_by(|a, b| {
        let a = a.trim_start_matches('0');
        let b = b.trim_start_matches('0');
        b.len().cmp(&a.len()).then_with(|| b.cmp(a))
    });
    let mut plan = Plan {
        pos: all.iter().enumerate().map(|(i, w)| (w.clone(), i)).collect(),
        newest: all.iter().take(300).cloned().collect(),
        all,
        seen: HashSet::new(),
        todo: vec![],
        extra: 300,
    };
    let saved: Vec<String> = c
        .prepare("SELECT DISTINCT post_id FROM image_sources WHERE site='pixiv' AND artist_id=?")?
        .query_map([artist], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for work in saved {
        plan.add(plan.around(&work), progress);
    }
    plan.add(plan.all.iter().take(if probe { 30 } else { 300 }).cloned().collect::<Vec<_>>(), progress);
    let found = std::thread::scope(|scope| -> Result<usize> {
        // Same one-thread, two-listings-ahead pipeline as the existing Python worker.
        // Network::gate serializes its single mailbox with thumbnail downloads.
        type Task = (Vec<String>, std::sync::mpsc::Sender<Result<Vec<Value>>>);
        let (jobs, incoming) = std::sync::mpsc::channel::<Task>();
        scope.spawn(move || {
            for (todo, response) in incoming {
                let _ = response.send(listing(net, uid, &todo));
            }
        });
        let (mut index, mut found, mut widened) = (0, 0, !probe);
        let mut ahead = VecDeque::new();
        loop {
            while ahead.len() < 2 && index < plan.todo.len() {
                let end = (index + 48).min(plan.todo.len());
                let batch = plan.todo[index..end].to_vec();
                index += 48;
                let (cached, todo) = prepare(c, lib, artist, &batch, known, progress, net, seq)?;
                found += plan.matches(cached, progress);
                if !todo.is_empty() {
                    let (response, future) = std::sync::mpsc::channel();
                    jobs.send((todo.clone(), response)).map_err(|_| invalid("Pixiv 列表线程已退出"))?;
                    ahead.push_back((todo, future));
                }
            }
            let Some((todo, future)) = ahead.pop_front() else {
                if !widened && found > 0 {
                    widened = true;
                    plan.add(plan.all.iter().take(300).cloned().collect::<Vec<_>>(), progress);
                    continue;
                }
                break;
            };
            let listed = future.recv().map_err(|_| invalid("Pixiv 列表线程未返回"))??;
            let hits = download(c, lib, net, artist, &todo, &listed, progress, seq)?;
            found += plan.matches(hits, progress);
        }
        Ok(found)
    })?;
    covered.extend(plan.seen);
    Ok(found)
}
fn worth(c: &Connection, request: &Request) -> Result<HashMap<String, (usize, usize)>> {
    let pictures:HashMap<String,usize>=c.prepare("SELECT artist_id,count(DISTINCT asset) FROM image_artists WHERE status IN ('auto','confirmed') GROUP BY artist_id")?.query_map([],|r|Ok((r.get(0)?,r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
    let mut owners = HashMap::<String, HashSet<String>>::new();
    for row in c
        .prepare("SELECT asset,artist_id FROM image_artists WHERE status IN ('auto','confirmed')")?
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
    {
        let (asset, artist) = row?;
        owners.entry(asset).or_default().insert(artist);
    }
    let mut open = HashMap::new();
    for (_, run) in groups::save_runs(request, &["pixiv"])? {
        let unresolved = run.iter().filter(|u| !owners.contains_key(*u)).count();
        let artists: BTreeSet<_> = run.iter().filter_map(|u| owners.get(u)).flatten().collect();
        for artist in artists {
            *open.entry(artist.clone()).or_insert(0) += unresolved;
        }
    }
    let artists: Vec<String> = c
        .prepare("SELECT DISTINCT artist_id FROM seeds")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(artists
        .into_iter()
        .map(|a| {
            let n = pictures.get(&a).copied().unwrap_or(0);
            let o = open.get(&a).copied().unwrap_or(0);
            (a, (n, o))
        })
        .collect())
}
fn popularity(
    c: &mut Connection,
    net: &Network,
    artists: &HashSet<String>,
    seq: &mut u64,
) -> Result<HashMap<String, i64>> {
    let mut tags = Vec::<(String, HashSet<String>)>::new();
    for row in c
        .prepare("SELECT artist_id,key FROM artist_links WHERE system='danbooru'")?
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
    {
        let (artist, tag) = row?;
        if !artists.contains(&artist) {
            continue;
        }
        if let Some((_, owners)) = tags.iter_mut().find(|(t, _)| *t == tag) {
            owners.insert(artist);
        } else {
            tags.push((tag, HashSet::from([artist])));
        }
    }
    let mut ask = vec![];
    for (tag, _) in &tags {
        let due:Option<bool>=c.query_row("SELECT next_due_at IS NULL OR next_due_at<=datetime('now') FROM lookups WHERE target=? AND backend='danbooru_count'",[format!("tag:{tag}")],|r|r.get(0)).optional()?;
        if due.unwrap_or(true) {
            ask.push(tag.clone());
        }
    }
    if !ask.is_empty() {
        local_match::progress(
            net.request,
            seq,
            json!({"log":format!("本地比对：查 {} 位画师在 Danbooru 的帖子数（判断是否热门）",ask.len())}),
        )?;
    }
    for batch in ask.chunks(50) {
        let answer = net.json(
            "https://danbooru.donmai.us/tags.json",
            &[("search[name_comma]", batch.join(",")), ("only", "name,post_count".into()), ("limit", "100".into())],
        )?;
        let mut counts = HashMap::new();
        if let Some(rows) = answer.as_array() {
            for r in rows {
                if let Some(name) = r["name"].as_str() {
                    counts.insert(name.to_owned(), r["post_count"].as_i64().unwrap_or(0));
                }
            }
        }
        let tx = c.transaction()?;
        for tag in batch {
            tx.execute("INSERT INTO lookups(target,backend,attempts,last_at,last_result,next_due_at,cursor) VALUES (?,'danbooru_count',1,datetime('now'),'hit',datetime('now','+30 days'),?) ON CONFLICT(target,backend) DO UPDATE SET attempts=attempts+1,last_at=excluded.last_at,next_due_at=excluded.next_due_at,cursor=excluded.cursor",params![format!("tag:{tag}"),counts.get(tag).copied().unwrap_or(0).to_string()])?;
        }
        tx.commit()?;
    }
    let mut out = HashMap::new();
    for row in c
        .prepare("SELECT target,cursor FROM lookups WHERE backend='danbooru_count'")?
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)))?
    {
        let (target, cursor) = row?;
        let count = cursor.unwrap_or_default();
        let count = if count.is_empty() { 0 } else { count.parse::<i64>().map_err(|e| invalid(e.to_string()))? };
        if let Some((_, artists)) = tags.iter().find(|(t, _)| Some(t.as_str()) == target.strip_prefix("tag:")) {
            for artist in artists {
                let n = out.entry(artist.clone()).or_insert(0);
                *n = (*n).max(count);
            }
        }
    }
    Ok(out)
}
fn execute(request: &Request, seq: &mut u64) -> Result<Value> {
    let mut lib = Library::open(&request.library)?;
    let mut c = super::staged_cache(&request.cache)?;
    let net = Network { request, used: std::sync::atomic::AtomicU64::new(0), gate: std::sync::Mutex::new(()) };
    let (mut processed, mut matched_artists, mut matches, mut cold) = (0usize, 0usize, 0usize, 0usize);
    let mut stopped = String::new();
    let mut todo = if !request.artists.is_empty() {
        request.artists.clone()
    } else {
        let rank="SELECT s.artist_id FROM seeds s LEFT JOIN (SELECT artist_id,count(*) n FROM image_artists GROUP BY artist_id) i ON i.artist_id=s.artist_id GROUP BY s.artist_id ORDER BY min(CASE s.source WHEN 'pixel_hash' THEN 0 WHEN 'iqdb' THEN 1 WHEN 'ocr' THEN 2 WHEN 'x_following' THEN 3 WHEN 'pixiv_following' THEN 4 WHEN 'pixiv_related' THEN 5 ELSE 6 END),coalesce(max(i.n),0) DESC,min(s.added_at),s.artist_id";
        let done:HashSet<String>=c.prepare("SELECT substr(target,8) FROM lookups WHERE backend='verify' AND last_result IN ('hit','miss') AND last_at>=datetime('now','-30 days')")?.query_map([],|r|r.get(0))?.collect::<rusqlite::Result<_>>()?;
        let candidates: Vec<String> = c.prepare(rank)?.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
        candidates.into_iter().filter(|a| !done.contains(a)).collect()
    };
    if request.artists.is_empty() {
        let worth = worth(&c, request)?;
        let weak: HashSet<_> = todo
            .iter()
            .filter(|a| {
                let (n, o) = worth.get(*a).copied().unwrap_or((0, 0));
                n < 3 && o < 2
            })
            .cloned()
            .collect();
        let posts = match popularity(&mut c, &net, &weak, seq) {
            Ok(p) => p,
            Err(Failure::Stop(e)) => {
                local_match::progress(
                    request,
                    seq,
                    json!({"log":format!("本地比对：没能查画师热度（{e}），只比对图多的画师")}),
                )?;
                HashMap::new()
            }
            Err(e) => return Err(e),
        };
        let before = todo.len();
        todo.retain(|a| {
            let (n, o) = worth.get(a).copied().unwrap_or((0, 0));
            n >= 3 || o >= 2 || posts.get(a).copied().unwrap_or(0) >= 1000
        });
        cold = before - todo.len();
        todo.sort_by_key(|a| {
            let (n, o) = worth.get(a).copied().unwrap_or((0, 0));
            (std::cmp::Reverse(n), std::cmp::Reverse(o), std::cmp::Reverse(posts.get(a).copied().unwrap_or(0)))
        });
    }
    let mut known = HashMap::new();
    for site in ["danbooru", "pixiv"] {
        let posts: HashSet<String> = c
            .prepare("SELECT DISTINCT post_id FROM image_sources WHERE site=?")?
            .query_map([site], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        known.insert(site, posts);
    }
    let weak:HashSet<String>=c.prepare("SELECT s.artist_id FROM seeds s GROUP BY s.artist_id HAVING sum(s.source NOT IN ('ocr','pixiv_related'))=0 AND NOT EXISTS (SELECT 1 FROM image_sources x WHERE x.artist_id=s.artist_id)")?.query_map([],|r|r.get(0))?.collect::<rusqlite::Result<_>>()?;
    let mut progress = Progress::new(todo.len());
    for artist in todo {
        let (mut found, mut skipped) = (0, false);
        let mut covered = HashSet::new();
        let probe = weak.contains(&artist);
        progress.works = 0;
        progress.planned = 0;
        progress.show(request, seq, net.used.load(std::sync::atomic::Ordering::Relaxed), true)?;
        let result = (|| -> Result<()> {
            let mut galleries = BTreeMap::new();
            for (system, key) in links(&c, &artist)? {
                if system == "danbooru" {
                    galleries.insert(key.clone(), posts(&mut c, &net, &artist, &key)?);
                }
            }
            progress.planned += galleries.values().map(Vec::len).sum::<usize>();
            for (system, key) in links(&c, &artist)? {
                if system == "pixiv" {
                    match pixiv(
                        &mut c,
                        &mut lib,
                        &net,
                        &artist,
                        &key,
                        &known["pixiv"],
                        &mut progress,
                        seq,
                        &mut covered,
                        probe,
                    ) {
                        Ok(n) => found += n,
                        Err(Failure::Stop(e)) => {
                            skipped = true;
                            stopped = e;
                        }
                        Err(e) => return Err(e),
                    }
                }
            }
            for items in galleries.values() {
                found += danbooru(
                    &mut c,
                    &mut lib,
                    &net,
                    &artist,
                    items,
                    &known["danbooru"],
                    &covered,
                    &mut progress,
                    seq,
                    probe,
                )?;
            }
            if !skipped {
                let tx = c.transaction()?;
                tx.execute("INSERT INTO lookups(target,backend,attempts,last_at,last_result,cursor) VALUES (?,'verify',1,datetime('now'),?,?) ON CONFLICT(target,backend) DO UPDATE SET attempts=attempts+1,last_at=excluded.last_at,last_result=excluded.last_result,cursor=excluded.cursor",params![format!("artist:{artist}"),if found>0{"hit"}else{"miss"},if probe && found==0 {Some("probe")}else{None}])?;
                tx.commit()?;
            }
            Ok(())
        })();
        match result {
            Ok(()) => {}
            Err(Failure::Stop(e)) => {
                stopped = e;
                break;
            }
            Err(e) => return Err(e),
        }
        processed += 1;
        matched_artists += usize::from(found > 0);
        matches += found;
        progress.done += 1;
        progress.show(request, seq, net.used.load(std::sync::atomic::Ordering::Relaxed), true)?;
    }
    Ok(json!({"artists":processed,"matched_artists":matched_artists,"matches":matches,"cold":cold,"stopped":stopped}))
}
pub(super) fn run(request: &Request, sequence: &mut u64) -> crate::Result<Value> {
    match execute(request, sequence) {
        Ok(v) => Ok(v),
        Err(Failure::Fatal(e)) => Err(e),
        Err(Failure::Stop(e)) => Err(Error::Invalid(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn listing_preserves_public_map_order_and_array_null_contract() {
        let r: Listing =
            serde_json::from_str(r#"{"body":{"works":{"10":{"id":"10"},"2":{"id":"2"},"1":{"id":"1"}}}}"#).unwrap();
        assert_eq!(
            r.body.unwrap().works.0.iter().map(|v| v["id"].as_str().unwrap()).collect::<Vec<_>>(),
            vec!["10", "2", "1"]
        );
        for json in [r#"{"body":{"works":null}}"#, r#"{"body":null}"#, r#"{}"#] {
            let r: Listing = serde_json::from_str(json).unwrap();
            assert!(r.body.map(|b| b.works.0).unwrap_or_default().is_empty());
        }
        let r: Listing = serde_json::from_str(r#"{"body":{"works":[{"id":"2"},{"id":"1"}]}}"#).unwrap();
        assert_eq!(r.body.unwrap().works.0[0]["id"], "2");
    }
}
