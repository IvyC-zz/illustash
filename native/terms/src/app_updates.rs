//! Update check, tier A (#191, decided in #165): ask GitHub for the newest Release, compare it with this build's
//! version (native/terms/application.json) and let the page link to the Release page; nothing is downloaded or
//! replaced. Network rules as everywhere else: automatic checks are off until the person allows them (asked once by
//! the new-library guide, changeable in 设置 › 网络与隐私); once allowed, the app checks at each launch and again
//! whenever a day has passed while it stays open (user 2026-10-18: a Mac app may run for weeks). A manual check is
//! the person's click.
//! The request is a plain HTTPS GET of the public releases list: no account, no library information.
//!
//! App-wide like the interface language: the choice lives in preferences.json (`update_check`: `"auto"` or
//! `"manual"`, absent until asked); the last result in updates.json beside it.
use crate::{
    app_paths::{app_support, user_home},
    http_json::Json,
    Error, Result,
};
use serde_json::{Map, Value};
use std::{
    cmp::Ordering,
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering as Atomic},
    time::Duration,
};

const REPOSITORY: &str = "shio-vinyl/illustash";
const API: &str = "https://api.github.com/";
/// Whether this launch (this host process) has made its automatic check; every page that opens asks, one checks.
static LAUNCH_CHECKED: AtomicBool = AtomicBool::new(false);
/// While the app stays open, automatic checks repeat after this many seconds.
const DAY: i64 = 24 * 60 * 60;
/// The releases list is small; anything larger is not GitHub's answer.
const LIMIT: u64 = 2_000_000;

/// SemVer 2.0.0 precedence without build metadata (tools/version.py writes the same grammar).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Version {
    core: [u64; 3],
    pre: Vec<String>,
}
impl Version {
    pub(crate) fn parse(text: &str) -> Option<Self> {
        let (core, pre) = match text.split_once('-') {
            Some((core, pre)) => (core, Some(pre)),
            None => (text, None),
        };
        let number = |part: &str| {
            (!part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()) && (part == "0" || !part.starts_with('0')))
                .then(|| part.parse().ok())
                .flatten()
        };
        let mut parts = core.split('.');
        let core = [number(parts.next()?)?, number(parts.next()?)?, number(parts.next()?)?];
        if parts.next().is_some() {
            return None;
        }
        let pre = match pre {
            None => Vec::new(),
            Some(pre) => {
                let ids: Vec<String> = pre.split('.').map(str::to_owned).collect();
                let valid = |id: &String| {
                    !id.is_empty()
                        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                        && (!id.bytes().all(|b| b.is_ascii_digit()) || id == "0" || !id.starts_with('0'))
                };
                if !ids.iter().all(valid) {
                    return None;
                }
                ids
            }
        };
        Some(Self { core, pre })
    }
    pub(crate) fn prerelease(&self) -> bool {
        !self.pre.is_empty()
    }
}
impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        self.core.cmp(&other.core).then_with(|| match (self.pre.is_empty(), other.pre.is_empty()) {
            (true, true) => Ordering::Equal,
            (true, false) => Ordering::Greater,
            (false, true) => Ordering::Less,
            (false, false) => {
                for (a, b) in self.pre.iter().zip(&other.pre) {
                    let order = match (a.parse::<u64>(), b.parse::<u64>()) {
                        (Ok(a), Ok(b)) => a.cmp(&b),
                        (Ok(_), Err(_)) => Ordering::Less,
                        (Err(_), Ok(_)) => Ordering::Greater,
                        (Err(_), Err(_)) => a.cmp(b),
                    };
                    if order != Ordering::Equal {
                        return order;
                    }
                }
                self.pre.len().cmp(&other.pre.len())
            }
        })
    }
}
impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let [major, minor, patch] = self.core;
        write!(f, "{major}.{minor}.{patch}")?;
        if self.prerelease() {
            write!(f, "-{}", self.pre.join("."))?;
        }
        Ok(())
    }
}

