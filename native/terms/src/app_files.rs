//! Native static bytes and the existing content-addressed JPEG cache. No source scanning.
use crate::{app_library::Library, Error};
use image::{metadata::Orientation, DynamicImage, ImageReader};
use rusqlite::OptionalExtension;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs,
    io::{self, BufRead, BufReader, Read, Seek},
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex, RwLock, Weak},
    time::UNIX_EPOCH,
};
use tiny_http::{Header, Response, StatusCode};

pub const ROUTES: &[&str] =
    &["/", "/app.js", "/app.css", "/theme.js", "/favicon.svg", "/splash.js", "/i18n.js", "/image"];
/// Folders of the page's ES modules under web/static (#199): core/ never touches the DOM and can serve any layout,
/// ui/ is the desktop layout. A module is /<folder>/<name>.js, its name in lowercase letters, digits, - and _.
const MODULE_FOLDERS: &[&str] = &["/core/", "/ui/"];
/// The module's folder under web/static and its file name.
fn module(path: &str) -> Option<(&'static str, &str)> {
    let folder = MODULE_FOLDERS.iter().find(|folder| path.starts_with(**folder))?;
    let file = &path[folder.len()..];
    let name = file.strip_suffix(".js")?;
    let plain = !name.is_empty()
        && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_');
    plain.then_some((folder.trim_matches('/'), file))
}
/// Whether `path` (without its query) is a static file, a module or a picture.
pub fn serves(path: &str) -> bool {
    ROUTES.contains(&path) || module(path).is_some()
}

pub enum Failure {
    Status(u16),
    Core(Error),
}
impl From<Error> for Failure {
    fn from(e: Error) -> Self {
        Self::Core(e)
    }
}
pub enum Prepared {
    Static(PathBuf, &'static str),
    /// web/static/i18n.js after the language setting and the tables of web/static/locales (#173, #182).
    I18n {
        script: PathBuf,
        locales: PathBuf,
    },
    Image {
        source: PathBuf,
        target: PathBuf,
        bound: u32,
    },
}
pub struct File {
    bytes: Vec<u8>,
    kind: &'static str,
    cache: bool,
}
impl File {
    /// A JPEG kept among the thumbnails, such as a Danbooru character's preview (#293); the page may cache it.
    pub fn jpeg(bytes: Vec<u8>) -> Self {
        Self { bytes, kind: "image/jpeg", cache: true }
    }
}

pub fn prepare(library: &Library, root: &Path, url: &str) -> Result<Prepared, Failure> {
    let path = url.split('?').next().unwrap_or(url);
    if path == "/i18n.js" {
        let script = root.join("web/static/i18n.js");
        return Ok(Prepared::I18n { script, locales: crate::app_preferences::locales_folder(root) });
    }
    if let Some((folder, file)) = module(path) {
        let script = root.join("web/static").join(folder).join(file);
        return if script.is_file() {
            Ok(Prepared::Static(script, "text/javascript"))
        } else {
            Err(Failure::Status(404))
        };
    }
    if path != "/image" {
        let (name, kind) = match path {
            "/" => ("index.html", "text/html"),
            "/app.css" => ("app.css", "text/css"),
            "/favicon.svg" => ("favicon.svg", "image/svg+xml"),
            "/app.js" => ("app.js", "text/javascript"),
            "/theme.js" => ("theme.js", "text/javascript"),
            "/splash.js" => ("splash.js", "text/javascript"),
            _ => return Err(Failure::Status(404)),
        };
        return Ok(Prepared::Static(root.join("web/static").join(name), kind));
    }
    let query = crate::http_read::query(url);
    let size = query.get("size").map(String::as_str).unwrap_or("thumb");
    let bound = match size {
        "thumb" => 400,
        "medium" => 800,
        "preview" => 1600,
        "full" => 3000,
        _ => return Err(Failure::Status(400)),
    };
    let ident = query.get("id").map(String::as_str).unwrap_or("");
    let connection = crate::app_library::read_connection(&library.db())?;
    let source: Option<String> = connection
        .query_row("SELECT src_path FROM assets WHERE uuid=?", [ident], |r| r.get(0))
        .optional()
        .map_err(Error::from)?
        .flatten();
    let source = source.filter(|p| !p.is_empty()).ok_or(Failure::Status(404))?;
    let source = PathBuf::from(source);
    let stat = fs::metadata(&source).map_err(|_| Failure::Status(404))?;
    // SystemTime permits pre-epoch timestamps too; Python st_mtime_ns is signed.
    let modified = stat.modified().map_err(|_| Failure::Status(404))?;
    let nanos = match modified.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_nanos() as i128,
        Err(e) => -(e.duration().as_nanos() as i128),
    };
    let key = format!("{:x}", Sha256::digest(format!("{ident}:{nanos}:{}:{size}", stat.len()).as_bytes()));
    Ok(Prepared::Image { source, target: library.thumbnails().join(format!("{key}.jpg")), bound })
}

