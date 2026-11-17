//! #445–#449 共用的固定种子计时图库；只从显式执行的 release 计时测试进入。
use super::*;
use illustash_terms::{schema, store, Kind};
use rusqlite::params;
use std::collections::BTreeSet;
use std::io::{BufWriter, Write};

const IMAGES: usize = 50_000;
const TOPICS: usize = 3_000;
const CCIP_DIM: usize = 768;
const WD_DIM: usize = 1024;
const CLUSTERS: usize = 64;

struct Seed(u64);
impl Seed {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }

    fn vector(&mut self, dim: usize, cluster: usize) -> Vec<u8> {
        let mut vector = vec![0; dim * 4];
        let noise = (0.1 + self.unit() * 0.15) as f32;
        let jitter = CLUSTERS + self.next() as usize % (dim - CLUSTERS);
        vector[cluster * 4..cluster * 4 + 4].copy_from_slice(&(1.0 - noise * noise).sqrt().to_le_bytes());
        vector[jitter * 4..jitter * 4 + 4].copy_from_slice(&noise.to_le_bytes());
        vector
    }
}

pub(super) struct Library50k {
    pub package: PathBuf,
    pub tags: Vec<String>,
}

pub(super) fn populate(root: &Path) -> Library50k {
    let package = root.join("shared-50k.pilib");
    illustash_terms::app_create::create_folder(&package, &root.join("folder"), None, None, None).unwrap();
    let tag_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tags/tags_v3.csv");
    let tags: Vec<_> = csv::Reader::from_path(tag_path)
        .unwrap()
        .records()
        .map(|row| row.unwrap())
        .filter(|row| row.get(2) == Some("0"))
        .take(TOPICS)
        .map(|row| row.get(1).unwrap().to_owned())
        .collect();
    assert_eq!(tags.len(), TOPICS);
    let mut db = illustash_terms::open(&package.join("library.sqlite")).unwrap();
    let mut source = Connection::open(package.join("source.sqlite")).unwrap();
    let app = db.transaction().unwrap();
    let input = source.transaction().unwrap();
    schema::begin_writing(&app).unwrap();
    app.execute_batch(
        r#"CREATE TEMP TABLE seq AS WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<50000)
             SELECT i,printf('img-%05d',i) AS uuid, date('2024-01-01',printf('+%d days',i%1095)) AS date FROM n;
         INSERT INTO assets(uuid,filename,width,height,added_at,src_kind)
          SELECT uuid,uuid||'.png',120,150,date,'orig' FROM seq;
         INSERT INTO term_assets(id,namespace,key) SELECT i,'library',uuid FROM seq;
         INSERT INTO duplicates
          SELECT uuid,printf('img-%05d',i-(i+1)%2),2,(i+1)%2,date,i%2,1-i%2 FROM seq WHERE i<=2500;
         INSERT INTO terms(key,kind,origin)
          SELECT printf('fixture-char-%05d',i),'character','fixture' FROM seq WHERE i<=8000;
         INSERT INTO term_tree(term_id,seq,label_only,mascot,original)
          SELECT id,id,0,0,0 FROM terms WHERE key GLOB 'fixture-char-*';
         INSERT INTO memberships(uuid,entity,status,origin,sources,score,source_count,ccip_only,skey)
          SELECT uuid,printf('fixture-char-%05d',(i-1)%8000+1),'machine','fixture','WD',0.9,1,0,uuid FROM seq;
         INSERT INTO terms(key,kind,origin)
          SELECT printf('fixture-artist-%05d',i),'artist','fixture' FROM seq WHERE i<=4000;
         INSERT INTO term_projection(asset_id,term_id,state)
          SELECT i,t.id,'machine' FROM seq JOIN terms t ON t.key=printf('fixture-artist-%05d',(i-1)%4000+1)
          WHERE i<=12000;
         INSERT INTO actions(id,label,before_json) VALUES (1,'合成标签','[{"terms":"verdicts"}]'),
                                                         (2,'合成收藏','[{"terms":"verdicts"}]');"#,
    )
    .unwrap();
    let favorites = store::create_term(&app, Kind::Mark, Some(schema::FAVORITE_KEY), "user").unwrap();
    app.execute(
        "INSERT INTO term_verdicts(action_id,anchor,asset_id,term_id,verdict,origin)
         SELECT 2,2,i,?,'yes','user' FROM seq WHERE i%50 IN (1,2,3)",
        [favorites],
    )
    .unwrap();
    app.execute(
        "INSERT INTO term_projection(asset_id,term_id,state)
         SELECT asset_id,term_id,'confirmed' FROM term_verdicts WHERE action_id=2",
        [],
    )
    .unwrap();
    let topic_terms: Vec<_> = tags
        .iter()
        .map(|tag| {
            let term = store::create_term(&app, Kind::Topic, Some(&format!("fixture-topic:{tag}")), "fixture").unwrap();
            store::add_link(&app, term, "wd", tag, "", "fixture").unwrap();
            term
        })
        .collect();
    let safe = store::create_term(&app, Kind::Rating, Some("fixture-safe"), "fixture").unwrap();
    let r18 = store::create_term(&app, Kind::Rating, Some("fixture-r18"), "fixture").unwrap();
    store::add_link(&app, safe, "wd_rating", "general", "", "fixture").unwrap();
    store::add_link(&app, r18, "wd_rating", "explicit", "", "fixture").unwrap();
    let user_tags: Vec<_> = (0..300)
        .map(|i| {
            let name = format!("夹具标签 {i:03}");
            illustash_terms::legacy_labels::concept_by_name(&app, &name, Some("user")).unwrap().unwrap()
        })
        .collect();
    let total: f64 = (1..=TOPICS).map(|i| 1.0 / i as f64).sum();
    let mut sum = 0.0;
    let cdf: Vec<_> = (1..=TOPICS)
        .map(|i| {
            sum += 1.0 / i as f64 / total;
            sum
        })
        .collect();
    let features = package.join("features");
    fs::create_dir_all(&features).unwrap();
    let mut manifest = BufWriter::new(fs::File::create(features.join("full-manifest.csv")).unwrap());
    writeln!(manifest, "uuid").unwrap();
    let mut npy = BufWriter::new(fs::File::create(features.join("full-features.npy")).unwrap());
    let mut header = format!("{{'descr': '<f4', 'fortran_order': False, 'shape': ({IMAGES}, {CCIP_DIM}), }}");
    let padding = (64 - ((10 + header.len() + 1) % 64)) % 64;
    header.push_str(&" ".repeat(padding));
    header.push('\n');
    npy.write_all(b"\x93NUMPY\x01\x00").unwrap();
    npy.write_all(&(header.len() as u16).to_le_bytes()).unwrap();
    npy.write_all(header.as_bytes()).unwrap();

    let mut seed = Seed(0x9451_1400_5a6f_682d);
    let mut assets = input.prepare("INSERT INTO assets(uuid,preset,in_scope,trashed) VALUES (?,?,1,0)").unwrap();
    let mut labels =
        input.prepare("INSERT INTO labels(uuid,dim,value,conf,source) VALUES (?,'general',?,?,'wd_v3')").unwrap();
    let mut wd = input.prepare("INSERT INTO embeddings(uuid,model,vec) VALUES (?,'wd',?)").unwrap();
    let mut evidence = app
        .prepare(
            "INSERT INTO term_evidence(asset_id,term_id,source,method,version,score,metric,fingerprint)
         VALUES (?,?,'wd','tagger','fixture',?,'wd_probability',zeroblob(16))",
        )
        .unwrap();
    let mut projection = app
        .prepare(
            "INSERT INTO term_projection(asset_id,term_id,state,score,sources) VALUES (?,?,'machine',?,'wd.tagger')",
        )
        .unwrap();
    let mut verdict = app
        .prepare(
            "INSERT INTO term_verdicts(action_id,anchor,asset_id,term_id,verdict,origin) VALUES (1,1,?,?,'yes','user')",
        )
        .unwrap();
    for i in 1..=IMAGES {
        let uuid = format!("img-{i:05}");
        let preset = ["pixiv", "pixiv", "pixiv", "pixiv", "pixiv", "x", "x", "x", "other", "other"][i % 10];
        assets.execute(params![uuid, preset]).unwrap();
        let cluster = i % CLUSTERS;
        writeln!(manifest, "{uuid}").unwrap();
        npy.write_all(&seed.vector(CCIP_DIM, cluster)).unwrap();
        wd.execute(params![uuid, seed.vector(WD_DIM, cluster)]).unwrap();
        let mut topics = BTreeSet::new();
        while topics.len() < 30 {
            let sample = seed.unit();
            let index = cdf.partition_point(|&p| p < sample);
            topics.insert(index.min(TOPICS - 1));
        }
        for index in topics {
            let probability = 0.4 + seed.unit() * 0.55;
            let term = topic_terms[index];
            labels.execute(params![uuid, tags[index], probability]).unwrap();
            evidence.execute(params![i, term, probability]).unwrap();
            projection.execute(params![i, term, probability]).unwrap();
        }
        let term = if i / 10 % 10 < 4 { r18 } else { safe };
        evidence.execute(params![i, term, 0.9]).unwrap();
        projection.execute(params![i, term, 0.9]).unwrap();
        if i % 5 < 2 {
            verdict.execute(params![i, user_tags[(i / 5 + i / 300) % user_tags.len()]]).unwrap();
        }
    }
    manifest.flush().unwrap();
    npy.flush().unwrap();
    drop((assets, labels, wd, evidence, projection, verdict));
    schema::commit_writing(app).unwrap();
    input.commit().unwrap();
    Library50k { package, tags }
}