/// The newest release worth offering to a build at `current`: no drafts; prereleases (by GitHub's flag or a
/// `-beta.1` tag) only to someone already on a prerelease. Tags are `vX.Y.Z`; others are ignored.
pub(crate) fn newest(releases: &Value, current: &Version) -> Option<Version> {
    releases
        .as_array()?
        .iter()
        .filter(|r| !r.get("draft").and_then(Value::as_bool).unwrap_or(false))
        .filter_map(|r| {
            let version = Version::parse(r.get("tag_name")?.as_str()?.strip_prefix('v')?)?;
            let pre = version.prerelease() || r.get("prerelease").and_then(Value::as_bool).unwrap_or(false);
            (!pre || current.prerelease()).then_some(version)
        })
        .max()
}

/// The Release page for a version, built here rather than taken from the reply: the page only ever opens this site.
pub(crate) fn page(version: &Version) -> String {
    format!("https://github.com/{REPOSITORY}/releases/tag/v{version}")
}

pub(crate) fn current() -> Version {
    Version::parse(env!("ILLUSTASH_VERSION")).expect("application.json version is SemVer (tools/version.py)")
}

fn support() -> Result<PathBuf> {
    Ok(app_support(&user_home()?))
}
pub(crate) fn read(file: &Path) -> Map<String, Value> {
    fs::read_to_string(file)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|value| match value {
            Value::Object(fields) => Some(fields),
            _ => None,
        })
        .unwrap_or_default()
}
/// Read-modify-write under the same lock and rename as libraries.json; other keys are kept.
pub(crate) fn update(file: &Path, change: impl FnOnce(&mut Map<String, Value>)) -> Result<()> {
    crate::app_libraries::registry_locked(file, || {
        let mut fields = read(file);
        change(&mut fields);
        let text =
            serde_json::to_string_pretty(&Value::Object(fields)).map_err(|e| Error::Invalid(e.to_string()))? + "\n";
        fs::create_dir_all(file.parent().ok_or_else(|| Error::Invalid("无效的偏好设置路径".into()))?)?;
        let tmp = file.with_extension("tmp");
        fs::write(&tmp, text)?;
        fs::rename(&tmp, file).inspect_err(|_| {
            let _ = fs::remove_file(&tmp);
        })?;
        Ok(())
    })
}

/// Where the state lives and what time it is: real files and the clock in the app, a fixture in tests.
struct Place<'a> {
    preferences: PathBuf,
    state: PathBuf,
    now: chrono::DateTime<chrono::FixedOffset>,
    launch: &'a AtomicBool,
}
impl Place<'static> {
    fn app() -> Result<Self> {
        let support = support()?;
        Ok(Self {
            preferences: support.join("preferences.json"),
            state: support.join("updates.json"),
            now: chrono::Local::now().fixed_offset(),
            launch: &LAUNCH_CHECKED,
        })
    }
}
impl Place<'_> {
    /// `Some(true)` automatic, `Some(false)` only by hand, `None` never asked.
    fn auto(&self) -> Option<bool> {
        match read(&self.preferences).get("update_check").and_then(Value::as_str) {
            Some("auto") => Some(true),
            Some("manual") => Some(false),
            _ => None,
        }
    }
    /// A day since the last check, by any launch; a clock set back also counts, rather than blocking checks for good.
    fn stale(&self) -> bool {
        let state = read(&self.state);
        let at =
            state.get("checked_at").and_then(Value::as_str).and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok());
        at.is_none_or(|at| !(0..DAY).contains(&(self.now - at).num_seconds()))
    }
    /// Allowed, and either not yet checked in this launch or a day has passed.
    fn due(&self) -> bool {
        self.auto() == Some(true) && (!self.launch.load(Atomic::SeqCst) || self.stale())
    }
    /// Takes the automatic check that is due: this launch's first (exactly one caller gets it), or the daily one.
    fn claim(&self) -> bool {
        self.auto() == Some(true)
            && (self.launch.compare_exchange(false, true, Atomic::SeqCst, Atomic::SeqCst).is_ok() || self.stale())
    }
    fn info(&self, current: &Version) -> Json {
        let state = read(&self.state);
        let latest = state.get("latest").and_then(Value::as_str).and_then(Version::parse);
        let newer = latest.as_ref().filter(|latest| *latest > current);
        let text = |key: &str| state.get(key).and_then(Value::as_str).map(str::to_owned);
        Json::object([
            ("current", current.to_string().into()),
            ("auto", self.auto().into()),
            ("checked_at", text("checked_at").into()),
            ("latest", latest.as_ref().map(Version::to_string).into()),
            ("newer", newer.is_some().into()),
            ("url", newer.map(page).into()),
            ("dismissed", (newer.is_some() && text("dismissed") == latest.as_ref().map(Version::to_string)).into()),
            ("error", text("error").into()),
            ("due", self.due().into()),
            ("address", format!("{API}repos/{REPOSITORY}/releases").into()),
        ])
    }
    fn record(&self, result: std::result::Result<Option<Version>, String>) -> Result<()> {
        let at = self.now.to_rfc3339_opts(chrono::SecondsFormat::Secs, false);
        update(&self.state, |fields| {
            fields.insert("checked_at".into(), at.into());
            match result {
                Ok(latest) => {
                    fields.insert("latest".into(), latest.map(|v| v.to_string()).into());
                    fields.remove("error");
                }
                // A failed check keeps the last known release.
                Err(message) => {
                    fields.insert("error".into(), message.into());
                }
            }
        })
    }
}

