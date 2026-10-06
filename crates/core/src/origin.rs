//! 出处类型：用户指定的来源分类。溯源站点和服务账号是独立概念。
use std::sync::LazyLock;

/// 专门解析得到的原帖；不改变用户指定的 assets.preset。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostRef {
    pub site: &'static str,
    pub id: String,
    pub url: String,
    pub artist: Option<(String, String)>,
}

/// 所有图先走通用本机处理，再叠加出处的可选处理。
/// 图片外发还必须由调用方检查用户授权；许可本身不会发送图片。
pub trait Origin: Sync {
    fn id(&self) -> &'static str;
    fn label(&self) -> &'static str;
    fn photos_apps(&self) -> &'static [&'static str] {
        &[]
    }
    fn parse_filename(&self, _name: &str) -> Option<PostRef> {
        None
    }
    fn parse_url(&self, _url: &str) -> Option<PostRef> {
        None
    }
    fn allows_image_upload(&self) -> bool {
        false
    }
    /// 统计中可识别的专门原帖站点；不等同于外发许可。
    fn tracks_original_posts(&self) -> bool {
        false
    }
}

pub struct Pixiv;
pub struct X;
pub struct Other;
static PIXIV: Pixiv = Pixiv;
static X_ORIGIN: X = X;
static OTHER: Other = Other;
static ORIGINS: [&'static dyn Origin; 3] = [&PIXIV, &X_ORIGIN, &OTHER];

impl Origin for Pixiv {
    fn id(&self) -> &'static str {
        "pixiv"
    }
    fn label(&self) -> &'static str {
        "Pixiv"
    }
    fn photos_apps(&self) -> &'static [&'static str] {
        &["net.pxv.iphone"]
    }
    fn parse_filename(&self, name: &str) -> Option<PostRef> {
        static PATTERN: LazyLock<regex::Regex> =
            LazyLock::new(|| regex::Regex::new(r"^(\d+)_p\d+(?:\.[^/\\]+)?$").expect("constant regex"));
        let m = PATTERN.captures(name)?;
        Some(pixiv_post(&m[1]))
    }
    fn parse_url(&self, url: &str) -> Option<PostRef> {
        // 保留现有 Danbooru source 解析口径，包括旧 illust_id 和图片 URL。
        static PATTERN: LazyLock<regex::Regex> = LazyLock::new(|| {
            regex::Regex::new(r"pixiv\.net/(?:en/)?artworks/(\d+)|illust_id=(\d+)|/img/[\d/]+/(\d+)_p\d+")
                .expect("constant regex")
        });
        let m = PATTERN.captures(url)?;
        let id = m.iter().skip(1).flatten().next()?.as_str();
        Some(pixiv_post(id))
    }
    fn allows_image_upload(&self) -> bool {
        true
    }
    fn tracks_original_posts(&self) -> bool {
        true
    }
}
fn pixiv_post(id: &str) -> PostRef {
    PostRef { site: PIXIV.id(), id: id.into(), url: format!("https://www.pixiv.net/artworks/{id}"), artist: None }
}
impl Origin for X {
    fn id(&self) -> &'static str {
        "x"
    }
    fn label(&self) -> &'static str {
        "X"
    }
    fn photos_apps(&self) -> &'static [&'static str] {
        &["com.atebits.Tweetie2"]
    }
    fn parse_url(&self, url: &str) -> Option<PostRef> {
        static POST: LazyLock<regex::Regex> =
            LazyLock::new(|| regex::Regex::new(r"(?:twitter|x)\.com/[^/]+/status/(\d+)").expect("constant regex"));
        static ARTIST: LazyLock<regex::Regex> = LazyLock::new(|| {
            regex::Regex::new(r"^https?://(?:www\.|mobile\.)?(?:twitter|x)\.com/([A-Za-z0-9_]{1,15})/status/\d+")
                .expect("constant regex")
        });
        let m = POST.captures(url)?;
        let artist = ARTIST.captures(url).and_then(|a| {
            let key = a[1].to_lowercase();
            (!matches!(key.as_str(), "i" | "intent")).then(|| (key, format!("https://x.com/{}", &a[1])))
        });
        Some(PostRef { site: self.id(), id: m[1].into(), url: url.into(), artist })
    }
    fn allows_image_upload(&self) -> bool {
        true
    }
    fn tracks_original_posts(&self) -> bool {
        true
    }
}
impl Origin for Other {
    fn id(&self) -> &'static str {
        "other"
    }
    fn label(&self) -> &'static str {
        "其他"
    }
}

pub fn all() -> &'static [&'static dyn Origin] {
    &ORIGINS
}
/// 校验显式用户输入；与未知值的运行期回退分开。
pub fn find(id: &str) -> Option<&'static dyn Origin> {
    all().iter().copied().find(|o| o.id() == id)
}
pub fn get(id: &str) -> &'static dyn Origin {
    find(id).unwrap_or(&OTHER)
}
pub fn parse_url(url: &str) -> Option<PostRef> {
    all().iter().find_map(|o| o.parse_url(url))
}
pub fn photos_app(app: &str) -> Option<&'static dyn Origin> {
    all().iter().copied().find(|o| o.photos_apps().contains(&app))
}
pub fn photos_defaults() -> serde_json::Value {
    all()
        .iter()
        .flat_map(|o| o.photos_apps().iter().map(|app| ((*app).to_owned(), serde_json::Value::from(o.id()))))
        .collect::<serde_json::Map<_, _>>()
        .into()
}
pub fn descriptions() -> serde_json::Value {
    all().iter().map(|o| serde_json::json!({"id":o.id(),"label":o.label()})).collect::<Vec<_>>().into()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn registry_defaults_and_permissions() {
        assert_eq!(all().iter().map(|o| o.id()).collect::<Vec<_>>(), ["pixiv", "x", "other"]);
        assert!(find("unknown").is_none());
        assert_eq!(get("unknown").id(), "other");
        for id in ["other", "unknown"] {
            let o = get(id);
            assert!(!o.allows_image_upload());
            assert!(o.parse_filename("123_p0.jpg").is_none());
            assert!(o.parse_url("https://x.com/synthetic/status/123").is_none());
            assert!(o.photos_apps().is_empty());
        }
        assert!(PIXIV.allows_image_upload() && X_ORIGIN.allows_image_upload());
        assert_eq!(photos_defaults(), serde_json::json!({"net.pxv.iphone":"pixiv","com.atebits.Tweetie2":"x"}));
        assert!(photos_app("synthetic.unknown").is_none());
    }
    #[test]
    fn post_parsing_preserves_legacy_forms() {
        for url in [
            "https://www.pixiv.net/en/artworks/123",
            "https://www.pixiv.net/member_illust.php?illust_id=123",
            "https://i.pximg.net/img/2026/10/02/123_p0.jpg",
        ] {
            assert_eq!(parse_url(url).unwrap(), pixiv_post("123"));
        }
        assert_eq!(PIXIV.parse_filename("123_p2.png").unwrap(), pixiv_post("123"));
        assert!(PIXIV.parse_filename("private123.jpg").is_none());
        let post = parse_url("https://mobile.twitter.com/Synthetic/status/456?x=1").unwrap();
        assert_eq!(post.site, "x");
        assert_eq!(post.id, "456");
        assert_eq!(post.artist, Some(("synthetic".into(), "https://x.com/Synthetic".into())));
        assert!(parse_url("https://x.com/i/status/456").unwrap().artist.is_none());
        assert!(parse_url("https://example.invalid/post/123").is_none());
    }
}
