//! illustash-iqdb：找回画师的 IQDB 种子来源（docs/PROVENANCE.md 第 9 节）。
//!
//! 标准输入每行一张图 `{"asset": …, "path": …}`；对每张图在本地算 IQDB 签名，只把 `iqdb_…` 哈希文本发给
//! Danbooru 的 `iqdb_queries.json`，标准输出每行一个结果：
//!   {"asset", "status": "hit"|"miss"|"unreadable"|"error", "score", "post": {id, tag_string_artist, source}, "bytes"}
//! 低频（请求开始时刻至少相隔 `--pace` 秒；`--workers` 个请求可同时在途，省掉等待响应的时间）、只读；遇到 403、429 或维护页输出 {"stopped": 原因} 并退出，不重试、不绕过。
//! 超过 `--budget-bytes` 输出 {"budget": 已用字节} 并退出。入库和挑哪些图由调用方（provenance/sources/iqdb.py）负责。
mod signature;

use std::io::{self, BufRead, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::thread;
use std::thread::sleep;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

const URL: &str = "https://danbooru.donmai.us/iqdb_queries.json";

struct Args {
    hash_only: bool,
    pace: f64,
    workers: usize,
    budget: u64,
    hit: f64,
    user_agent: String,
}

fn args() -> Args {
    let mut a = Args {
        hash_only: false,
        pace: 1.0,
        workers: 1,
        budget: u64::MAX,
        hit: 80.0,
        user_agent: "Illustash/0.1 (+local library; provenance lookups)".into(),
    };
    let argv: Vec<String> = std::env::args().skip(1).collect();
    for pair in argv.chunks(2) {
        let value = pair.get(1).cloned().unwrap_or_default();
        match pair[0].as_str() {
            "--hash-only" => a.hash_only = value == "1",
            "--pace" => a.pace = value.parse().unwrap_or(a.pace),
            "--workers" => a.workers = value.parse().unwrap_or(a.workers),
            "--budget-bytes" => a.budget = value.parse().unwrap_or(a.budget),
            "--hit" => a.hit = value.parse().unwrap_or(a.hit),
            "--user-agent" => a.user_agent = value,
            other => eprintln!("未知参数 {other}"),
        }
    }
    a
}

fn emit(v: Value) {
    let mut out = io::stdout().lock();
    let _ = writeln!(out, "{v}");
    let _ = out.flush();
}

enum Answer {
    Matches(Vec<Value>, u64),
    Refused(String),
    Failed(String),
}

fn query(agent: &ureq::Agent, hash: &str, user_agent: &str) -> Answer {
    let mut last = String::new();
    for attempt in 0..3 {
        if attempt > 0 {
            sleep(Duration::from_secs(2 * attempt));
        }
        // `only` 对 IQDB 结果不起作用（返回的是空对象）：取完整的响应。
        let call = agent.get(URL).set("User-Agent", user_agent).query("hash", hash).query("limit", "3").call();
        match call {
            Ok(response) => {
                let body = match response.into_string() {
                    Ok(body) => body,
                    Err(e) => {
                        last = e.to_string();
                        continue;
                    }
                };
                let bytes = body.len() as u64;
                return match serde_json::from_str::<Value>(&body) {
                    Ok(Value::Array(items)) => Answer::Matches(items, bytes),
                    Ok(_) => Answer::Matches(vec![], bytes), // 返回的是错误提示而不是匹配：算没找到
                    Err(e) => Answer::Failed(format!("响应不是 JSON：{e}")),
                };
            }
            Err(ureq::Error::Status(code, response)) => {
                let text = response.into_string().unwrap_or_default().to_lowercase();
                if code == 403 || code == 429 || (code == 503 && text.contains("maintenance")) {
                    return Answer::Refused(format!("danbooru.donmai.us: HTTP {code}"));
                }
                last = format!("HTTP {code}");
            }
            Err(e) => last = e.to_string(),
        }
    }
    Answer::Failed(last)
}

/// 分发请求的开始时间，每 `pace` 秒最多一个，由各 worker 共用：可以有两个请求同时在途，
/// 但相邻两次开始的间隔不会小于 pace（服务器的延迟不再叠加上去）。
struct Slots {
    next: Mutex<Instant>,
    pace: Duration,
}

impl Slots {
    fn wait(&self) {
        let start = {
            let mut next = self.next.lock().expect("rate slot mutex poisoned");
            let start = (*next).max(Instant::now());
            *next = start + self.pace;
            start
        };
        sleep(start.saturating_duration_since(Instant::now()));
    }
}

fn work(
    a: &Args,
    agent: &ureq::Agent,
    input: &Mutex<io::Lines<io::BufReader<io::Stdin>>>,
    slots: &Slots,
    used: &AtomicU64,
    done: &AtomicBool,
) {
    loop {
        if done.load(Ordering::SeqCst) {
            return;
        }
        let Some(Ok(line)) = input.lock().expect("stdin mutex poisoned").next() else { return };
        let Ok(item) = serde_json::from_str::<Value>(&line) else { continue };
        let asset = item["asset"].clone();
        let img = match image::open(Path::new(item["path"].as_str().unwrap_or_default())) {
            Ok(img) => img,
            Err(e) => {
                emit(json!({"asset": asset, "status": "unreadable", "error": e.to_string()}));
                continue;
            }
        };
        let hash = signature::of_image(&img).to_hash();
        if a.hash_only {
            emit(json!({"asset":asset,"hash":hash}));
            continue;
        }
        slots.wait();
        if done.load(Ordering::SeqCst) {
            return;
        }
        match query(agent, &hash, &a.user_agent) {
            Answer::Matches(items, bytes) => {
                let total = used.fetch_add(bytes, Ordering::SeqCst) + bytes;
                let best = items.into_iter().max_by(|x, y| {
                    x["score"]
                        .as_f64()
                        .unwrap_or(0.0)
                        .partial_cmp(&y["score"].as_f64().unwrap_or(0.0))
                        .expect("JSON numbers are never NaN")
                });
                let score = best.as_ref().and_then(|b| b["score"].as_f64()).unwrap_or(0.0);
                let status = if score >= a.hit { "hit" } else { "miss" };
                let post = best
                    .map(|b| {
                        let p = &b["post"]; // 只留调用方要记录的，不要帖子的 46 个字段
                        json!({"id": p["id"], "tag_string_artist": p["tag_string_artist"], "source": p["source"]})
                    })
                    .unwrap_or(Value::Null);
                emit(json!({"asset": asset, "status": status, "score": score, "post": post, "bytes": bytes}));
                if total >= a.budget && !done.swap(true, Ordering::SeqCst) {
                    emit(json!({"budget": total}));
                }
            }
            Answer::Refused(why) => {
                if !done.swap(true, Ordering::SeqCst) {
                    emit(json!({"stopped": why}));
                }
                return;
            }
            Answer::Failed(why) => emit(json!({"asset": asset, "status": "error", "error": why})),
        }
    }
}

fn main() {
    let a = args();
    let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(30)).try_proxy_from_env(true).build();
    let input = Mutex::new(io::BufReader::new(io::stdin()).lines());
    let slots = Slots { next: Mutex::new(Instant::now()), pace: Duration::from_secs_f64(a.pace) };
    let (used, done) = (AtomicU64::new(0), AtomicBool::new(false));
    thread::scope(|scope| {
        for _ in 0..a.workers.max(1) {
            scope.spawn(|| work(&a, &agent, &input, &slots, &used, &done));
        }
    });
}