/// The API root: GitHub, or a local test server named by ILLUSTASH_UPDATE_BASE (plain HTTP on this machine only).
fn base() -> std::result::Result<String, String> {
    let Ok(base) = std::env::var("ILLUSTASH_UPDATE_BASE") else { return Ok(API.into()) };
    let u = url::Url::parse(&base).map_err(|_| "合成服务地址无效".to_string())?;
    if u.scheme() != "http"
        || !matches!(u.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))
        || u.port().is_none()
        || u.path() != "/"
        || u.query().is_some()
        || !u.username().is_empty()
        || u.password().is_some()
    {
        return Err("检查更新的测试服务只允许本机 HTTP 端口".into());
    }
    Ok(base)
}

/// One GET of the releases list. Failures become a short message for the page, never an error dialog.
fn fetch(current: &Version) -> std::result::Result<Option<Version>, String> {
    let address = format!("{}repos/{REPOSITORY}/releases?per_page=30", base()?);
    let agent = ureq::AgentBuilder::new().redirects(0).timeout(Duration::from_secs(15)).build();
    let reply = agent
        .get(&address)
        .set("Accept", "application/vnd.github+json")
        .set("User-Agent", "Illustash-update-check")
        .call();
    let reply = match reply {
        Ok(reply) => reply,
        Err(ureq::Error::Status(403 | 429, _)) => return Err("GitHub 暂时拒绝了请求，请稍后再试".into()),
        Err(ureq::Error::Status(code, _)) => return Err(format!("GitHub 返回了错误（HTTP {code}）")),
        Err(ureq::Error::Transport(_)) => return Err("连不上 GitHub，请检查网络后再试".into()),
    };
    let mut bytes = Vec::new();
    reply.into_reader().take(LIMIT + 1).read_to_end(&mut bytes).map_err(|_| "读取 GitHub 的回复失败".to_string())?;
    if bytes.len() as u64 > LIMIT {
        return Err("GitHub 的回复不是版本列表".into());
    }
    let releases: Value = serde_json::from_slice(&bytes).map_err(|_| "GitHub 的回复不是版本列表".to_string())?;
    if !releases.is_array() {
        return Err("GitHub 的回复不是版本列表".into());
    }
    Ok(newest(&releases, current))
}

/// GET /api/updates.
pub(crate) fn info() -> Result<Json> {
    Ok(Place::app()?.info(&current()))
}

/// POST /api/updates:
/// - `{"action": "auto", "allow": true|false}` records the choice (the guide's question or the settings switch);
/// - `{"action": "check"}` checks now (the person's click);
/// - `{"action": "check", "auto": true}` checks only when allowed and not yet checked in this launch, else just answers;
/// - `{"action": "dismiss"}` stops offering the newest known release (a newer one is offered again).
pub(crate) fn post(body: &Json) -> Result<Json> {
    post_at(&Place::app()?, body, &current(), fetch)
}