fn filter_url(path: &str, filter: &[(&str, &str)]) -> String {
    format!(
        "{path}?{}",
        url::form_urlencoded::Serializer::new(String::new()).extend_pairs(filter.iter().copied()).finish()
    )
}

fn get_ok(origin: &str, token: &str, url: &str) {
    let (status, value, _) = request_json(origin, url, None, token);
    assert_eq!(status, 200, "{url}: {value}");
    assert!(!value.is_null());
}

fn measure(origin: &str, token: &str, url: &str) -> f64 {
    for _ in 0..5 {
        get_ok(origin, token, url);
    }
    let start = Instant::now();
    for _ in 0..50 {
        get_ok(origin, token, url);
    }
    start.elapsed().as_secs_f64() * 1000.0 / 50.0
}

#[cfg(windows)]
fn peak_rss_mib(pid: u32) -> f64 {
    let cmd = format!("(Get-Process -Id {pid}).PeakWorkingSet64");
    let out =
        Command::new("powershell.exe").args(["-NoProfile", "-NonInteractive", "-Command", &cmd]).output().unwrap();
    assert!(out.status.success(), "无法读取宿主峰值工作集");
    let value: f64 = String::from_utf8(out.stdout).unwrap().trim().parse().unwrap();
    value / 1_048_576.0
}

