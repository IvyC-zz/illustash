use super::*;
use std::{
    collections::HashMap,
    io::{BufRead, BufReader},
    net::TcpListener,
    sync::{atomic::AtomicUsize, Arc},
};

/// 合成服务器对某个路径的应答方式。
#[derive(Clone)]
enum Mode {
    Normal,
    /// 忽略 Range：总是用 200 返回整个文件。
    NoRange,
    Missing,
    /// 大小相同，字节不同。
    Corrupt,
    /// 第一次应答声明完整长度，但发送这么多字节后就中断。
    DropOnce(usize),
    /// 慢速发送，用于测试取消。
    Slow,
    /// 302 到另一个路径（像 hf-mirror 跳转到 CDN 那样）；Range 头必须跟着过去。
    Redirect(String),
    /// 前 n 次读完请求就关掉连接，什么也不回（像握手时被对端断开那样，#642）；之后照常。
    CloseFirst(usize),
}
struct Route {
    body: Vec<u8>,
    mode: Mode,
    served: AtomicUsize,
}
/// 服务器收到的每个请求的（路径，Range 头）。
type Hits = Arc<Mutex<Vec<(String, Option<String>)>>>;
struct Server {
    base: String,
    /// 每个请求的（路径，Range 头）。
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
                let served = route.served.fetch_add(1, Ordering::SeqCst);
                let first_time = served == 0;
                let mut body = route.body.clone();
                match &route.mode {
                    Mode::CloseFirst(n) if served < *n => return,
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

fn with_variant(mut place: Place, server: &Server, data: &[u8]) -> Place {
    let mut manifest = place.manifest.clone();
    manifest.files.push(Entry {
        path: "coreml/wd.onnx".into(),
        size: data.len() as u64,
        sha256: sha(data),
        source: Source::Project { url: format!("{}/variant.onnx", server.base) },
        optional: true,
    });
    place.manifest = Box::leak(Box::new(manifest));
    place
}

#[test]
fn optional_download_failure_keeps_originals_ready_and_retry_repairs_the_variant() {
    let fixture = Fixture::new();
    let data = bytes(45_678, 37);
    let server1 = server(fixture.routes(&[], Mode::Normal, true));
    let place = with_variant(fixture.place(&server1), &server1, &data);
    let models = models();
    post(models, &place, json!({"action": "download"})).unwrap();
    let job = finished(models);
    assert_eq!(job["status"], "failed");
    assert_eq!(job["failed_file"], "coreml/wd.onnx");
    assert!(ready_at(&place.root, place.manifest), "可选文件 404 不阻止识别");
    for (path, expected) in &fixture.files {
        assert_eq!(fs::read(place.root.join(path)).unwrap(), *expected);
    }
    let info: Value = serde_json::from_str(&info_at(models, &place).dump()).unwrap();
    assert_eq!(info["status"], "ready");
    assert_eq!(info["optional_missing"], true);
    assert_eq!(info["have"].as_u64().unwrap(), fixture.files.iter().map(|(_, b)| b.len() as u64).sum::<u64>());
    let partial = partial_dir(&place.root).join("coreml");
    fs::create_dir_all(&partial).unwrap();
    fs::write(partial.join("wd.onnx.part"), &data[..1_000]).unwrap();
    let server2 = serve(vec![("/variant.onnx", data.clone(), Mode::Normal)]);
    let repaired = with_variant(fixture.place(&server1), &server2, &data);
    for damage in [false, true] {
        if damage {
            fs::write(repaired.root.join("coreml/wd.onnx"), bytes(data.len(), 38)).unwrap();
        }
        post(models, &repaired, json!({"action": "download"})).unwrap();
        let job = finished(models);
        assert_eq!(job["status"], "done");
        // 只补一个文件时，进度的分母是它自己的大小（#607）。
        assert_eq!(job["total"], data.len() as u64, "{job}");
        assert_eq!(job["done"], job["total"]);
        assert_eq!(fs::read(repaired.root.join("coreml/wd.onnx")).unwrap(), data);
        for (path, expected) in &fixture.files {
            assert_eq!(fs::read(repaired.root.join(path)).unwrap(), *expected, "原始文件不受补下载影响");
        }
    }
    let hits = server2.hits.lock().unwrap();
    assert_eq!(hits.len(), 2, "只下载缺失或损坏的变体");
    assert_eq!(hits[0].1.as_deref(), Some("bytes=1000-"), "原始模型已就位时保留变体的断点");
}

#[test]
fn optional_file_can_be_imported_after_originals_are_ready() {
    let fixture = Fixture::new();
    let server = server(fixture.routes(&[], Mode::Normal, true));
    let original_place = fixture.place(&server);
    let models = models();
    post(models, &original_place, json!({"action": "download"})).unwrap();
    assert_eq!(finished(models)["status"], "done");
    let data = bytes(45_678, 37);
    let place = with_variant(original_place, &server, &data);
    let chosen = fixture.dir.join("renamed.onnx");
    fs::write(&chosen, &data).unwrap();
    for damage in [false, true] {
        if damage {
            fs::write(place.root.join("coreml/wd.onnx"), bytes(data.len(), 38)).unwrap();
        }
        post(models, &place, json!({"action": "import", "paths": [chosen]})).unwrap();
        let job = finished(models);
        assert_eq!(job["status"], "done");
        assert_eq!(job["results"][0]["result"], "accepted");
        assert_eq!(fs::read(place.root.join("coreml/wd.onnx")).unwrap(), data);
    }
}
fn secs(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).unwrap().as_secs()
}
fn status(shared: &Path, records: Value) {
    fs::create_dir_all(shared).unwrap();
    fs::write(shared.join(crate::recognition_compute::backend::STATUS_FILE), records.to_string()).unwrap();
}

#[test]
fn a_wd_fallback_from_before_the_variant_was_installed_is_dropped() {
    let installed = UNIX_EPOCH + Duration::from_millis(1_000_500);
    let record = |at: u64, reason: Value| json!({"requested": "coreml", "used": "cpu", "reason": reason, "at": at});
    let check = |wd: Value, installed: Option<SystemTime>| {
        let mut last = serde_json::Map::new();
        last.insert("wd".into(), wd);
        last.insert("ccip".into(), record(1, "ccip 失败".into()));
        drop_superseded_wd(&mut last, installed);
        assert!(last.contains_key("ccip"), "CCIP 的记录不看");
        last.contains_key("wd")
    };
    assert!(!check(record(999, "加速文件损坏".into()), Some(installed)), "装好之前的回退");
    assert!(check(record(1_000, "加速文件损坏".into()), Some(installed)), "同一秒：算作装好之后");
    assert!(check(record(1_001, "加速文件损坏".into()), Some(installed)), "装好之后的识别又回退了");
    assert!(check(record(999, Value::Null), Some(installed)), "没有回退的记录保留");
    assert!(check(record(999, "加速文件损坏".into()), None), "没有加速文件");
}

#[test]
fn the_models_state_stops_reporting_the_old_wd_fallback_once_the_variant_is_imported() {
    let fixture = Fixture::new();
    let server = server(fixture.routes(&[], Mode::Normal, true));
    let models = models();
    post(models, &fixture.place(&server), json!({"action": "download"})).unwrap();
    assert_eq!(finished(models)["status"], "done");
    let data = bytes(45_678, 37);
    let place = with_variant(fixture.place(&server), &server, &data);
    let shared = fixture.dir.join("shared-status");
    let reported = || acceleration_at(Some(&shared), &place)["last"].as_object().unwrap().clone();
    // 上一次识别：WD 缺加速文件，用了 CPU；CCIP 也回退了。
    let earlier = secs(SystemTime::now()) - 60;
    let fell = |at| json!({"requested": "coreml", "used": "cpu", "reason": "加速文件不可用", "at": at});
    status(&shared, json!({"wd": fell(earlier), "ccip": fell(earlier)}));
    assert!(reported().contains_key("wd"), "加速文件还没装：照常报告");
    // 所选文件的时间很早：装好的时间仍是现在。
    let chosen = fixture.dir.join("renamed.onnx");
    fs::write(&chosen, &data).unwrap();
    fs::File::options().write(true).open(&chosen).unwrap().set_modified(UNIX_EPOCH + Duration::from_secs(1)).unwrap();
    let start = SystemTime::now() - Duration::from_secs(1);
    post(models, &place, json!({"action": "import", "paths": [chosen]})).unwrap();
    assert_eq!(finished(models)["results"][0]["result"], "accepted");
    assert!(variant_installed(&place.root, place.manifest).unwrap() >= start);
    // 每次读取（页面重开、App 重启都只是再读一次）都一样：旧的 WD 记录不报，CCIP 照报。
    for _ in 0..2 {
        let last = reported();
        assert!(!last.contains_key("wd"));
        assert_eq!(last["ccip"]["reason"], "加速文件不可用");
    }
    // 下一次识别写了新记录：照常报告。
    status(&shared, json!({"wd": fell(secs(SystemTime::now()) + 1), "ccip": fell(earlier)}));
    assert_eq!(reported()["wd"]["reason"], "加速文件不可用");
}

/// 三个大小不同的合成文件：根目录里一个大文件，子文件夹里一个，再加一个小 JSON。
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
                    source: Source::Upstream {
                        hf: origin("org/repo", "abc123"),
                        modelscope: origin("org/repo", "master"),
                    },
                    optional: false,
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
            wd_coreml: None,
            character_index: None,
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
            on_demand: true,
            in_use: false,
            raw_archive: false,
        }
    }
    fn hf(&self, path: &str) -> String {
        format!("/hf/org/repo/resolve/abc123/upstream/{path}")
    }
    fn ms(&self, path: &str) -> String {
        format!("/ms/models/org/repo/resolve/master/upstream/{path}")
    }
    /// 由一个来源提供全部文件：`special` 里列出的按各自模式，其余按 `rest`。
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
        assert!(started.elapsed() < crate::test_wait::limit(), "job did not finish: {}", models.snapshot());
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
    // 镜像缺大文件，其他文件重定向到 CDN 路径；ModelScope 发大文件时中途断开。
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
    // 大文件：镜像 404，ModelScope 从 0 开始，然后恰好从断开的位置续传。
    let big: Vec<_> = hits.iter().filter(|(p, _)| p.ends_with("big.onnx")).cloned().collect();
    assert_eq!(big[0].0, fixture.hf("big.onnx"));
    assert_eq!(big[1], (fixture.ms("big.onnx"), None));
    assert_eq!(big[2], (fixture.ms("big.onnx"), Some("bytes=120000-".into())));
    assert_eq!(big.len(), 3);
    // 重定向的文件来自镜像的 CDN；没有别的文件向 ModelScope 请求。
    assert!(hits.iter().any(|(p, _)| p == "/cdn/feat.onnx"));
    assert!(!hits.iter().any(|(p, _)| p.starts_with("/ms/") && !p.ends_with("big.onnx")));
    let info = post(models, &place, json!({"action": "download"})).unwrap_err();
    assert!(info.to_string().contains("已经就位"));
}

