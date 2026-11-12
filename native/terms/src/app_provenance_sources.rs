//! 固定的外部查询和机器缓存回执留在原生侧；签名在本地计算。
use super::{cache, net::Net};
use crate::{app_library::Library, app_tasks::TaskContext, Error, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};
fn address(path: &str, params: &[(&str, &str)]) -> String {
    let mut u =
        url::Url::parse(&format!("https://danbooru.donmai.us{path}")).expect("internal service URL must be valid");
    for (k, v) in params {
        u.query_pairs_mut().append_pair(k, v);
    }
    u.into()
}
pub(crate) fn artist(c: &Connection, links: &[(String, String, String)], source: &str) -> Result<String> {
    let mut owners = BTreeSet::<String>::new();
    for (system, key, _) in links {
        if let Some(id) = c
            .query_row("SELECT artist_id FROM artist_links WHERE system=? AND key=?", [system, key], |r| r.get(0))
            .optional()?
        {
            owners.insert(id);
        }
    }
    let id = owners.into_iter().next().unwrap_or(crate::store::new_key()?);
    c.execute("INSERT OR IGNORE INTO artists(id) VALUES (?)", [&id])?;
    for (system, key, url) in links {
        c.execute(
            "INSERT OR IGNORE INTO artist_links(system,key,artist_id,url,source) VALUES (?,?,?,?,?)",
            params![system, key, id, url, source],
        )?;
    }
    Ok(id)
}
pub(super) fn x_following(ctx: &TaskContext, path: &Path, net: &mut Net) -> Result<Value> {
    let mut c = cache::connect(path)?;
    let due = "
        SELECT key
        FROM leads
        WHERE source='x_following'
          AND (status='pending' OR (status='unresolved' AND next_due_at<=datetime('now')))
        ORDER BY added_at,key";
    let keys = c.prepare(due)?.query_map([], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
    let (mut asked, mut resolved, mut unresolved) = (0, 0, 0);
    let mut stopped = String::new();
    for key in keys {
        ctx.check()?;
        let query = format!("https://twitter.com/intent/user?user_id={key}");
        let found = match net
            .json(ctx, &address("/artists.json", &[("search[url_matches]", &query), ("only", "id,name,is_banned")]))
        {
            Ok(v) => v,
            Err(e) => {
                stopped = e.to_string();
                break;
            }
        };
        let items = found
            .as_array()
            .ok_or_else(|| Error::Network("画师列表响应无效".into()))?
            .iter()
            .filter(|v| v["is_banned"] != true)
            .collect::<Vec<_>>();
        let tx = c.transaction()?;
        if items.len() == 1 {
            let name = items[0]["name"].as_str().ok_or_else(|| Error::Network("画师名无效".into()))?;
            let id = artist(
                &tx,
                &[
                    ("danbooru".into(), name.into(), format!("https://danbooru.donmai.us/artists/{}", items[0]["id"])),
                    ("x_id".into(), key.clone(), format!("https://x.com/i/user/{key}")),
                ],
                "x_following",
            )?;
            tx.execute(
                "UPDATE leads SET status='resolved',artist_id=?,next_due_at=NULL WHERE source='x_following' AND key=?",
                params![id, key],
            )?;
            tx.execute(
                "INSERT OR IGNORE INTO seeds(artist_id,source,evidence) VALUES (?,'x_following',?)",
                params![id, json!({"x_id":key}).to_string()],
            )?;
            resolved += 1;
        } else {
            tx.execute("UPDATE leads SET status='unresolved',next_due_at=datetime('now','+30 days') WHERE source='x_following' AND key=?",[key])?;
            unresolved += 1;
        }
        tx.commit()?;
        asked += 1;
    }
    Ok(json!({"asked":asked,"resolved":resolved,"unresolved":unresolved,"stopped":stopped}))
}
pub(super) fn pixiv_following(ctx: &TaskContext, path: &Path, net: &mut Net) -> Result<Value> {
    let Some(uid) =
        net.binding.as_ref().and_then(|v| v["pixiv_user_id"].as_str()).filter(|s| !s.is_empty()).map(str::to_owned)
    else {
        return Ok(json!({"candidates":0,"pages":0,"stopped":"需要绑定 Pixiv 账号"}));
    };
    if let Some(reason) = net.pixiv_problem() {
        return Ok(json!({"candidates":0,"pages":0,"stopped":reason}));
    }
    let mut c = cache::connect(path)?;
    let target = format!("following:{uid}");
    let due = "
        SELECT next_due_at IS NULL OR next_due_at<=datetime('now')
        FROM lookups
        WHERE target=? AND backend='pixiv_following'";
    let due = c.query_row(due, [&target], |r| r.get::<_, bool>(0)).optional()?.unwrap_or(true);
    if !due {
        return Ok(json!({"candidates":0,"pages":0,"stopped":""}));
    }
    let mut offset = c
        .query_row("SELECT cursor FROM lookups WHERE target=? AND backend='pixiv_following'", [&target], |r| {
            r.get::<_, Option<String>>(0)
        })
        .optional()?
        .flatten()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(0);
    let (mut candidates, mut pages) = (0, 0);
    let mut stopped = String::new();
    loop {
        ctx.check()?;
        let base = format!("https://www.pixiv.net/ajax/user/{uid}/following");
        let params = json!({"offset":offset,"limit":100,"rest":"show","tag":"","lang":"ja"});
        let key = format!("{}:{}", base, crate::character_json::dump(&params, false));
        let old = c
            .query_row("SELECT body FROM ext_cache WHERE key=? AND fetched_at>datetime('now','-7 days')", [&key], |r| {
                r.get::<_, String>(0)
            })
            .optional()?;
        let result = if let Some(old) = old {
            serde_json::from_str(&old).map_err(|_| Error::Invalid("关注缓存无效".into()))?
        } else {
            let mut url = url::Url::parse(&base).expect("app_provenance_sources internal invariant");
            for (k, v) in params.as_object().expect("validated internal JSON object") {
                url.query_pairs_mut().append_pair(k, &v.as_str().map(str::to_owned).unwrap_or_else(|| v.to_string()));
            }
            match net.json(ctx, url.as_str()) {
                Ok(v) => v,
                Err(e) => {
                    stopped = e.to_string();
                    break;
                }
            }
        };
        let users = result["body"]["users"].as_array().ok_or_else(|| Error::Network("关注列表响应格式无效".into()))?;
        if users.len() > 100 {
            return Err(Error::Network("关注列表响应超过页限额".into()));
        }
        let tx = c.transaction()?;
        for user in users {
            let key = user["userId"]
                .as_str()
                .map(str::to_owned)
                .or_else(|| user["userId"].as_u64().map(|v| v.to_string()))
                .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
                .ok_or_else(|| Error::Network("Pixiv 用户 ID 无效".into()))?;
            let id = artist(
                &tx,
                &[("pixiv".into(), key.clone(), format!("https://www.pixiv.net/users/{key}"))],
                "pixiv_following",
            )?;
            tx.execute(
                "INSERT OR REPLACE INTO seeds(artist_id,source,evidence) VALUES (?,'pixiv_following',?)",
                params![id, json!({"version":1,"source":"pixiv_following","account":uid,"user_id":key}).to_string()],
            )?;
            if let Some(name) = user["userName"].as_str().map(str::trim).filter(|s| !s.is_empty()) {
                tx.execute("INSERT INTO artist_names(artist_id,source,name) VALUES (?,'pixiv',?) ON CONFLICT(artist_id,source) DO UPDATE SET name=excluded.name,updated_at=CURRENT_TIMESTAMP",params![id,name.chars().take(300).collect::<String>()])?;
            }
        }
        offset += users.len();
        let complete =
            users.is_empty() || offset >= result["body"]["total"].as_u64().unwrap_or((offset + 1) as u64) as usize;
        tx.execute("INSERT OR REPLACE INTO ext_cache VALUES (?,?,datetime('now'))", params![key, result.to_string()])?;
        tx.execute("INSERT INTO lookups(target,backend,attempts,last_at,last_result,next_due_at,cursor) VALUES (?,'pixiv_following',1,datetime('now'),?,CASE WHEN ? THEN datetime('now','+7 days') ELSE NULL END,?) ON CONFLICT(target,backend) DO UPDATE SET attempts=attempts+1,last_at=excluded.last_at,last_result=excluded.last_result,next_due_at=excluded.next_due_at,cursor=excluded.cursor",params![target,if users.is_empty(){"miss"}else{"hit"},complete,if complete{None}else{Some(offset.to_string())}])?;
        tx.commit()?;
        pages += 1;
        candidates += users.len();
        if complete {
            break;
        }
    }
    Ok(json!({"candidates":candidates,"pages":pages,"stopped":stopped}))
}

fn post(c: &Connection, asset: &str, p: &Value, method: &str, score: f64) -> Result<bool> {
    let mut links = Vec::new();
    for name in p["tag_string_artist"]
        .as_str()
        .unwrap_or("")
        .split_whitespace()
        .filter(|s| !["banned_artist", "unknown_artist", "anonymous_artist", "artist_request"].contains(s))
    {
        links.push((
            "danbooru".into(),
            name.into(),
            format!("https://danbooru.donmai.us/artists/show_or_new?name={name}"),
        ));
    }
    let original = p["source"].as_str().unwrap_or("");
    // 原帖解析由出处实现承担；账号与网络授权仍在服务层。
    let parsed = crate::origin::all().iter().filter_map(|o| o.parse_url(original)).collect::<Vec<_>>();
    for post in &parsed {
        if let Some((key, url)) = &post.artist {
            links.push((post.site.into(), key.clone(), url.clone()));
        }
    }
    let artist = if links.is_empty() {
        None
    } else {
        Some(artist(c, &links, if method == "pixel_hash" { "danbooru_post" } else { "iqdb" })?)
    };
    let id = p["id"].as_i64().ok_or_else(|| Error::Network("帖子 ID 无效".into()))?;
    let similarity = if method == "iqdb" { (score * 10.0).round() / 10.0 } else { 0.0 };
    c.execute("INSERT OR REPLACE INTO image_sources(asset,site,post_id,url,original,artist_id,method,similarity) VALUES (?,'danbooru',?,?,?,?,?,?)",params![asset,id.to_string(),format!("https://danbooru.donmai.us/posts/{id}"),original,artist,method,similarity])?;
    if let Some(post) = parsed.first() {
        c.execute("INSERT OR REPLACE INTO image_sources(asset,site,post_id,url,artist_id,method,similarity) VALUES (?,?,?,?,?,?,?)",params![asset,post.site,post.id,post.url,artist,method,similarity])?;
    }
    if let Some(artist) = artist {
        let data = if method == "pixel_hash" {
            json!({"danbooru_post":id})
        } else {
            json!({"danbooru_post":id,"score":similarity})
        };
        let sql = if method == "pixel_hash" {
            "INSERT OR REPLACE INTO image_artists(asset,artist_id,method,score,evidence,status)
             VALUES (?,?,?,?,?,'auto')"
        } else {
            "INSERT OR IGNORE INTO image_artists(asset,artist_id,method,score,evidence,status)
             VALUES (?,?,?,?,?,'auto')"
        };
        c.execute(
            sql,
            params![asset, artist, method, if method == "iqdb" { score / 100.0 } else { 1.0 }, data.to_string()],
        )?;
        let mut proof = data;
        proof["asset"] = asset.into();
        c.execute(
            "INSERT OR IGNORE INTO seeds(artist_id,source,evidence) VALUES (?,?,?)",
            params![artist, method, proof.to_string()],
        )?;
        return Ok(true);
    }
    Ok(false)
}
pub(super) fn hashes(ctx: &TaskContext, path: &Path, net: &mut Net, step: &str, v: &Value) -> Result<Value> {
    if v.get("stopped").is_some() || v.get("failed").is_some() {
        return Ok(v.clone());
    }
    let rows = v.as_array().ok_or_else(|| Error::Invalid("签名计算结果无效".into()))?;
    let mut c = cache::connect(path)?;
    if step == "pixel_hash" {
        pixel_hashes(ctx, &mut c, net, rows)
    } else {
        iqdb(ctx, &mut c, net, step, rows)
    }
}
/// 用图片的像素哈希在 Danbooru 上查找，每次五十张。
fn pixel_hashes(ctx: &TaskContext, c: &mut Connection, net: &mut Net, rows: &[Value]) -> Result<Value> {
    let (mut looked, mut hits, mut unreadable, mut failed) = (0, 0, 0, 0);
    let mut stopped = String::new();
    for chunk in rows.chunks(50) {
        let mut by_hash = BTreeMap::<String, Vec<String>>::new();
        for row in chunk {
            let asset = row["asset"].as_str().ok_or_else(|| Error::Invalid("签名缺少图片身份".into()))?;
            if let Some(hash) = row["hash"].as_str() {
                if hash.len() != 32 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return Err(Error::Invalid("像素哈希无效".into()));
                }
                by_hash.entry(hash.into()).or_default().push(asset.into());
            } else {
                unreadable += 1;
            }
        }
        if by_hash.is_empty() {
            continue;
        }
        let mut u = url::Url::parse("https://danbooru.donmai.us/media_assets.json")
            .expect("internal service URL must be valid");
        for hash in by_hash.keys() {
            u.query_pairs_mut().append_pair("search[pixel_hash][]", hash);
        }
        u.query_pairs_mut()
            .append_pair("only", "id,pixel_hash,post[id,tag_string_artist,source]")
            .append_pair("limit", "1000");
        let answer = match net.json(ctx, u.as_str()) {
            Ok(v) => v,
            Err(e) => {
                if !net.terminal_refusal() {
                    failed += by_hash.values().map(Vec::len).sum::<usize>();
                }
                stopped = e.to_string();
                break;
            }
        };
        let posts = answer.as_array().ok_or_else(|| Error::Network("像素哈希响应无效".into()))?;
        let tx = c.transaction()?;
        for (hash, assets) in by_hash {
            let found = posts.iter().filter(|v| v["pixel_hash"] == hash && v["post"].is_object()).collect::<Vec<_>>();
            let result = if found.is_empty() { "miss" } else { "hit" };
            for asset in assets {
                for p in &found {
                    post(&tx, &asset, &p["post"], "pixel_hash", 1.0)?;
                }
                tx.execute("INSERT INTO lookups(target,backend,attempts,last_at,last_result) VALUES (?,'pixel_hash',1,datetime('now'),?) ON CONFLICT(target,backend) DO UPDATE SET attempts=attempts+1,last_at=excluded.last_at,last_result=excluded.last_result",params![asset,result])?;
                looked += 1;
                if !found.is_empty() {
                    hits += 1;
                }
            }
        }
        tx.commit()?;
    }
    let misses = looked - hits;
    Ok(json!({
        "looked":looked,"hits":hits,"misses":misses,"unreadable":unreadable,"failed":failed,"stopped":stopped
    }))
}
/// 向 Danbooru 的 IQDB 查询每张图片的最佳匹配（最多 10,000 张），得分 80 起采用。
fn iqdb(ctx: &TaskContext, c: &mut Connection, net: &mut Net, step: &str, rows: &[Value]) -> Result<Value> {
    let (mut looked, mut hits, mut failed) = (0, 0, 0);
    let mut stopped = String::new();
    let mut artists = BTreeSet::new();
    for row in rows.iter().take(10000) {
        ctx.check()?;
        let asset = row["asset"].as_str().ok_or_else(|| Error::Invalid("签名缺少图片身份".into()))?;
        let result = if let Some(hash) = row["hash"].as_str() {
            if !hash.starts_with("iqdb_") || hash.len() > 3000 {
                return Err(Error::Invalid("IQDB 签名无效".into()));
            }
            let answer = match net.json(ctx, &address("/iqdb_queries.json", &[("hash", hash), ("limit", "3")])) {
                Ok(v) => v,
                Err(e) => {
                    if !net.terminal_refusal() {
                        failed += 1;
                    }
                    stopped = e.to_string();
                    break;
                }
            };
            let score = |p: &Value| p["score"].as_f64().unwrap_or(0.0);
            let best = answer.as_array().into_iter().flatten().max_by(|a, b| score(a).total_cmp(&score(b)));
            if let Some(p) = best.filter(|p| score(p) >= 80.0 && p["post"].is_object()) {
                let tx = c.transaction()?;
                let score = p["score"].as_f64().expect("validated internal numeric field");
                if post(&tx, asset, &p["post"], step, score)? {
                    artists.insert(p["post"]["tag_string_artist"].to_string());
                }
                tx.commit()?;
                hits += 1;
                "hit"
            } else {
                "miss"
            }
        } else {
            "unreadable"
        };
        let delay = match result {
            "miss" => "+90 days",
            "unreadable" => "+7 days",
            _ => "+1 hour",
        };
        let result = if result == "unreadable" { "miss" } else { result };
        c.execute("INSERT INTO lookups(target,backend,attempts,last_at,last_result,next_due_at) VALUES (?,'iqdb',1,datetime('now'),?,datetime('now',?)) ON CONFLICT(target,backend) DO UPDATE SET attempts=attempts+1,last_at=excluded.last_at,last_result=excluded.last_result,next_due_at=excluded.next_due_at",params![asset,result,delay])?;
        looked += 1;
    }
    Ok(json!({"asked":looked,"hits":hits,"artists":artists.len(),"failed":failed,"stopped":stopped}))
}
pub(super) fn report(lib: &Library, path: &Path) -> Result<Value> {
    let s = crate::app_library::read_connection(&lib.source())?;
    let ids = crate::origin::all().iter().map(|o| o.id()).collect::<Vec<_>>();
    let marks = vec!["?"; ids.len()].join(",");
    let pictures = s
        .prepare(&format!(
            "SELECT uuid,preset FROM assets WHERE in_scope=1 AND coalesce(trashed,0)=0 AND preset IN ({marks})"
        ))?
        .query_map(rusqlite::params_from_iter(ids), |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<BTreeMap<_, _>>>()?;
    let c = cache::connect(path)?;
    let mut total = BTreeMap::<String, usize>::new();
    let mut have = BTreeMap::<String, usize>::new();
    let mut by_source = BTreeMap::<String, usize>::new();
    let mut posts = BTreeSet::new();
    let mut owned = BTreeMap::<String, BTreeSet<String>>::new();
    for (asset, artist) in c
        .prepare("SELECT asset,artist_id FROM image_artists WHERE status='auto'")?
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?
    {
        owned.entry(asset).or_default().insert(artist);
    }
    for (asset, preset) in &pictures {
        *total.entry(preset.clone()).or_default() += 1;
        if owned.contains_key(asset) {
            *have.entry(preset.clone()).or_default() += 1;
        }
    }
    let sites = crate::origin::all().iter().filter(|o| o.tracks_original_posts()).map(|o| o.id()).collect::<Vec<_>>();
    let marks = vec!["?"; sites.len()].join(",");
    for asset in c
        .prepare(&format!("SELECT DISTINCT asset FROM image_sources WHERE site IN ({marks})"))?
        .query_map(rusqlite::params_from_iter(sites), |r| r.get::<_, String>(0))?
    {
        let a = asset?;
        if pictures.contains_key(&a) {
            posts.insert(a);
        }
    }
    for (asset, artists) in &owned {
        if !pictures.contains_key(asset) {
            continue;
        }
        let mut sources = BTreeSet::new();
        for id in artists {
            for r in
                c.prepare("SELECT source FROM seeds WHERE artist_id=?")?.query_map([id], |r| r.get::<_, String>(0))?
            {
                sources.insert(r?);
            }
        }
        for source in sources {
            *by_source.entry(source).or_default() += 1;
        }
    }
    let formatted = total
        .iter()
        .map(|(k, n)| {
            (
                k.clone(),
                format!(
                    "{} ({:.1}%)",
                    have.get(k).unwrap_or(&0),
                    *have.get(k).unwrap_or(&0) as f64 / *n as f64 * 100.0
                ),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let with_post = format!("{} ({:.1}%)", posts.len(), posts.len() as f64 / pictures.len().max(1) as f64 * 100.0);
    let conflicts = owned.iter().filter(|(a, s)| pictures.contains_key(*a) && s.len() > 1).count();
    Ok(json!({
        "pictures":total,"with_artist":formatted,"with_original_post":with_post,"by_seed_source":by_source,
        "conflicts":conflicts,"numbers":{"pictures":total,"with_artist":have,"with_original_post":posts.len()}
    }))
}

pub(super) fn validate_hashes(lib: &Library, path: &Path, step: &str, value: &Value, only: &[String]) -> Result<Value> {
    if !value.is_array() {
        return Ok(value.clone());
    }
    let c = crate::app_library::read_connection(&lib.db())?;
    let present = c
        .prepare("SELECT uuid FROM assets")?
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<BTreeSet<_>>>()?;
    let cache = cache::connect(path)?;
    let mut seen = BTreeSet::new();
    let mut rows = Vec::new();
    for row in value.as_array().expect("validated internal JSON array") {
        let asset = row["asset"].as_str().ok_or_else(|| Error::Invalid("签名缺少图片身份".into()))?;
        if !present.contains(asset)
            || (!only.is_empty() && !only.iter().any(|s| s == asset))
            || !seen.insert(asset.to_owned())
        {
            return Err(Error::Invalid("签名结果超出当前任务资产范围或重复".into()));
        }
        let done = if step == "pixel_hash" {
            let looked = "
                SELECT EXISTS(
                  SELECT 1 FROM lookups WHERE target=? AND backend='pixel_hash' AND last_result IN ('hit','miss'))";
            cache.query_row(looked, [asset], |r| r.get::<_, bool>(0))?
        } else {
            cache.query_row(
                "SELECT EXISTS(SELECT 1 FROM image_artists WHERE asset=? AND status='auto')",
                [asset],
                |r| r.get::<_, bool>(0),
            )?
        };
        if !done {
            rows.push(row.clone());
        }
    }
    Ok(Value::Array(rows))
}

#[cfg(test)]
#[path = "../tests/support/origin_sources.rs"]
mod origin_tests;