fn medians(mut samples: BTreeMap<String, Vec<f64>>) {
    for (name, values) in &mut samples {
        assert_eq!(values.len(), 3);
        values.sort_by(f64::total_cmp);
        println!("BENCH {name} {:.3}", values[1]);
    }
}

#[test]
#[ignore = "只在空闲验收机上运行 release 宿主合成图库计时"]
fn benchmark_galleries_http() {
    if cfg!(debug_assertions) {
        panic!("需用 release 编译");
    }
    let candidate = Path::new(env!("CARGO_BIN_EXE_illustash-terms"));
    let baseline = PathBuf::from(std::env::var_os("ILLUSTASH_BENCH_BASELINE").expect("显式指定基线 release 宿主"));
    let (_scratch, root, _) = materialize(&json!({"input": {}}), "gallery-bench");
    let Library50k { package, tags } = populate(&root);
    let positive = json!([format!("tag:{}", tags[0]), format!("tag:{}", tags[1]), "source:pixiv"]).to_string();
    let negative = json!(["utag:夹具标签 000"]).to_string();
    let more: Vec<_> = tags.iter().take(5).map(|tag| format!("tag:{tag}")).chain(["source:pixiv".into()]).collect();
    let many = json!(more).to_string();
    let excluding_many = json!(["utag:夹具标签 000", "utag:夹具标签 001", "source:x", "source:other"]).to_string();
    let common = ("nsfw", "hide");
    let single = json!([format!("tag:{}", tags[0])]).to_string();
    let date = [("from", "2025-01-01"), ("to", "2025-12-31")];
    let sql = "INSERT INTO galleries(name,params,position,created_at,updated_at)
               VALUES (?,?,?,CURRENT_TIMESTAMP,CURRENT_TIMESTAMP)";
    let add_galleries = |positions: std::ops::Range<i64>| {
        let db = Connection::open(package.join("library.sqlite")).unwrap();
        for i in positions {
            let params = match i % 4 {
                0 => json!({"has":single}),
                1 => json!({"has":positive,"not":negative,"from":date[0].1,"to":date[1].1}),
                2 => json!({"has":"[\"fav\"]"}),
                _ => json!({"has":"[\"source:x\"]","from":"2024-10-01"}),
            };
            db.execute(sql, params![format!("合成画廊 {i}"), params.to_string(), i]).unwrap();
        }
    };
    add_galleries(0..20);
    let list_url = filter_url("/api/galleries", &[common]);
    // 原夹具前四个位置分别使用四组选区，模拟只请求当前可见画廊的封面。
    let covers_url = filter_url("/api/galleries/covers", &[("ids", "[1,2,3,4]"), common]);
    let old_page = filter_url("/api/assets", &[("tag", &tags[0]), common]);
    let old_count = filter_url("/api/assets/count", &[("tag", &tags[0]), common]);
    let legacy = [("page", old_page.as_str()), ("count", old_count.as_str()), ("detail", "/api/detail?id=img-00013")];
    let groups = [
        ("one", vec![("has", single.as_str()), common]),
        ("five", vec![("has", positive.as_str()), ("not", negative.as_str()), date[0], date[1], common]),
        ("ten", vec![("has", many.as_str()), ("not", excluding_many.as_str()), common]),
    ];
    let mut samples = BTreeMap::<String, Vec<f64>>::new();
    for trial in 0..3 {
        for (label, binary) in [("baseline", baseline.as_path()), ("candidate", candidate)] {
            let (_host, origin, token) = launch(&root, &package, "gallery-bench", false, binary);
            for (name, url) in legacy {
                samples.entry(format!("{label}_{name}_ms")).or_default().push(measure(&origin, &token, url));
            }
            #[cfg(windows)]
            samples.entry(format!("{label}_legacy_peak_rss_mib")).or_default().push(peak_rss_mib(_host.child.id()));
            if label == "candidate" {
                for (group, filters) in &groups {
                    for (route, path) in [("page", "/api/assets"), ("count", "/api/assets/count")] {
                        let url = filter_url(path, filters);
                        samples
                            .entry(format!("candidate_{group}_{route}_ms"))
                            .or_default()
                            .push(measure(&origin, &token, &url));
                    }
                }
                for (name, url, count) in
                    [("candidate_list_20_ms", list_url.as_str(), 20), ("candidate_covers_4_ms", covers_url.as_str(), 4)]
                {
                    samples.entry(name.into()).or_default().push(measure(&origin, &token, url));
                    let (status, body, _) = request_json(&origin, url, None, &token);
                    assert_eq!(status, 200, "{url}: {body}");
                    assert_eq!(body.as_array().unwrap().len(), count);
                }
            }
        }
        println!("TRIAL {trial} complete");
    }
    add_galleries(20..100);
    for _ in 0..3 {
        let (_host, origin, token) = launch(&root, &package, "gallery-bench", false, candidate);
        samples.entry("candidate_list_100_ms".into()).or_default().push(measure(&origin, &token, &list_url));
        let (status, body, _) = request_json(&origin, &list_url, None, &token);
        assert_eq!(status, 200, "{list_url}: {body}");
        assert_eq!(body.as_array().unwrap().len(), 100);
    }
    medians(samples);
}