/// 只有一个来源的文件（像角色资料包那样直接从项目 Release 下载），测同一来源上的重试（#642）。
fn project_place(fixture: &Fixture, server: &Server, data: &[u8]) -> Place {
    let manifest = Box::leak(Box::new(Manifest {
        format: 1,
        pack: "project.1".into(),
        manual: "https://example.invalid/packs".into(),
        mirror: String::new(),
        modelscope: String::new(),
        files: vec![Entry {
            path: "pack.bin".into(),
            size: data.len() as u64,
            sha256: sha(data),
            source: Source::Project { url: format!("{}/release/pack.bin", server.base) },
            optional: false,
        }],
        wd_coreml: None,
        character_index: None,
    }));
    Place { root: fixture.dir.join("shared/extensions/project.1"), manifest, ..fixture.place(server) }
}

#[test]
fn a_connection_closed_before_the_reply_is_tried_again_on_the_same_source() {
    let fixture = Fixture::new();
    let data = bytes(50_000, 3);
    let server = serve(vec![("/release/pack.bin", data.clone(), Mode::CloseFirst(2))]);
    let place = project_place(&fixture, &server, &data);
    let models = models();
    post(models, &place, json!({"action": "download"})).unwrap();
    let job = finished(models);
    assert_eq!(job["status"], "done", "{job}");
    assert_eq!(fs::read(place.root.join("pack.bin")).unwrap(), data);
    let hits = server.hits.lock().unwrap().clone();
    assert_eq!(hits, vec![("/release/pack.bin".to_owned(), None); 3], "断开两次，第三次下完；都从头请求");
}

