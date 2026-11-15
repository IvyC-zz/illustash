//! 联网同意、宿主白名单、节流、拒绝处理和共享的单次运行配额都由 Rust 负责。
use crate::{app_tasks::TaskContext, Error, Result};
use serde_json::Value;
use std::{
    collections::HashMap,
    io::Read,
    time::{Duration, Instant},
};
pub(super) struct Net {
    pub authorized: bool,
    pub used: usize,
    budget: usize,
    agent: ureq::Agent,
    last: HashMap<String, Instant>,
    stopped: HashMap<String, String>,
    /// Pixiv 对已登录的请求返回 401：登录已过期（#204：按这一点判断，不看文字）。
    login_expired: bool,
    base: Option<String>,
    pub binding: Option<Value>,
    session: Option<String>,
}
impl Net {
    pub fn new(authorized: bool, budget_mb: usize) -> Result<Self> {
        let base = std::env::var("ILLUSTASH_PROVENANCE_BASE").ok();
        if let Some(base) = &base {
            let u = url::Url::parse(base).map_err(|_| Error::Invalid("合成服务地址无效".into()))?;
            if u.scheme() != "http"
                || !matches!(u.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))
                || u.port().is_none()
                || u.path() != "/"
                || u.query().is_some()
                || !u.username().is_empty()
                || u.password().is_some()
            {
                return Err(Error::Invalid("溯源测试服务只允许本机 HTTP 端口".into()));
            }
        }
        Ok(Self {
            authorized,
            used: 0,
            budget: budget_mb * 1048576,
            agent: ureq::AgentBuilder::new().redirects(0).timeout(Duration::from_secs(30)).build(),
            last: HashMap::new(),
            stopped: HashMap::new(),
            login_expired: false,
            base,
            binding: None,
            session: None,
        })
    }
    pub fn get(&mut self, ctx: &TaskContext, address: &str) -> Result<(u16, Vec<u8>)> {
        ctx.check()?;
        if !self.authorized {
            return Err(Error::Invalid("外部请求未授权".into()));
        }
        if address.len() > 8192 {
            return Err(Error::Invalid("外部查询参数过大".into()));
        }
        let url = url::Url::parse(address).map_err(|_| Error::Invalid("外部地址无效".into()))?;
        let host = url.host_str().unwrap_or("");
        let path = url.path();
        let allowed = match host {
            "danbooru.donmai.us" => {
                ["/media_assets.json", "/iqdb_queries.json", "/artists.json", "/posts.json", "/tags.json"]
                    .contains(&path)
            }
            "www.pixiv.net" => path.starts_with("/ajax/user/") && !path.contains(".."),
            "i.pximg.net" | "cdn.donmai.us" => true,
            _ => false,
        };
        if !allowed
            || url.scheme() != "https"
            || url.port().is_some()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
        {
            return Err(Error::Invalid("溯源外部地址超出只读查询范围".into()));
        }
        if let Some(reason) = self.stopped.get(host) {
            return Err(Error::Network(reason.clone()));
        }
        if self.used >= self.budget {
            return Err(Error::Network("达到本次流量上限".into()));
        }
        let gap = match host {
            "www.pixiv.net" => 2.0,
            "i.pximg.net" | "cdn.donmai.us" => 0.25,
            _ => {
                if path == "/iqdb_queries.json" {
                    0.5
                } else {
                    0.35
                }
            }
        };
        let target = if let Some(base) = &self.base {
            format!(
                "{}{}{}",
                base.trim_end_matches('/'),
                path,
                url.query().map(|s| format!("?{s}")).unwrap_or_default()
            )
        } else {
            address.into()
        };
        for attempt in 0..3 {
            if attempt > 0 {
                pause(ctx, Duration::from_secs(2 * attempt))?;
            }
            if let Some(last) = self.last.get(host) {
                pause(ctx, Duration::from_secs_f64(gap).saturating_sub(last.elapsed()))?;
            }
            self.last.insert(host.into(), Instant::now());
            let in_app = self.binding.as_ref().is_some_and(|v| v["pixiv_in_app"] == true);
            let (code, bytes) = if host == "www.pixiv.net" && in_app {
                let route = format!(
                    "/pixiv/fetch?path={}",
                    url::form_urlencoded::byte_serialize(
                        format!("{}{}", path, url.query().map(|q| format!("?{q}")).unwrap_or_default()).as_bytes()
                    )
                    .collect::<String>()
                );
                let reply = crate::app_accounts::desktop("GET", &route, 120)?;
                (
                    reply.get("status").map(crate::http_json::Json::integer).unwrap_or(0) as u16,
                    reply.get("body").map(crate::http_json::Json::text).unwrap_or("").as_bytes().to_vec(),
                )
            } else {
                let mut request =
                    self.agent.get(&target).set("User-Agent", "Illustash/0.1 (+local library; provenance lookups)");
                if host == "i.pximg.net" {
                    request = request.set("Referer", "https://www.pixiv.net/");
                }
                if host == "www.pixiv.net" {
                    if let Some(cookie) = &self.session {
                        request = request.set("Cookie", &format!("PHPSESSID={cookie}"));
                    }
                }
                let call = request.call();
                let response = match call {
                    Ok(r) => r,
                    Err(ureq::Error::Status(_, r)) => r,
                    Err(_) => {
                        if attempt == 2 {
                            return Err(Error::Network("外部查询失败，下次重试".into()));
                        }
                        continue;
                    }
                };
                let code = response.status();
                let mut bytes = Vec::new();
                let cap = (self.budget - self.used).min(20 * 1048576);
                response.into_reader().take(cap as u64 + 1).read_to_end(&mut bytes)?;
                (code, bytes)
            };
            let cap = (self.budget - self.used).min(20 * 1048576);
            self.used += bytes.len();
            ctx.check()?;
            if bytes.len() > cap {
                return Err(Error::Network("达到本次流量或单响应上限".into()));
            }
            let text = String::from_utf8_lossy(&bytes[..bytes.len().min(4096)]).to_lowercase();
            if [403, 429].contains(&code)
                || (code == 503 && text.contains("maintenance"))
                || text.contains("just a moment")
                || text.contains("cf-mitigated")
            {
                let reason = format!("{host}: HTTP {code}，已停止此站点");
                self.stopped.insert(host.into(), reason.clone());
                return Err(Error::Network(reason));
            }
            if code == 401 && host == "www.pixiv.net" && (in_app || self.session.is_some()) {
                let reason = "登录已失效（HTTP 401）";
                crate::app_accounts::mark_pixiv(None, Some(crate::app_accounts::PIXIV_EXPIRED), Some(reason))?;
                self.stopped.insert(host.into(), reason.into());
                self.login_expired = true;
                return Err(Error::Network(reason.into()));
            }
            if (200..300).contains(&code) || [401, 404].contains(&code) {
                return Ok((code, bytes));
            }
            if attempt == 2 {
                return Err(Error::Network(format!("{host}: HTTP {code}")));
            }
        }
        unreachable!()
    }
    pub fn prepare_pixiv(&mut self, ctx: &TaskContext, kind: &str) -> Result<()> {
        if !self.authorized {
            return Ok(());
        }
        let metadata = crate::app_accounts::pixiv_binding()?;
        let in_app = metadata["via"] == "app";
        if !in_app {
            self.session = crate::app_accounts::secret("pixiv")?;
        }
        let uid = if in_app {
            metadata["user_id"].as_str().map(str::to_owned)
        } else {
            self.session.as_ref().map(|s| s.split('_').next().unwrap_or("").to_owned())
        };
        let binding = serde_json::json!({"pixiv_in_app":in_app, "pixiv_user_id":uid,
            "desktop":crate::app_accounts::desktop_available()});
        self.binding = Some(binding.clone());
        if in_app && binding["desktop"] != true {
            self.stopped.insert("www.pixiv.net".into(), crate::app_accounts::PIXIV_NO_WINDOW.into());
            return Ok(());
        }
        let Some(uid) =
            binding["pixiv_user_id"].as_str().filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        else {
            return Ok(());
        };
        let mut refused = false;
        let result = (|| {
            let extra = self.json(ctx, "https://www.pixiv.net/ajax/user/extra?lang=ja")?;
            if extra["error"] == true || extra.is_null() {
                refused = true;
                return Err(Error::Network("Pixiv 没有接受这个登录状态".into()));
            }
            let user = self.json(ctx, &format!("https://www.pixiv.net/ajax/user/{uid}?lang=ja"))?;
            crate::app_accounts::mark_pixiv(
                Some(
                    &serde_json::json!({"user_id":uid,"name":user["body"]["name"],"following":extra["body"]["following"]}),
                ),
                None,
                None,
            )?;
            Ok(())
        })();
        if let Err(e) = result {
            let msg = e.to_string();
            if refused || self.login_expired {
                crate::app_accounts::mark_pixiv(None, Some(crate::app_accounts::PIXIV_EXPIRED), Some(&msg))?;
                self.stopped.insert("www.pixiv.net".into(), crate::app_accounts::PIXIV_EXPIRED.into());
                if in_app && kind != "new" {
                    let _ = crate::app_accounts::desktop("POST", "/pixiv/login?force=1", 30);
                }
            } else {
                self.stopped.insert("www.pixiv.net".into(), msg);
            }
        }
        Ok(())
    }
    pub fn pixiv_problem(&self) -> Option<&str> {
        self.stopped.get("www.pixiv.net").map(String::as_str)
    }
    pub fn terminal_refusal(&self) -> bool {
        self.used >= self.budget || !self.stopped.is_empty()
    }
    pub fn json(&mut self, ctx: &TaskContext, address: &str) -> Result<Value> {
        let (code, bytes) = self.get(ctx, address)?;
        if [401, 404].contains(&code) {
            return Ok(Value::Null);
        }
        serde_json::from_slice(&bytes).map_err(|_| Error::Network("外部响应不是有效 JSON".into()))
    }
}
pub(super) fn pause(ctx: &TaskContext, duration: Duration) -> Result<()> {
    let until = Instant::now() + duration;
    while Instant::now() < until {
        ctx.check()?;
        std::thread::sleep(until.saturating_duration_since(Instant::now()).min(Duration::from_millis(20)));
    }
    ctx.check()
}
