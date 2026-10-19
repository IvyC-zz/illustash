use super::*;
use std::{
    collections::HashMap,
    io::{BufRead, BufReader},
    net::TcpListener,
    sync::{atomic::AtomicUsize, Arc},
};

/// How the synthetic server answers one path.
#[derive(Clone)]
enum Mode {
    Normal,
    /// Ignores Range: always the whole file with 200.
    NoRange,
    Missing,
    /// Same size, different bytes.
    Corrupt,
    /// The first answer declares the full length but stops after this many bytes.
    DropOnce(usize),
    /// Sends slowly, for cancelling.
    Slow,
    /// 302 to another path (as hf-mirror sends to a CDN); the Range header must follow.
    Redirect(String),
}
struct Route {
    body: Vec<u8>,
    mode: Mode,
    served: AtomicUsize,
}
/// (path, Range header) of each request the server saw.
type Hits = Arc<Mutex<Vec<(String, Option<String>)>>>;
struct Server {
    base: String,
    /// (path, Range header) of every request.
    hits: Hits,
}
fn serve(routes: Vec<(&str, Vec<u8>, Mode)>) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let routes: Arc<HashMap<String, Route>> = Arc::new(
        routes
            .into_iter()
            .map(|(path, body, mode)| (path.to_owned(), Route { body, mode, served: AtomicUsize::new(0) }))
            .collect(),
    );
    let hits = Arc::new(Mutex::new(Vec::new()));
    let seen = hits.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let (routes, seen) = (routes.clone(), seen.clone());
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut first = String::new();
                reader.read_line(&mut first).unwrap();
                let path = first.split_whitespace().nth(1).unwrap_or("").to_owned();
                let mut range = None;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                    if let Some((k, v)) = line.split_once(':') {
                        if k.eq_ignore_ascii_case("range") {
                            range = Some(v.trim().to_owned());
                        }
                    }
                }
                seen.lock().unwrap().push((path.clone(), range.clone()));
                let Some(route) = routes.get(&path) else {
                    let _ =
                        stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                    return;
                };
                let first_time = route.served.fetch_add(1, Ordering::SeqCst) == 0;
                let mut body = route.body.clone();
                match &route.mode {
                    Mode::Missing => {
                        let _ = stream
                            .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                        return;
                    }
                    Mode::Redirect(to) => {
                        let _ = write!(
                            stream,
                            "HTTP/1.1 302 Found\r\nLocation: {to}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        );
                        return;
                    }
                    Mode::Corrupt => body.iter_mut().for_each(|b| *b ^= 0x5a),
                    _ => {}
                }
                let start = match (&route.mode, range.as_deref().and_then(|r| r.strip_prefix("bytes="))) {
                    (Mode::NoRange, _) | (_, None) => None,
                    (_, Some(r)) => r.trim_end_matches('-').parse::<usize>().ok(),
                };
                let head = match start {
                    Some(start) => format!(
                        "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {start}-{}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len() - 1,
                        body.len(),
                        body.len() - start
                    ),
                    None => format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()),
                };
                let body = &body[start.unwrap_or(0)..];
                let _ = stream.write_all(head.as_bytes());
                match route.mode {
                    Mode::DropOnce(n) if first_time => {
                        let _ = stream.write_all(&body[..n.min(body.len())]);
                    }
                    Mode::Slow => {
                        for chunk in body.chunks(4096) {
                            if stream.write_all(chunk).is_err() {
                                return;
                            }
                            std::thread::sleep(Duration::from_millis(30));
                        }
                    }
                    _ => {
                        let _ = stream.write_all(body);
                    }
                }
            });
        }
    });
    Server { base, hits }
}