#[test]
fn a_source_that_keeps_closing_the_connection_fails_after_a_few_tries() {
    let fixture = Fixture::new();
    let data = bytes(50_000, 5);
    let server = serve(vec![("/release/pack.bin", data.clone(), Mode::CloseFirst(usize::MAX))]);
    let place = project_place(&fixture, &server, &data);
    let models = models();
    post(models, &place, json!({"action": "download"})).unwrap();
    let job = finished(models);
    assert_eq!(job["status"], "failed", "{job}");
    assert_eq!(job["diagnostic"]["publication"], "not_applicable");
    assert_eq!(server.hits.lock().unwrap().len(), 4, "第一次加三次重试");
    assert_eq!(job["failed_file"], "pack.bin");
    let host = server.base.trim_start_matches("http://");
    assert_eq!(job["failures"], json!([{"source": host, "why": "网络连接中断或超时"}]));
    assert!(!place.root.join("pack.bin").exists());
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

    // 所有来源都损坏：任务失败并指出文件和哈希；根目录始终不会创建。
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
        assert!(started.elapsed() < crate::test_wait::limit());
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
        on_demand: true,
        in_use: false,
        raw_archive: false,
    };
    let chosen = fixture.dir.join("downloads");
    fs::create_dir_all(&chosen).unwrap();
    // ZIP 带目录条目和 CP437 文件名；特征模型文件改了名；另有一个与 JSON 大小相同但内容错误的文件。
    let archive = chosen.join("models.zip");
    let mut zip = zip::ZipWriter::new(fs::File::create(&archive).unwrap());
    let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    zip.add_directory("folder/", options).unwrap();
    zip.start_file_with_options(options.name(b"folder/mod\x82l.onnx")).unwrap();
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
            {"name": "models.zip/folder/modél.onnx", "result": "accepted", "path": "big.onnx"},
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
    // 启用之后，再导入同一个文件会报告已经就位。
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
fn reference_package_uses_the_whole_zip_for_download_and_import_verification() {
    let fixture = Fixture::new();
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    zip.start_file("synthetic.txt", zip::write::SimpleFileOptions::default()).unwrap();
    zip.write_all(b"reference pack fixture").unwrap();
    let good = zip.finish().unwrap().into_inner();
    let mut bad = good.clone();
    bad[0] ^= 1;
    let server = serve(vec![("/good.zip", good.clone(), Mode::Normal), ("/bad.zip", bad.clone(), Mode::Normal)]);
    let make_place = |root: &str, remote: &str| {
        let mut place = fixture.place(&server);
        let mut manifest = (*place.manifest).clone();
        manifest.files = vec![Entry {
            path: "fixture.zip".into(),
            size: good.len() as u64,
            sha256: sha(&good),
            source: Source::Project { url: format!("{}/{remote}", server.base) },
            optional: false,
        }];
        place.manifest = Box::leak(Box::new(manifest));
        place.root = fixture.dir.join(root);
        place.raw_archive = true;
        place
    };
    let invalid = make_place("invalid", "bad.zip");
    let valid = make_place("valid", "good.zip");
    let job = models();
    post(job, &invalid, json!({"action":"download"})).unwrap();
    assert_eq!(finished(job)["status"], "failed");
    assert!(!invalid.root.join("fixture.zip").exists());
    post(job, &valid, json!({"action":"download"})).unwrap();
    assert_eq!(finished(job)["status"], "done");
    assert_eq!(fs::read(valid.root.join("fixture.zip")).unwrap(), good);

    let renamed = fixture.dir.join("renamed.zip");
    fs::write(&renamed, &good).unwrap();
    post(job, &invalid, json!({"action":"import","paths":[renamed]})).unwrap();
    assert_eq!(finished(job)["results"][0]["result"], "accepted");
    post(job, &invalid, json!({"action":"import","paths":[renamed]})).unwrap();
    assert_eq!(finished(job)["results"][0]["result"], "already");
    fs::write(&renamed, bad).unwrap();
    post(job, &invalid, json!({"action":"import","paths":[renamed]})).unwrap();
    assert_eq!(finished(job)["results"][0]["result"], "mismatch");
    assert_eq!(fs::read(invalid.root.join("fixture.zip")).unwrap(), good);
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
    // reg.exe 用完整路径调用，从不经 PATH 查找。
    assert_eq!(reg_exe(Some(r"D:\Win".into())), Path::new(r"D:\Win").join("System32").join("reg.exe"));
    assert_eq!(reg_exe(None), Path::new(r"C:\Windows").join("System32").join("reg.exe"));
    assert_eq!(reg_exe(Some("".into())), reg_exe(None));
}