#[test]
#[ignore = "只在空闲验收机上运行 release 宿主合成图库计时"]
fn benchmark_favorites_http() {
    if cfg!(debug_assertions) {
        panic!("需用 release 编译");
    }
    let binary = PathBuf::from(std::env::var_os("ILLUSTASH_FAVORITE_BINARY").expect("显式指定 #447 release 宿主"));
    let (_scratch, root, _) = materialize(&json!({"input": {}}), "favorite-bench");
    let Library50k { package, .. } = populate(&root);
    let mut samples = BTreeMap::<String, Vec<f64>>::new();
    let page = "/api/assets?nsfw=hide&limit=100";
    let marked = format!("{page}&marks=fav");
    let filtered = format!("{page}&has=%5B%22fav%22%5D");
    let writes: Vec<_> = [100, 1000]
        .into_iter()
        .map(|count| {
            let ids: Vec<_> = (12001..12001 + count).map(|n| format!("img-{n:05}")).collect();
            (count, json!({"ids":ids, "confirm":true}))
        })
        .collect();
    for run in 0..3 {
        let trial_package = root.join(format!("favorites-{run}.pilib"));
        fs::create_dir(&trial_package).unwrap();
        for filename in ["library.json", "library.sqlite", "source.sqlite"] {
            fs::copy(package.join(filename), trial_package.join(filename)).unwrap();
        }
        let (_host, origin, token) = launch(&root, &trial_package, "favorite-bench", false, &binary);
        let before = measure(&origin, &token, page);
        let after = measure(&origin, &token, &marked);
        samples.entry("favorite_marks_overhead_ms".into()).or_default().push(after - before);
        samples.entry("favorite_page_ms".into()).or_default().push(measure(&origin, &token, &filtered));
        for (count, body) in &writes {
            let mut write = 0.0;
            let mut undo = 0.0;
            for i in 0..55 {
                let start = Instant::now();
                let (status, value, _) = request_json(&origin, "/api/favorite", Some(body), &token);
                assert_eq!(status, 200, "{value}");
                let elapsed = start.elapsed().as_secs_f64() * 1000.0;
                let id = value["undo"]["id"].as_i64().unwrap();
                let start = Instant::now();
                let (status, value, _) = request_json(&origin, "/api/undo", Some(&json!({"id":id})), &token);
                assert_eq!(status, 200, "{value}");
                if i >= 5 {
                    write += elapsed;
                    undo += start.elapsed().as_secs_f64() * 1000.0;
                }
            }
            samples.entry(format!("favorite_write_{count}_ms")).or_default().push(write / 50.0);
            samples.entry(format!("favorite_undo_{count}_ms")).or_default().push(undo / 50.0);
        }
        println!("TRIAL {run} complete");
    }
    medians(samples);
}