#[derive(Default)]
pub struct Images {
    keys: Mutex<HashMap<PathBuf, Weak<Mutex<()>>>>,
    caches: Mutex<HashMap<PathBuf, Weak<RwLock<()>>>>,
    active: Mutex<usize>,
    available: Condvar,
    #[cfg(test)]
    generated: std::sync::atomic::AtomicUsize,
}
struct Slot<'a>(&'a Images);
impl Drop for Slot<'_> {
    fn drop(&mut self) {
        *self.0.active.lock().expect("app_files mutex poisoned") -= 1;
        self.0.available.notify_one();
    }
}
impl Images {
    fn cache_lock(&self, cache: &Path) -> io::Result<Arc<RwLock<()>>> {
        // Aliases of the same package share a gate, even before thumbnails/ exists.
        let key = crate::app_paths::canonical(cache).or_else(|_| {
            Ok::<_, io::Error>(
                crate::app_paths::canonical(cache.parent().expect("internal file path must have a parent"))?
                    .join(cache.file_name().expect("app_files internal invariant")),
            )
        })?;
        let mut caches = self.caches.lock().expect("app_files mutex poisoned");
        caches.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = caches.get(&key).and_then(Weak::upgrade) {
            return Ok(lock);
        }
        let lock = Arc::new(RwLock::new(()));
        caches.insert(key, Arc::downgrade(&lock));
        Ok(lock)
    }
    pub(crate) fn clear(&self, library: &Library, forced_off: &[String]) -> crate::Result<crate::http_json::Json> {
        let cache = library.thumbnails();
        let lock = self.cache_lock(&cache)?;
        // Readers include cached-byte reads and the entire atomic generation. Socket IO
        // holds neither this gate nor a decode slot. Other packages keep independent gates.
        let _guard = lock.write().expect("app_files session lock poisoned");
        let _slot = self.slot();
        let removed = Self::remove_jpegs(&cache)?;
        let crate::http_json::Json::Object(mut fields) = crate::app_settings::info(library, forced_off)? else {
            unreachable!()
        };
        fields.insert(0, ("removed".into(), serde_json::Value::from(removed).into()));
        Ok(crate::http_json::Json::Object(fields))
    }
    fn remove_jpegs(cache: &Path) -> io::Result<u64> {
        let entries = match fs::read_dir(cache) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(e),
        };
        let mut removed = 0;
        for entry in entries {
            let entry = entry?;
            // Python glob('*.jpg'): direct children and hidden names; Windows
            // uses its default case-insensitive matching, POSIX is case-sensitive.
            let name = entry.file_name();
            let name = name.as_encoded_bytes();
            let jpeg = name
                .get(name.len().saturating_sub(4)..)
                .is_some_and(|suffix| crate::platform_filesystem::extension_matches(suffix, b".jpg"));
            if jpeg {
                match fs::remove_file(entry.path()) {
                    Ok(()) => {}
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e),
                }
                removed += 1;
            }
        }
        Ok(removed)
    }
    fn key_lock(&self, key: &Path) -> Arc<Mutex<()>> {
        let mut keys = self.keys.lock().expect("app_files mutex poisoned");
        // No permanent per-asset locks; retain only in-flight owners and waiters.
        keys.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = keys.get(key).and_then(Weak::upgrade) {
            return lock;
        }
        let lock = Arc::new(Mutex::new(()));
        keys.insert(key.to_owned(), Arc::downgrade(&lock));
        lock
    }
    fn slot(&self) -> Slot<'_> {
        let mut active = self.active.lock().expect("app_files mutex poisoned");
        while *active >= 4 {
            active = self.available.wait(active).expect("app_files internal invariant");
        }
        *active += 1;
        Slot(self)
    }
    pub fn load(&self, file: Prepared) -> Result<File, Failure> {
        match file {
            Prepared::Static(path, kind) => {
                Ok(File { bytes: fs::read(path).map_err(Error::from)?, kind, cache: false })
            }
            Prepared::I18n { script, locales } => {
                let locales = crate::app_preferences::locales(&locales);
                let language = crate::app_preferences::language(&locales);
                let bytes = i18n_script(&language, &fs::read(script).map_err(Error::from)?, &locales);
                Ok(File { bytes, kind: "text/javascript", cache: false })
            }
            Prepared::Image { source, target, bound } => {
                let bytes = self.cached(&source, &target, bound).map_err(|e| {
                    eprintln!("HTTP image: {e}");
                    Failure::Status(404)
                })?;
                Ok(File { bytes, kind: "image/jpeg", cache: true })
            }
        }
    }
    fn cached(&self, source: &Path, target: &Path, bound: u32) -> io::Result<Vec<u8>> {
        let cache = self.cache_lock(target.parent().expect("internal file path must have a parent"))?;
        let _cache_guard = cache.read().expect("app_files session lock poisoned");
        match fs::read(target) {
            Ok(bytes) => return Ok(bytes),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        let lock = self.key_lock(target);
        let _guard = lock.lock().expect("app_files mutex poisoned");
        match fs::read(target) {
            Ok(bytes) => return Ok(bytes),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        let _slot = self.slot();
        #[cfg(test)]
        self.generated.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let bytes = generate(source, bound)?;
        fs::create_dir_all(target.parent().expect("internal file path must have a parent"))?;
        let mut nonce = [0u8; 12];
        getrandom::fill(&mut nonce).map_err(|e| io::Error::other(e.to_string()))?;
        let nonce: String = nonce.iter().map(|b| format!("{b:02x}")).collect();
        let temp = target.with_extension(format!("{nonce}.tmp"));
        let result = (|| {
            use std::io::Write;
            let mut file = fs::OpenOptions::new().write(true).create_new(true).open(&temp)?;
            file.write_all(&bytes)?;
            drop(file);
            fs::rename(&temp, target)
        })();
        let _ = fs::remove_file(temp);
        result?;
        Ok(bytes)
    }
}
/// The page reads the setting synchronously before app.js runs, so the first paint is already in its language.
/// Every language on offer is listed by name for the settings. Tables go only for the language set: `system`
/// is resolved from the browser's languages, which only the page knows, so then every table goes and i18n.js
/// picks one; fetching it afterwards would make app.js wait and flash Chinese first (#182). Tables are parsed
/// and written back as JSON: a broken or missing file is left out and its text stays in Chinese.
fn i18n_script(language: &str, script: &[u8], locales: &[crate::app_preferences::Locale]) -> Vec<u8> {
    use serde_json::{Map, Value};
    let names: Map<String, Value> = locales.iter().map(|l| (l.code.clone(), l.name.as_str().into())).collect();
    let tables: Map<String, Value> = locales
        .iter()
        .filter(|l| language == "system" || l.code == language)
        .map(|l| (l.code.clone(), Value::Object(l.table.clone())))
        .collect();
    let mut out = format!(
        "self.ILLUSTASH_LANGUAGE = {};\nself.ILLUSTASH_LANGUAGES = {};\nself.ILLUSTASH_TABLES = {};\n",
        Value::from(language),
        Value::Object(names),
        Value::Object(tables)
    )
    .into_bytes();
    out.extend_from_slice(script);
    out
}
fn generate(source: &Path, bound: u32) -> io::Result<Vec<u8>> {
    let mut reader = BufReader::new(fs::File::open(source)?);
    // PIL also tolerates absent/malformed EXIF. Container parsing covers PNG, which
    // image 0.25.6's decoder does not expose, as well as JPEG/TIFF/WebP.
    let orientation = exif::Reader::new()
        .read_from_container(&mut reader)
        .ok()
        .and_then(|exif| exif.get_field(exif::Tag::Orientation, exif::In::PRIMARY).and_then(|f| f.value.get_uint(0)))
        .and_then(|n| u8::try_from(n).ok())
        .and_then(Orientation::from_exif)
        .unwrap_or(Orientation::NoTransforms);
    reader.rewind()?;
    let header = reader.fill_buf()?;
    // PIL resizes palette and bilevel PNG/BMP in nearest-neighbour mode.
    let png = header.starts_with(b"\x89PNG\r\n\x1a\n");
    let nearest = header.starts_with(b"GIF")
        || (png && (header.get(25) == Some(&3) || (header.get(24) == Some(&1) && header.get(25) == Some(&0))))
        || (header.starts_with(b"BM") && header.get(28) == Some(&1) && header.get(29) == Some(&0));
    let mut reader = ImageReader::new(reader).with_guessed_format()?;
    reader.no_limits();
    let mut im = if reader.format() == Some(image::ImageFormat::Jpeg) {
        // mozjpeg's error manager unwinds on corrupt JPEGs; contain it before the
        // HTTP worker/key lock, so failures return 404 and never poison shared state.
        std::panic::catch_unwind(|| -> io::Result<DynamicImage> {
            let mut decoder = mozjpeg::Decompress::new_reader(reader.into_inner())?;
            pixel_limit(decoder.width() as u32, decoder.height() as u32)?;
            let ratio = decoder.width().min(decoder.height()) / bound as usize;
            decoder.scale(if ratio >= 8 {
                1
            } else if ratio >= 4 {
                2
            } else if ratio >= 2 {
                4
            } else {
                8
            });
            let cmyk = matches!(decoder.color_space(), mozjpeg::ColorSpace::JCS_CMYK | mozjpeg::ColorSpace::JCS_YCCK);
            let mut decoder =
                if cmyk { decoder.to_colorspace(mozjpeg::ColorSpace::JCS_CMYK)? } else { decoder.rgb()? };
            let (width, height) = (decoder.width() as u32, decoder.height() as u32);
            let pixels = decoder.read_scanlines::<u8>()?;
            let pixels = if cmyk {
                // Pillow's JPEG plugin assumes Adobe (inverted CMYK) polarity.
                pixels
                    .chunks_exact(4)
                    .flat_map(|p| [0, 1, 2].map(|i| ((u32::from(p[i]) * u32::from(p[3]) + 127) / 255) as u8))
                    .collect()
            } else {
                pixels
            };
            decoder.finish()?;
            let rgb = image::RgbImage::from_raw(width, height, pixels)
                .ok_or_else(|| io::Error::other("invalid JPEG buffer"))?;
            Ok(DynamicImage::ImageRgb8(rgb))
        })
        .map_err(|_| io::Error::other("JPEG decoder failed"))??
    } else {
        let decoder = reader.into_decoder().map_err(io::Error::other)?;
        let (width, height) = image::ImageDecoder::dimensions(&decoder);
        pixel_limit(width, height)?;
        DynamicImage::from_decoder(decoder).map_err(io::Error::other)?
    };
    im.apply_orientation(orientation);
    if im.width() > bound || im.height() > bound {
        im = resize(im, bound, nearest)?;
    }
    let rgb = if im.color().has_alpha() {
        let rgba = im.into_rgba8();
        image::RgbImage::from_fn(rgba.width(), rgba.height(), |x, y| {
            let pixel = rgba.get_pixel(x, y);
            let alpha = u32::from(pixel[3]);
            image::Rgb(
                [0, 1, 2]
                    .map(|i| ((u32::from(pixel[i]) * alpha + [244u32, 244, 242][i] * (255 - alpha) + 127) / 255) as u8),
            )
        })
    } else {
        im.into_rgb8()
    };
    std::panic::catch_unwind(|| -> io::Result<Vec<u8>> {
        let mut encoder = mozjpeg::Compress::new(mozjpeg::ColorSpace::JCS_RGB);
        encoder.set_fastest_defaults();
        encoder.set_size(rgb.width() as usize, rgb.height() as usize);
        encoder.set_quality(84.0);
        let mut encoder = encoder.start_compress(Vec::new())?;
        encoder.write_scanlines(rgb.as_raw())?;
        encoder.finish()
    })
    .map_err(|_| io::Error::other("JPEG encoder failed"))?
}

fn resize(im: DynamicImage, bound: u32, nearest: bool) -> io::Result<DynamicImage> {
    use fast_image_resize::{images::Image, FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer};
    let (width, height) = (im.width(), im.height());
    let aspect = f64::from(width) / f64::from(height);
    // PIL picks floor/ceil by aspect error (ties choose floor), never upscales.
    let (out_width, out_height) = if width <= height {
        let value = f64::from(bound) * aspect;
        let rounded =
            if (aspect - value.floor() / f64::from(bound)).abs() <= (aspect - value.ceil() / f64::from(bound)).abs() {
                value.floor()
            } else {
                value.ceil()
            };
        (rounded.max(1.) as u32, bound)
    } else {
        let value = f64::from(bound) / aspect;
        let error = |n: f64| if n == 0. { 0. } else { (aspect - f64::from(bound) / n).abs() };
        let rounded = if error(value.floor()) <= error(value.ceil()) { value.floor() } else { value.ceil() };
        (bound, rounded.max(1.) as u32)
    };
    let alpha = im.color().has_alpha();
    let (pixels, kind) = if alpha {
        (im.into_rgba8().into_raw(), PixelType::U8x4)
    } else {
        (im.into_rgb8().into_raw(), PixelType::U8x3)
    };
    let source = Image::from_vec_u8(width, height, pixels, kind).map_err(io::Error::other)?;
    let mut target = Image::new(out_width, out_height, kind);
    let algorithm = if nearest { ResizeAlg::Nearest } else { ResizeAlg::Convolution(FilterType::CatmullRom) };
    Resizer::new()
        .resize(&source, &mut target, &ResizeOptions::new().resize_alg(algorithm))
        .map_err(io::Error::other)?;
    if alpha {
        Ok(DynamicImage::ImageRgba8(
            image::RgbaImage::from_raw(out_width, out_height, target.into_vec())
                .expect("internal JSON value is serializable"),
        ))
    } else {
        Ok(DynamicImage::ImageRgb8(
            image::RgbImage::from_raw(out_width, out_height, target.into_vec())
                .expect("internal JSON value is serializable"),
        ))
    }
}

fn pixel_limit(width: u32, height: u32) -> io::Result<()> {
    // PIL raises a decompression-bomb error above twice MAX_IMAGE_PIXELS.
    if u64::from(width) * u64::from(height) > 178_956_970 {
        Err(io::Error::other("image exceeds pixel limit"))
    } else {
        Ok(())
    }
}

pub fn response(file: File) -> Response<Box<dyn Read + Send>> {
    let length = file.bytes.len();
    let headers = [
        ("Content-Type", file.kind),
        ("Cache-Control", if file.cache { "private, max-age=86400" } else { "no-store" }),
        ("X-Content-Type-Options", "nosniff"),
        ("Referrer-Policy", "no-referrer"),
        ("Content-Security-Policy", "default-src 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline'; script-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'"),
        ("X-Illustash-Route", "core"),
    ].into_iter().map(|(k,v)| Header::from_bytes(k,v).expect("app_files internal invariant")).collect();
    Response::new(
        StatusCode(200),
        headers,
        Box::new(io::Cursor::new(file.bytes)) as Box<dyn Read + Send>,
        Some(length),
        None,
    )
    .with_chunked_threshold(usize::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;
    #[test]
    fn i18n_script_prefixes_setting_names_and_tables() {
        let mut nonce = [0u8; 12];
        getrandom::fill(&mut nonce).unwrap();
        let root = std::env::temp_dir().join(format!("illustash-i18n-{:x}", Sha256::digest(nonce)));
        let folder = crate::app_preferences::locales_folder(&root);
        fs::create_dir_all(&folder).unwrap();
        fs::write(
            folder.join("en.json"),
            "{\"@name\": \"English\", \"设置\": \"Settings\", \"共 {n} 张\": \"{n} pictures\"}",
        )
        .unwrap();
        fs::write(folder.join("ja.json"), "{\"@name\": \"日本語\", \"设置\": \"設定\"}").unwrap();
        fs::write(folder.join("fr.json"), "{broken").unwrap();
        let locales = crate::app_preferences::locales(&folder);
        let script = |language| String::from_utf8(i18n_script(language, b"/* script */", &locales)).unwrap();
        const NAMES: &str = "self.ILLUSTASH_LANGUAGES = {\"en\":\"English\",\"ja\":\"日本語\"};\n";
        // A language set: only its table. The broken fr.json is neither offered nor sent.
        assert_eq!(
            script("en"),
            format!("self.ILLUSTASH_LANGUAGE = \"en\";\n{NAMES}self.ILLUSTASH_TABLES = {{\"en\":{{\"@name\":\"English\",\"共 {{n}} 张\":\"{{n}} pictures\",\"设置\":\"Settings\"}}}};\n/* script */")
        );
        assert!(script("ja").ends_with(&format!(
            "{NAMES}self.ILLUSTASH_TABLES = {{\"ja\":{{\"@name\":\"日本語\",\"设置\":\"設定\"}}}};\n/* script */"
        )));
        // Chinese needs no table; following the system sends every one, for the page to choose.
        assert!(script("zh-CN").ends_with("self.ILLUSTASH_TABLES = {};\n/* script */"));
        let system = script("system");
        let tables = system.lines().nth(2).unwrap().strip_prefix("self.ILLUSTASH_TABLES = ").unwrap();
        let tables: serde_json::Value = serde_json::from_str(tables.strip_suffix(';').unwrap()).unwrap();
        assert_eq!(tables.as_object().unwrap().keys().collect::<Vec<_>>(), ["en", "ja"]);
        // No tables at all leave the page in Chinese instead of failing.
        assert_eq!(
            String::from_utf8(i18n_script("system", b"", &[])).unwrap(),
            "self.ILLUSTASH_LANGUAGE = \"system\";\nself.ILLUSTASH_LANGUAGES = {};\nself.ILLUSTASH_TABLES = {};\n"
        );
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn modules_are_plain_names_in_their_folders() {
        for path in ["/core/session.js", "/ui/dom.js", "/core/a_b-2.js", "/app.js", "/image"] {
            assert!(serves(path), "{path}");
        }
        for path in [
            "/core/../app.js",
            "/core/a/b.js",
            "/core/Session.js",
            "/core/.js",
            "/core/a.mjs",
            "/other/a.js",
            "/core",
            "/core/",
            "/js/a.js",
        ] {
            assert!(!serves(path), "{path}");
        }
        // Every module the page has is served.
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/static");
        for folder in MODULE_FOLDERS.iter().map(|folder| folder.trim_matches('/')) {
            for entry in fs::read_dir(root.join(folder)).unwrap() {
                let name = entry.unwrap().file_name().into_string().unwrap();
                assert!(serves(&format!("/{folder}/{name}")), "{folder}/{name}");
            }
        }
    }
    #[test]
    fn clear_waits_for_generation_and_aliases_share_gate() {
        let mut nonce = [0u8; 12];
        getrandom::fill(&mut nonce).unwrap();
        let root = std::env::temp_dir().join(format!("illustash-clear-{:x}", Sha256::digest(nonce)));
        fs::create_dir(&root).unwrap();
        fs::create_dir(root.join("cache")).unwrap();
        let source = root.join("source.png");
        let target = root.join("cache/target.jpg");
        image::RgbImage::from_pixel(12, 8, image::Rgb([40, 120, 80])).save(&source).unwrap();
        let images = Images::default();
        let gate = images.cache_lock(&root.join("cache")).unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(root.join("cache"), root.join("alias")).unwrap();
            assert!(Arc::ptr_eq(&gate, &images.cache_lock(&root.join("alias")).unwrap()));
        }
        let key = images.key_lock(&target);
        let key_guard = key.lock().unwrap();
        std::thread::scope(|scope| {
            let generator = scope.spawn(|| images.cached(&source, &target, 400).unwrap());
            // The blocked key proves cached() entered its directory read gate.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while gate.try_write().is_ok() {
                assert!(std::time::Instant::now() < deadline);
                std::thread::yield_now();
            }
            let (send, receive) = std::sync::mpsc::channel();
            let (gate, images, root) = (&gate, &images, &root);
            let cleaner = scope.spawn(move || {
                let _guard = gate.write().unwrap();
                let _slot = images.slot();
                send.send(Images::remove_jpegs(&root.join("cache")).unwrap()).unwrap();
            });
            assert!(receive.recv_timeout(std::time::Duration::from_millis(30)).is_err());
            // An unrelated cache remains available during the wait.
            fs::create_dir(root.join("other")).unwrap();
            assert!(images.cache_lock(&root.join("other")).unwrap().try_write().is_ok());
            drop(key_guard);
            assert!(!generator.join().unwrap().is_empty());
            assert_eq!(receive.recv_timeout(std::time::Duration::from_secs(5)).unwrap(), 1);
            cleaner.join().unwrap();
        });
        assert!(!target.exists());
        assert_eq!(fs::read_dir(root.join("cache")).unwrap().count(), 0);
        assert!(!images.cached(&source, &target, 400).unwrap().is_empty());
        assert_eq!(images.generated.load(Ordering::SeqCst), 2);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn concurrent_cache_miss_generates_once_and_drops_key_owners() {
        let mut nonce = [0u8; 12];
        getrandom::fill(&mut nonce).unwrap();
        let root = std::env::temp_dir().join(format!("illustash-files-{:x}", Sha256::digest(nonce)));
        fs::create_dir(&root).unwrap();
        let source = root.join("source.png");
        let target = root.join("cache/target.jpg");
        image::RgbImage::from_pixel(1201, 803, image::Rgb([40, 120, 80])).save(&source).unwrap();
        let images = Images::default();
        let barrier = std::sync::Barrier::new(12);
        std::thread::scope(|scope| {
            let workers: Vec<_> = (0..12)
                .map(|_| {
                    scope.spawn(|| {
                        barrier.wait();
                        images.cached(&source, &target, 400).unwrap()
                    })
                })
                .collect();
            let bytes = workers.into_iter().map(|w| w.join().unwrap()).collect::<Vec<_>>();
            assert!(bytes.iter().all(|b| b == &bytes[0]));
        });
        assert_eq!(images.generated.load(Ordering::SeqCst), 1);
        assert!(images.keys.lock().unwrap().values().all(|w| w.strong_count() == 0));
        assert_eq!(fs::read_dir(root.join("cache")).unwrap().count(), 1);
        fs::remove_dir_all(root).unwrap();
    }
}