#[test]
fn the_built_in_manifest_lists_the_packaged_models() {
    let m = manifest();
    let paths: Vec<_> = m.files.iter().filter(|e| !e.optional).map(|e| e.path.as_str()).collect();
    assert_eq!(
        paths,
        ["wd_swinv2_v3_emb.onnx", "ccip/model_feat.onnx", "ccip/model_metrics.onnx", "ccip/metrics.json"]
    );
    for e in &m.files {
        assert_eq!(e.sha256.len(), 64);
        match &e.source {
            Source::Upstream { hf, .. } => assert_eq!(hf.revision.len(), 40, "HF 固定提交"),
            Source::Project { url } => {
                assert!(e.optional);
                assert!(
                    url.starts_with("https://github.com/shio-vinyl/illustash/releases/download/wd-coreml-v1/")
                        || url.starts_with(
                            "https://github.com/shio-vinyl/illustash/releases/download/character-index-v1/"
                        )
                );
            }
        }
    }
    let variant = m.wd_coreml.as_ref().unwrap();
    assert_eq!(variant.source_sha256, m.files[0].sha256);
    assert_eq!(variant.revision, 1);
    // 基础角色索引（#607）在每个平台都是可选文件，排在最后；WD 加速文件只在 Apple 芯片的 macOS 上。
    let index = m.character_index.as_ref().unwrap();
    assert_eq!(m.files.last().map(|e| (e.path.as_str(), e.optional)), Some((index.file.path.as_str(), true)));
    assert_eq!(index.release.ccip_sha256, m.files[1].sha256, "包的锚点来自清单里的 CCIP 模型");
    let variants = m.files.iter().filter(|e| e.optional && e.path != index.file.path).count();
    assert_eq!(variants == 1, cfg!(all(target_os = "macos", target_arch = "aarch64")));
    assert_eq!(base(&m.mirror).unwrap(), m.mirror);
    assert!(m.manual.starts_with("https://github.com/shio-vinyl/illustash/"));
    assert_eq!(configured_root(Path::new("/r"), None), Path::new("/r/models"));
    assert_eq!(configured_root(Path::new("/r"), Some(Path::new("m"))), Path::new("/r/m"));
    assert_eq!(partial_dir(Path::new("/s/models/pack.1")), Path::new("/s/models/pack.1.partial"));
}