fn bytes(size: usize, seed: u8) -> Vec<u8> {
    (0..size).map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed)).collect()
}
fn sha(data: &[u8]) -> String {
    format!("{:x}", Sha256::digest(data))
}
/// Three synthetic files of different sizes: a large one in the root, one in a subfolder, a small JSON.
struct Fixture {
    dir: PathBuf,
    files: Vec<(String, Vec<u8>)>,
    manifest: &'static Manifest,
}
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "illustash-models-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let files = vec![
            ("big.onnx".to_owned(), bytes(300_000, 7)),
            ("sub/feat.onnx".to_owned(), bytes(20_000, 11)),
            ("sub/metrics.json".to_owned(), b"{\"threshold\": 0.5}\n".to_vec()),
        ];
        let entries = files
            .iter()
            .map(|(path, data)| {
                let origin = |repo: &str, revision: &str| Origin {
                    repo: repo.into(),
                    revision: revision.into(),
                    file: format!("upstream/{path}"),
                };
                Entry {
                    path: path.clone(),
                    size: data.len() as u64,
                    sha256: sha(data),
                    hf: origin("org/repo", "abc123"),
                    modelscope: origin("org/repo", "master"),
                }
            })
            .collect();
        let manifest = Box::leak(Box::new(Manifest {
            format: 1,
            pack: "synthetic.1".into(),
            manual: "https://example.invalid/models".into(),
            mirror: "https://mirror.invalid".into(),
            modelscope: "https://modelscope.invalid".into(),
            files: entries,
        }));
        Self { dir, files, manifest }
    }
    fn place(&self, server: &Server) -> Place {
        let preferences = self.dir.join("support/preferences.json");
        crate::app_updates::update(&preferences, |p| {
            p.insert("model_mirror".into(), format!("{}/hf", server.base).into());
        })
        .unwrap();
        Place {
            root: self.dir.join("shared/models/synthetic.1"),
            manifest: self.manifest,
            preferences,
            modelscope: format!("{}/ms", server.base),
            proxy: None,
        }
    }
    fn hf(&self, path: &str) -> String {
        format!("/hf/org/repo/resolve/abc123/upstream/{path}")
    }
    fn ms(&self, path: &str) -> String {
        format!("/ms/models/org/repo/resolve/master/upstream/{path}")
    }
    /// Every file served by one source in one mode, the others by `rest`.
    fn routes(&self, special: &[(&str, Mode)], rest: Mode, hf: bool) -> Vec<(String, Vec<u8>, Mode)> {
        self.files
            .iter()
            .map(|(path, data)| {
                let mode = special.iter().find(|(p, _)| p == path).map(|(_, m)| m.clone()).unwrap_or(rest.clone());
                (if hf { self.hf(path) } else { self.ms(path) }, data.clone(), mode)
            })
            .collect()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}
fn server(routes: Vec<(String, Vec<u8>, Mode)>) -> Server {
    serve(routes.iter().map(|(p, b, m)| (p.as_str(), b.clone(), m.clone())).collect())
}
fn models() -> &'static Models {
    Box::leak(Box::new(Models::new()))
}
fn post(models: &'static Models, place: &Place, body: Value) -> Result<Value> {
    post_at(models, place.clone(), &Json::from(body)).map(|j| serde_json::from_str(&j.dump()).unwrap())
}
fn finished(models: &Models) -> Value {
    let started = Instant::now();
    while models.running.load(Ordering::SeqCst) {
        assert!(started.elapsed() < Duration::from_secs(30), "job did not finish: {}", models.snapshot());
        std::thread::sleep(Duration::from_millis(20));
    }
    models.snapshot()
}
fn installed(fixture: &Fixture, place: &Place) {
    assert!(ready_at(&place.root, fixture.manifest));
    for (path, data) in &fixture.files {
        assert_eq!(&fs::read(place.root.join(path)).unwrap(), data, "{path}");
    }
    let record: Value = serde_json::from_slice(&fs::read(place.root.join("pack.json")).unwrap()).unwrap();
    assert_eq!(record["pack"], "synthetic.1");
    assert!(!partial_dir(&place.root).exists());
}

#[test]
fn a_failed_source_hands_over_and_a_dropped_connection_resumes() {
    let fixture = Fixture::new();
    // The mirror lacks the big file and redirects the others to a CDN path; ModelScope drops the big file half way.
    let mut routes = fixture.routes(&[("big.onnx", Mode::Missing)], Mode::Normal, true);
    let mut cdn = Vec::new();
    for route in routes.iter_mut().skip(1) {
        let path = format!("/cdn/{}", route.0.rsplit('/').next().unwrap());
        cdn.push((path.clone(), route.1.clone(), Mode::Normal));
        route.2 = Mode::Redirect(path);
    }
    routes.extend(cdn);
    routes.extend(fixture.routes(&[("big.onnx", Mode::DropOnce(120_000))], Mode::Normal, false));
    let server = server(routes);
    let place = fixture.place(&server);
    let models = models();
    let out = post(models, &place, json!({"action": "download"})).unwrap();
    assert_eq!(out["status"], "missing");
    assert_eq!(out["total"], 320_000 + 19);
    let job = finished(models);
    assert_eq!(job["status"], "done", "{job}");
    assert_eq!(job["done"], job["total"]);
    installed(&fixture, &place);
    let hits = server.hits.lock().unwrap().clone();
    // The big file: mirror 404, ModelScope from 0, then resumed exactly where the dropped connection stopped.
    let big: Vec<_> = hits.iter().filter(|(p, _)| p.ends_with("big.onnx")).cloned().collect();
    assert_eq!(big[0].0, fixture.hf("big.onnx"));
    assert_eq!(big[1], (fixture.ms("big.onnx"), None));
    assert_eq!(big[2], (fixture.ms("big.onnx"), Some("bytes=120000-".into())));
    assert_eq!(big.len(), 3);
    // Redirected files came from the mirror's CDN; nothing else asked ModelScope.
    assert!(hits.iter().any(|(p, _)| p == "/cdn/feat.onnx"));
    assert!(!hits.iter().any(|(p, _)| p.starts_with("/ms/") && !p.ends_with("big.onnx")));
    let info = post(models, &place, json!({"action": "download"})).unwrap_err();
    assert!(info.to_string().contains("已经就位"));
}