fn post_at(
    place: &Place,
    body: &Json,
    current: &Version,
    fetch: impl FnOnce(&Version) -> std::result::Result<Option<Version>, String>,
) -> Result<Json> {
    let Json::Object(fields) = body else { return Err(Error::Invalid("未知操作".into())) };
    let get = |key: &str| fields.iter().find(|(k, _)| k == key).map(|(_, v)| v);
    let flag = |key: &str| match get(key) {
        None => Ok(None),
        Some(Json::Scalar(Value::Bool(value))) => Ok(Some(*value)),
        Some(_) => Err(Error::Invalid("参数取值不正确".into())),
    };
    let allowed: &[&str] = match get("action").map(Json::text) {
        Some("auto") => &["action", "allow"],
        Some("check") => &["action", "auto"],
        Some("dismiss") => &["action"],
        _ => return Err(Error::Invalid("未知操作".into())),
    };
    if fields.iter().any(|(key, _)| !allowed.contains(&key.as_str())) {
        return Err(Error::Invalid("未知参数".into()));
    }
    match get("action").map(Json::text) {
        Some("auto") => {
            let allow = flag("allow")?.ok_or_else(|| Error::Invalid("参数取值不正确".into()))?;
            update(&place.preferences, |fields| {
                fields.insert("update_check".into(), if allow { "auto" } else { "manual" }.into());
            })?;
        }
        Some("check") => {
            if flag("auto")? != Some(true) || place.claim() {
                place.record(fetch(current))?;
            }
        }
        _ => {
            let latest = read(&place.state).get("latest").cloned().unwrap_or(Value::Null);
            update(&place.state, |fields| {
                fields.insert("dismissed".into(), latest);
            })?;
        }
    }
    Ok(place.info(current))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::cell::Cell;
    use std::sync::atomic::{AtomicU64, Ordering as Atomic};
    static NEXT: AtomicU64 = AtomicU64::new(0);

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap_or_else(|| panic!("{text}"))
    }
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "illustash-updates-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Atomic::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
        /// The app at `now`, in the launch whose automatic check is `launch`.
        fn at<'a>(&self, now: &str, launch: &'a AtomicBool) -> Place<'a> {
            Place {
                preferences: self.0.join("preferences.json"),
                state: self.0.join("updates.json"),
                now: chrono::DateTime::parse_from_rfc3339(now).unwrap(),
                launch,
            }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn body(value: Value) -> Json {
        crate::http_json::parse_request(&value.to_string()).unwrap()
    }
    fn field(info: &Json, key: &str) -> Value {
        serde_json::from_str(&info.get(key).unwrap().dump()).unwrap()
    }

    #[test]
    fn versions_follow_semver_precedence() {
        let order = [
            "0.1.0",
            "0.1.1",
            "0.2.0-alpha",
            "0.2.0-alpha.1",
            "0.2.0-alpha.beta",
            "0.2.0-beta",
            "0.2.0-beta.2",
            "0.2.0-beta.11",
            "0.2.0-rc.1",
            "0.2.0",
            "0.10.0",
            "1.0.0",
        ];
        for pair in order.windows(2) {
            assert!(v(pair[0]) < v(pair[1]), "{} < {}", pair[0], pair[1]);
        }
        for text in order {
            assert_eq!(v(text).to_string(), text);
        }
        for bad in ["", "0.1", "0.1.0.0", "v0.1.0", "01.0.0", "0.1.0-", "0.1.0-01", "0.1.0-a..b", "0.1.0+b", "0.1.x"] {
            assert_eq!(Version::parse(bad), None, "{bad}");
        }
        assert!(current().to_string() == env!("ILLUSTASH_VERSION"));
    }

    #[test]
    fn newest_skips_drafts_odd_tags_and_prereleases_for_stable_builds() {
        let releases = json!([
            {"tag_name": "v0.3.0", "draft": true, "prerelease": false},
            {"tag_name": "v0.2.1-beta.1", "draft": false, "prerelease": true},
            {"tag_name": "v0.2.2", "draft": false, "prerelease": true},
            {"tag_name": "v0.2.0", "draft": false, "prerelease": false},
            {"tag_name": "release-9", "draft": false, "prerelease": false},
            {"tag_name": "v9.0", "draft": false, "prerelease": false},
            {"name": "no tag"},
        ]);
        assert_eq!(newest(&releases, &v("0.1.0")), Some(v("0.2.0")));
        assert_eq!(newest(&releases, &v("0.2.0-beta.1")), Some(v("0.2.2")));
        assert_eq!(newest(&json!([]), &v("0.1.0")), None);
        assert_eq!(newest(&json!({"message": "Not Found"}), &v("0.1.0")), None);
        assert_eq!(page(&v("0.2.0-beta.1")), "https://github.com/shio-vinyl/illustash/releases/tag/v0.2.0-beta.1");
    }

    #[test]
    fn nothing_is_fetched_until_allowed_then_at_each_launch_and_daily_while_open() {
        let fixture = Fixture::new();
        let launch = AtomicBool::new(false);
        let calls = Cell::new(0);
        let fetch = |_: &Version| {
            calls.set(calls.get() + 1);
            Ok(Some(v("0.2.0")))
        };
        let auto = || body(json!({"action": "check", "auto": true}));
        let morning = fixture.at("2026-10-18T09:00:00+08:00", &launch);
        let info = morning.info(&v("0.1.0"));
        assert_eq!(field(&info, "auto"), Value::Null);
        assert_eq!(field(&info, "due"), json!(false));
        // Never asked, then declined: an automatic check answers without the network and uses up nothing.
        post_at(&morning, &auto(), &v("0.1.0"), fetch).unwrap();
        post_at(&morning, &body(json!({"action": "auto", "allow": false})), &v("0.1.0"), fetch).unwrap();
        post_at(&morning, &auto(), &v("0.1.0"), fetch).unwrap();
        assert_eq!(calls.get(), 0);
        assert!(!fixture.0.join("updates.json").exists());
        assert!(!launch.load(Atomic::SeqCst));
        // Allowed: this launch checks once, however many pages ask.
        let info = post_at(&morning, &body(json!({"action": "auto", "allow": true})), &v("0.1.0"), fetch).unwrap();
        assert_eq!((field(&info, "auto"), field(&info, "due")), (json!(true), json!(true)));
        let info = post_at(&morning, &auto(), &v("0.1.0"), fetch).unwrap();
        assert_eq!(calls.get(), 1);
        assert_eq!(field(&info, "latest"), json!("0.2.0"));
        assert_eq!(field(&info, "newer"), json!(true));
        assert_eq!(field(&info, "url"), json!("https://github.com/shio-vinyl/illustash/releases/tag/v0.2.0"));
        assert_eq!(field(&info, "due"), json!(false));
        let evening = fixture.at("2026-10-18T23:00:00+08:00", &launch);
        post_at(&evening, &auto(), &v("0.1.0"), fetch).unwrap();
        assert_eq!(calls.get(), 1);
        // A manual check is the person's click: always.
        post_at(&evening, &body(json!({"action": "check"})), &v("0.1.0"), fetch).unwrap();
        assert_eq!(calls.get(), 2);
        // The next launch checks again, even an hour later.
        let relaunch = AtomicBool::new(false);
        let next = fixture.at("2026-10-19T00:00:00+08:00", &relaunch);
        assert_eq!(field(&next.info(&v("0.1.0")), "due"), json!(true));
        post_at(&next, &auto(), &v("0.1.0"), fetch).unwrap();
        post_at(&next, &auto(), &v("0.1.0"), fetch).unwrap();
        assert_eq!(calls.get(), 3);
        // Left open: once a day has passed since the last check.
        let later = fixture.at("2026-10-19T23:59:59+08:00", &relaunch);
        assert_eq!(field(&later.info(&v("0.1.0")), "due"), json!(false));
        let day = fixture.at("2026-10-20T00:00:00+08:00", &relaunch);
        assert_eq!(field(&day.info(&v("0.1.0")), "due"), json!(true));
        post_at(&day, &auto(), &v("0.1.0"), fetch).unwrap();
        post_at(&day, &auto(), &v("0.1.0"), fetch).unwrap();
        assert_eq!(calls.get(), 4);
        // A clock set back counts as due rather than blocking checks for good.
        assert_eq!(field(&fixture.at("2026-10-01T00:00:00+08:00", &relaunch).info(&v("0.1.0")), "due"), json!(true));
        // Turned off again: nothing more, launch or day.
        post_at(&day, &body(json!({"action": "auto", "allow": false})), &v("0.1.0"), fetch).unwrap();
        let fresh = AtomicBool::new(false);
        post_at(&fixture.at("2026-10-25T00:00:00+08:00", &fresh), &auto(), &v("0.1.0"), fetch).unwrap();
        assert_eq!(calls.get(), 4);
        // The choice sits in preferences.json beside the language.
        let preferences: Value =
            serde_json::from_str(&fs::read_to_string(fixture.0.join("preferences.json")).unwrap()).unwrap();
        assert_eq!(preferences, json!({"update_check": "manual"}));
    }

    #[test]
    fn other_preferences_are_kept() {
        let fixture = Fixture::new();
        let launch = AtomicBool::new(false);
        fs::write(fixture.0.join("preferences.json"), r#"{"language": "en", "future": 1}"#).unwrap();
        let place = fixture.at("2026-10-18T09:00:00+08:00", &launch);
        post_at(&place, &body(json!({"action": "auto", "allow": true})), &v("0.1.0"), |_| unreachable!()).unwrap();
        let preferences: Value = serde_json::from_str(&fs::read_to_string(&place.preferences).unwrap()).unwrap();
        assert_eq!(preferences, json!({"language": "en", "future": 1, "update_check": "auto"}));
    }

    #[test]
    fn failures_keep_the_last_release_and_dismissing_lasts_until_a_newer_one() {
        let fixture = Fixture::new();
        let launch = AtomicBool::new(false);
        let place = fixture.at("2026-10-18T09:00:00+08:00", &launch);
        let check = body(json!({"action": "check"}));
        post_at(&place, &check, &v("0.1.0"), |_| Ok(Some(v("0.2.0")))).unwrap();
        let info = post_at(&place, &check, &v("0.1.0"), |_| Err("连不上 GitHub，请检查网络后再试".into())).unwrap();
        assert_eq!(field(&info, "latest"), json!("0.2.0"));
        assert_eq!(field(&info, "error"), json!("连不上 GitHub，请检查网络后再试"));
        let info = post_at(&place, &body(json!({"action": "dismiss"})), &v("0.1.0"), |_| unreachable!()).unwrap();
        assert_eq!((field(&info, "newer"), field(&info, "dismissed")), (json!(true), json!(true)));
        let info = post_at(&place, &check, &v("0.1.0"), |_| Ok(Some(v("0.2.1")))).unwrap();
        assert_eq!((field(&info, "error"), field(&info, "dismissed")), (Value::Null, json!(false)));
        // Once installed, the same release is no longer newer.
        let info = place.info(&v("0.2.1"));
        assert_eq!((field(&info, "newer"), field(&info, "url")), (json!(false), Value::Null));
        // No release at all is a successful answer, not a failure.
        let info = post_at(&place, &check, &v("0.1.0"), |_| Ok(None)).unwrap();
        assert_eq!((field(&info, "latest"), field(&info, "newer")), (Value::Null, json!(false)));
    }

    #[test]
    fn requests_are_checked_strictly() {
        let fixture = Fixture::new();
        let launch = AtomicBool::new(false);
        let place = fixture.at("2026-10-18T09:00:00+08:00", &launch);
        for bad in [
            json!({}),
            json!({"action": "install"}),
            json!({"action": "auto"}),
            json!({"action": "auto", "allow": "yes"}),
            json!({"action": "check", "auto": 1}),
            json!({"action": "check", "url": "https://example.com/"}),
            json!({"action": "dismiss", "version": "0.2.0"}),
            json!(["check"]),
        ] {
            let Ok(request) = crate::http_json::parse_request(&bad.to_string()) else { continue };
            let result = post_at(&place, &request, &v("0.1.0"), |_| unreachable!());
            assert!(matches!(result, Err(Error::Invalid(_))), "{bad}");
        }
        assert!(!place.preferences.exists() && !place.state.exists());
    }
}