#[test]
fn downloaded_models_are_deleted_only_when_this_install_downloads_them_and_nothing_uses_them() {
    let fixture = Fixture::new();
    let server = server(fixture.routes(&[], Mode::Normal, true));
    let place = fixture.place(&server);
    let models = models();
    post(models, &place, json!({"action": "download"})).unwrap();
    assert_eq!(finished(models)["status"], "done");
    installed(&fixture, &place);
    for (change, message) in [
        ((|p: &mut Place| p.on_demand = false) as fn(&mut Place), "自带识别模型"),
        (|p: &mut Place| p.in_use = true, "正在识别图片"),
    ] {
        let mut refused = place.clone();
        change(&mut refused);
        let error = post(models, &refused, json!({"action": "delete"})).unwrap_err().to_string();
        assert!(error.contains(message), "{error}");
        assert!(ready_at(&place.root, fixture.manifest));
    }
    // 之后某次下载留下的部分文件也一并删除。
    fs::create_dir_all(partial_dir(&place.root)).unwrap();
    fs::write(partial_dir(&place.root).join("left.part"), b"x").unwrap();
    let out = post(models, &place, json!({"action": "delete"})).unwrap();
    assert_eq!(out["status"], "missing");
    assert_eq!(out["have"], 0);
    assert_eq!(out["job"], Value::Null);
    assert!(!place.root.exists() && !partial_dir(&place.root).exists());
    assert!(!place.root.with_file_name("synthetic.1.deleting").exists());
    assert!(fs::read_dir(place.root.parent().unwrap()).unwrap().next().is_none());
    // 再删一次也无妨。
    assert_eq!(post(models, &place, json!({"action": "delete"})).unwrap()["status"], "missing");
}

#[cfg(windows)]
#[test]
fn the_system_reg_exe_is_called_by_its_full_path_and_read() {
    use std::os::windows::process::CommandExt;
    let reg = reg_exe(std::env::var_os("SystemRoot"));
    assert!(reg.is_absolute() && reg.is_file(), "{}", reg.display());
    // PATH 为空：仅凭完整路径必须能找到它。
    let out = std::process::Command::new(&reg)
        .args(["query", r"HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings"])
        .env("PATH", "")
        .creation_flags(0x08000000)
        .output()
        .unwrap();
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    // 无论本机怎么设置，读取都不会失败，并且只有启用时才报告代理。
    if windows_proxy(&text).is_some() {
        assert!(text.contains("0x1"));
    }
    let _ = system_proxy();
}