#[test]
fn bytes_that_do_not_match_the_manifest_are_refused_from_every_source() {
    let fixture = Fixture::new();
    let mut routes = fixture.routes(&[("sub/feat.onnx", Mode::Corrupt)], Mode::Normal, true);
    routes.extend(fixture.routes(&[], Mode::Normal, false));
    let server = server(routes);
    let place = fixture.place(&server);
    let models = models();
    post(models, &place, json!({"action": "download"})).unwrap();
    assert_eq!(finished(models)["status"], "done");
    installed(&fixture, &place);

    // Every source corrupt: the job fails naming the file and the hash; the root is never created.
    let fixture = Fixture::new();
    let mut routes = fixture.routes(&[("sub/feat.onnx", Mode::Corrupt)], Mode::Normal, true);
    routes.extend(fixture.routes(&[("sub/feat.onnx", Mode::Corrupt)], Mode::Normal, false));
    let server = self::server(routes);
    let place = fixture.place(&server);
    let models = super::tests::models();
    post(models, &place, json!({"action": "download"})).unwrap();
    let job = finished(models);
    assert_eq!(job["status"], "failed");
    let message = job["message"].as_str().unwrap();
    assert!(message.contains("sub/feat.onnx") && message.contains("SHA-256"), "{message}");
    assert!(message.contains("127.0.0.1"), "{message}");
    assert_eq!(job["failed_file"], "sub/feat.onnx");
    let source = host(&server.base);
    assert_eq!(
        job["failures"],
        json!([{"source": source, "why": "SHA-256 与清单不符，已丢弃"}, {"source": source, "why": "SHA-256 与清单不符，已丢弃"}])
    );
    assert!(!place.root.exists());
    let partial = partial_dir(&place.root);
    assert!(partial.join("big.onnx").is_file(), "finished files are kept for the next try");
    assert!(!part(&partial, &fixture.manifest.files[1]).exists(), "a mismatching part is deleted");
    let info = info_at(models, &place);
    let info: Value = serde_json::from_str(&info.dump()).unwrap();
    assert_eq!(info["missing"], json!(["sub/feat.onnx", "sub/metrics.json"]));
    assert_eq!(info["partial"], true);
    assert_eq!(info["have"], 300_000);
}

#[test]
fn an_earlier_part_resumes_or_restarts_when_the_source_cannot_resume() {
    for resumes in [true, false] {
        let fixture = Fixture::new();
        let mode = if resumes { Mode::Normal } else { Mode::NoRange };
        let server = server(fixture.routes(&[], mode, true));
        let place = fixture.place(&server);
        let partial = partial_dir(&place.root);
        fs::create_dir_all(&partial).unwrap();
        fs::write(part(&partial, &fixture.manifest.files[0]), &fixture.files[0].1[..50_000]).unwrap();
        let models = models();
        post(models, &place, json!({"action": "download"})).unwrap();
        assert_eq!(finished(models)["status"], "done");
        installed(&fixture, &place);
        let hits = server.hits.lock().unwrap().clone();
        assert_eq!(hits[0].1.as_deref(), Some("bytes=50000-"));
        assert_eq!(hits.len(), 3, "{hits:?}");
    }
}

#[test]
fn cancelling_keeps_the_part_and_discarding_removes_it() {
    let fixture = Fixture::new();
    let server = server(fixture.routes(&[("big.onnx", Mode::Slow)], Mode::Normal, true));
    let place = fixture.place(&server);
    let models = models();
    post(models, &place, json!({"action": "download"})).unwrap();
    let started = Instant::now();
    while models.snapshot()["done"].as_u64().unwrap_or(0) == 0 {
        assert!(started.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(post(models, &place, json!({"action": "discard"})).unwrap_err().to_string().contains("先取消"));
    assert!(post(models, &place, json!({"action": "import", "paths": ["x"]})).is_err(), "one job at a time");
    post(models, &place, json!({"action": "cancel"})).unwrap();
    let job = finished(models);
    assert_eq!(job["status"], "cancelled");
    assert_eq!(job["source"], host(&server.base));
    assert_eq!(job["file"], "big.onnx");
    let partial = partial_dir(&place.root);
    let kept = fs::metadata(part(&partial, &fixture.manifest.files[0])).unwrap().len();
    assert!(kept > 0 && kept < 300_000);
    assert!(!place.root.exists());
    let out = post(models, &place, json!({"action": "discard"})).unwrap();
    assert!(!partial.exists());
    assert_eq!(out["job"], Value::Null);
    assert_eq!(out["have"], 0);
}

#[test]
fn imported_files_and_zip_entries_are_matched_by_hash_whatever_their_names() {
    let fixture = Fixture::new();
    let place = Place {
        root: fixture.dir.join("models"),
        manifest: fixture.manifest,
        preferences: fixture.dir.join("preferences.json"),
        modelscope: "https://modelscope.invalid".into(),
        proxy: None,
    };
    let chosen = fixture.dir.join("downloads");
    fs::create_dir_all(&chosen).unwrap();
    // The big file inside a ZIP with an unrelated entry; the feature file renamed; a wrong file the JSON's size.
    let archive = chosen.join("models.zip");
    let mut zip = zip::ZipWriter::new(fs::File::create(&archive).unwrap());
    let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    zip.start_file("folder/model.onnx", options).unwrap();
    zip.write_all(&fixture.files[0].1).unwrap();
    zip.start_file("readme.txt", options).unwrap();
    zip.write_all(b"synthetic").unwrap();
    zip.finish().unwrap();
    fs::write(chosen.join("model (1).onnx"), &fixture.files[1].1).unwrap();
    let mut wrong = fixture.files[2].1.clone();
    wrong[0] ^= 1;
    fs::write(chosen.join("metrics.json"), &wrong).unwrap();
    let models = models();
    let paths = json!([archive, chosen.join("model (1).onnx"), chosen.join("metrics.json")]);
    post(models, &place, json!({"action": "import", "paths": paths})).unwrap();
    let job = finished(models);
    assert_eq!(job["status"], "done", "{job}");
    assert_eq!(
        job["results"],
        json!([
            {"name": "models.zip/folder/model.onnx", "result": "accepted", "path": "big.onnx"},
            {"name": "models.zip/readme.txt", "result": "unknown", "path": null},
            {"name": "model (1).onnx", "result": "accepted", "path": "sub/feat.onnx"},
            {"name": "metrics.json", "result": "mismatch", "path": "sub/metrics.json"},
        ])
    );
    assert!(!place.root.exists(), "not complete: nothing is enabled");
    let info: Value = serde_json::from_str(&info_at(models, &place).dump()).unwrap();
    assert_eq!(info["missing"], json!(["sub/metrics.json"]));
    fs::write(chosen.join("metrics.json"), &fixture.files[2].1).unwrap();
    post(
        models,
        &place,
        json!({"action": "import", "paths": [chosen.join("metrics.json"), chosen.join("model (1).onnx")]}),
    )
    .unwrap();
    let job = finished(models);
    assert_eq!(job["results"][0]["result"], "accepted");
    installed(&fixture, &place);
    // Once enabled, the same file again is reported as already there.
    post(models, &place, json!({"action": "import", "paths": [chosen.join("model (1).onnx")]})).unwrap();
    assert_eq!(finished(models)["results"][0]["result"], "already");
    for bad in [
        json!({"action": "import"}),
        json!({"action": "import", "paths": []}),
        json!({"action": "import", "paths": [""]}),
        json!({"action": "x"}),
        json!({"action": "cancel", "extra": 1}),
    ] {
        assert!(post(models, &place, bad).is_err());
    }
}

#[test]
fn an_existing_root_keeps_its_other_files_when_the_pack_is_moved_in() {
    let fixture = Fixture::new();
    let server = server(fixture.routes(&[], Mode::Normal, true));
    let place = fixture.place(&server);
    fs::create_dir_all(place.root.join("sub")).unwrap();
    fs::write(place.root.join("other.onnx"), b"kept").unwrap();
    fs::write(place.root.join("sub/feat.onnx"), b"too short").unwrap();
    let models = models();
    post(models, &place, json!({"action": "download"})).unwrap();
    assert_eq!(finished(models)["status"], "done");
    installed(&fixture, &place);
    assert_eq!(fs::read(place.root.join("other.onnx")).unwrap(), b"kept");
}

#[test]
fn the_mirror_is_an_https_address_kept_with_the_other_preferences() {
    let fixture = Fixture::new();
    let server = server(vec![]);
    let place = fixture.place(&server);
    crate::app_updates::update(&place.preferences, |p| {
        p.insert("language".into(), "en".into());
    })
    .unwrap();
    let models = models();
    let out = post(models, &place, json!({"action": "mirror", "mirror": "https://hf.example.org/"})).unwrap();
    assert_eq!(out["mirror"], "https://hf.example.org");
    assert_eq!(out["sources"][0], "hf.example.org");
    assert_eq!(
        place.sources(&fixture.manifest.files[1])[0].1,
        "https://hf.example.org/org/repo/resolve/abc123/upstream/sub/feat.onnx"
    );
    assert_eq!(
        place.sources(&fixture.manifest.files[1])[1].1,
        format!("{}/ms/models/org/repo/resolve/master/upstream/sub/feat.onnx", server.base)
    );
    for bad in [
        "http://hf.example.org",
        "ftp://x",
        "https://u:p@hf.example.org",
        "https://hf.example.org/?a=1",
        "http://127.0.0.1/",
        "x",
    ] {
        assert!(post(models, &place, json!({"action": "mirror", "mirror": bad})).is_err(), "{bad}");
    }
    assert_eq!(
        post(models, &place, json!({"action": "mirror", "mirror": "https://huggingface.co"})).unwrap()["mirror"],
        "https://huggingface.co"
    );
    let out = post(models, &place, json!({"action": "mirror", "mirror": null})).unwrap();
    assert_eq!(out["mirror"], "https://mirror.invalid");
    let prefs = crate::app_updates::read(&place.preferences);
    assert_eq!(prefs.get("language").unwrap(), "en");
    assert!(!prefs.contains_key("model_mirror"));
}

#[test]
fn system_proxy_settings_are_read_for_https() {
    let mac = "<dictionary> {\n  HTTPEnable : 1\n  HTTPPort : 7890\n  HTTPProxy : 127.0.0.1\n  HTTPSEnable : 1\n  HTTPSPort : 7891\n  HTTPSProxy : proxy.local\n}\n";
    assert_eq!(scutil_proxy(mac).as_deref(), Some("http://proxy.local:7891"));
    assert_eq!(scutil_proxy(&mac.replace("HTTPSEnable : 1", "HTTPSEnable : 0")), None);
    assert_eq!(scutil_proxy("<dictionary> {\n  ExceptionsList : <array> {\n  }\n}\n"), None);
    let win = "\r\nHKEY_CURRENT_USER\\Software\\Microsoft\\Windows\\CurrentVersion\\Internet Settings\r\n    ProxyEnable    REG_DWORD    0x1\r\n    ProxyServer    REG_SZ    127.0.0.1:7890\r\n";
    assert_eq!(windows_proxy(win).as_deref(), Some("http://127.0.0.1:7890"));
    assert_eq!(windows_proxy(&win.replace("0x1", "0x0")), None);
    let split = win.replace("127.0.0.1:7890", "http=a.local:1;https=b.local:2;socks=c.local:3");
    assert_eq!(windows_proxy(&split).as_deref(), Some("http://b.local:2"));
    assert_eq!(windows_proxy("    ProxyEnable    REG_DWORD    0x1\r\n"), None);
}

#[test]
fn the_built_in_manifest_lists_the_packaged_models() {
    let m = manifest();
    let paths: Vec<_> = m.files.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(
        paths,
        ["wd_swinv2_v3_emb.onnx", "ccip/model_feat.onnx", "ccip/model_metrics.onnx", "ccip/metrics.json"]
    );
    for e in &m.files {
        assert_eq!(e.sha256.len(), 64);
        assert!(e.hf.revision.len() == 40, "the Hugging Face address is pinned to a commit");
    }
    assert_eq!(base(&m.mirror).unwrap(), m.mirror);
    assert!(m.manual.starts_with("https://github.com/shio-vinyl/illustash/"));
    assert_eq!(configured_root(Path::new("/r"), None), Path::new("/r/models"));
    assert_eq!(configured_root(Path::new("/r"), Some(Path::new("m"))), Path::new("/r/m"));
    assert_eq!(partial_dir(Path::new("/s/models/pack.1")), Path::new("/s/models/pack.1.partial"));
}
