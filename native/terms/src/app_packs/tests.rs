//! 包目录（#602）：签名、字段校验、本版本能用哪些条目、本机缓存的序号规则、卡片上的版本和「新版本」。
//! 夹具在 tests/fixtures/pack_catalog/（make.py 生成，全部合成数据；`packs.json.tauri.sig` 由 `cargo tauri signer` 写）。
use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

fn fixture(name: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/pack_catalog").join(name);
    fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn key(name: &str) -> String {
    String::from_utf8(fixture(name)).unwrap().trim().to_owned()
}

fn temp() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "illustash-packs-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn context(app: &str) -> Context {
    Context { app: Version::parse(app).unwrap(), builtin: builtin().unwrap() }
}

#[test]
fn signature_must_match_the_pack_key_and_the_exact_bytes() {
    let catalog = fixture("packs.json");
    let a = key("key-a.pub");
    verify(&catalog, &fixture("packs.json.sig"), &a).unwrap();
    // 用户发包时用的工具（cargo tauri signer）写出的签名，格式和应用认的一致。
    verify(&catalog, &fixture("packs.json.tauri.sig"), &key("tauri-key.pub")).unwrap();

    let wrong_key = verify(&catalog, &fixture("packs.json.sig-b"), &a).unwrap_err().reason();
    assert!(wrong_key.contains("不是用数据包的密钥签的"), "{wrong_key}");
    // 另一把钥匙的公钥核对 A 的签名，同样不认。
    assert!(verify(&catalog, &fixture("packs.json.sig"), &key("key-b.pub")).is_err());
    assert!(verify(&catalog, &fixture("packs.json.sig"), &key("tauri-key.pub")).is_err());

    let mut tampered = catalog.clone();
    let at = tampered.iter().position(|&b| b == b'1').unwrap();
    tampered[at] = b'2';
    let changed = verify(&tampered, &fixture("packs.json.sig"), &a).unwrap_err().reason();
    assert!(changed.contains("签名对不上"), "{changed}");
    let mut extra = catalog.clone();
    extra.push(b'\n');
    assert!(verify(&extra, &fixture("packs.json.sig"), &a).is_err());

    // 签名本身被改、不是 Tauri 格式、或给了别的文件的签名，都不认。
    let mut signature = fixture("packs.json.sig");
    let middle = signature.len() / 2;
    signature[middle] = if signature[middle] == b'A' { b'B' } else { b'A' };
    assert!(verify(&catalog, &signature, &a).is_err());
    assert!(verify(&catalog, b"not a signature", &a).is_err());
    assert!(verify(&catalog, &fixture("packs-2.json.sig"), &a).is_err());
    assert!(verify(&catalog, &fixture("packs.json.sig"), "not a key").is_err());
}

#[test]
fn placeholder_key_file_disables_the_catalog() {
    assert_eq!(configured(include_str!("../../pack-key.pub")), None);
    assert_eq!(configured("# 说明\n\n  RWQabc  \n# 末尾\n").as_deref(), Some("RWQabc"));
}

fn part(name: &str, base: Value) -> Value {
    json!({"format":if base.get("wd_model_sha256").is_some() {WD_FORMAT_V1} else {CCIP_FORMAT},"heads":2,
        "file":{"name":name,"size":8,"sha256":crate::strict_json::sha256_hex(name.as_bytes()),
            "url":format!("{RELEASE_PREFIX}tag/{name}")},
        "base":base,"license":"MIT"})
}

fn entry(version: &str, revision: u32) -> Value {
    let wd = json!({"wd_model_sha256":"ae25f5f12b2d5891728fb7768fa63d65bf13a83d6d4823e3a1bf7b42875c74ff",
        "wd_tags_sha256":WD_TAGS_SHA256});
    let ccip = json!({"ccip_model_sha256":"4ea118d16496274f4f6e08d3afc768cc592389e8f7f32f8732ce2215c228ac5f"});
    json!({
        "kind":PACK_KIND,"revision":revision,"version":version,"min_app":"0.2.0","license":"Apache-2.0 AND MIT",
        "components":{"wd":part(&format!("{version}-wd.bin"), wd),"ccip":part(&format!("{version}-ccip.zip"), ccip)}
    })
}

fn catalog(entries: Vec<Value>) -> Value {
    json!({"format":CATALOG_FORMAT,"serial":3,"packs":entries})
}

fn parse(value: &Value) -> Result<Catalog> {
    Catalog::parse(value.to_string().as_bytes())
}

#[test]
fn catalog_fields_are_checked_entry_by_entry() {
    let parsed = Catalog::parse(&fixture("packs.json")).unwrap();
    assert_eq!((parsed.serial, parsed.entries.len()), (1, 8));
    let good = catalog(vec![entry("0.2", 2), entry("1.0", 3)]);
    assert_eq!(parse(&good).unwrap().entries.len(), 2);
    // 不认识的字段照常读（以后的目录加字段时，要改含义就换格式或提高最低版本）。
    let mut extra = good.clone();
    extra["generated_by"] = "tools/pack_catalog.py".into();
    extra["packs"][0]["unknown"] = json!({"x": 1});
    extra["packs"][0]["components"]["wd"]["unknown"] = 1.into();
    assert!(parse(&extra).is_ok());
    // 两个版本可以共用一个组成部分的文件（下个月只换 WD 头）。
    let mut shared = good.clone();
    shared["packs"][1]["components"]["ccip"] = shared["packs"][0]["components"]["ccip"].clone();
    assert!(parse(&shared).is_ok());
    // 以后的种类：通用字段照查，没有组成部分也行。
    let mut future = good.clone();
    future["packs"][1] =
        json!({"kind":"future-pack","revision":1,"version":"future-v1","min_app":"9.0.0","license":"MIT"});
    assert!(parse(&future).is_ok());

    let wd = |c: &mut Value| c["packs"][0]["components"]["wd"].take();
    type Change = Box<dyn Fn(&mut Value)>;
    let broken: Vec<(&str, Change)> = vec![
        ("目录格式", Box::new(|c| c["format"] = "illustash-packs/2".into())),
        ("序号为零", Box::new(|c| c["serial"] = 0.into())),
        ("序号不是整数", Box::new(|c| c["serial"] = "3".into())),
        ("条目不是列表", Box::new(|c| c["packs"] = json!({}))),
        ("版本重复", Box::new(|c| c["packs"][1]["version"] = "0.2".into())),
        ("标记是空的", Box::new(|c| c["packs"][0]["stage"] = "".into())),
        ("标记带空格", Box::new(|c| c["packs"][0]["stage"] = "pre view".into())),
        ("标记不是文字", Box::new(|c| c["packs"][0]["stage"] = true.into())),
        ("修订号重复", Box::new(|c| c["packs"][1]["revision"] = 2.into())),
        ("缺组成部分", Box::new(|c| drop(c["packs"][0].as_object_mut().unwrap().remove("components")))),
        ("缺 CCIP 部分", Box::new(|c| drop(c["packs"][0]["components"].as_object_mut().unwrap().remove("ccip")))),
        (
            "SHA-256 不是十六进制",
            Box::new(|c| c["packs"][0]["components"]["wd"]["file"]["sha256"] = "x".repeat(64).into()),
        ),
        ("SHA-256 大写", Box::new(|c| c["packs"][0]["components"]["ccip"]["file"]["sha256"] = "A".repeat(64).into())),
        ("大小为零", Box::new(|c| c["packs"][0]["components"]["wd"]["file"]["size"] = 0.into())),
        ("文件过大", Box::new(|c| c["packs"][0]["components"]["wd"]["file"]["size"] = (FILE_LIMIT + 1).into())),
        (
            "地址不在本项目的 Release",
            Box::new(|c| c["packs"][0]["components"]["wd"]["file"]["url"] = "https://example.com/tag/x.bin".into()),
        ),
        (
            "地址里的文件名和 name 不同",
            Box::new(|c| {
                c["packs"][0]["components"]["wd"]["file"]["url"] = format!("{RELEASE_PREFIX}tag/other.bin").into()
            }),
        ),
        (
            "地址没有 tag",
            Box::new(|c| c["packs"][0]["components"]["ccip"]["file"]["url"] = format!("{RELEASE_PREFIX}x.zip").into()),
        ),
        ("文件名带路径", Box::new(|c| c["packs"][0]["components"]["wd"]["file"]["name"] = "../x.bin".into())),
        ("版本名带路径", Box::new(|c| c["packs"][0]["version"] = "0/2".into())),
        ("版本名有 ..", Box::new(|c| c["packs"][0]["version"] = "0..2".into())),
        ("修订号为零", Box::new(|c| c["packs"][0]["revision"] = 0.into())),
        ("头数为零", Box::new(|c| c["packs"][0]["components"]["wd"]["heads"] = 0.into())),
        ("二级头比头还多", Box::new(|c| c["packs"][0]["components"]["wd"]["candidate_only"] = 3.into())),
        ("最低版本不是 X.Y.Z", Box::new(|c| c["packs"][0]["min_app"] = "0.3".into())),
        ("缺少许可", Box::new(|c| c["packs"][0]["license"] = " ".into())),
        ("组成部分缺少许可", Box::new(|c| c["packs"][0]["components"]["ccip"]["license"] = "".into())),
        ("说明带控制字符", Box::new(|c| c["packs"][0]["note"] = json!({"zh-CN":"一\n行","en":"x"}))),
        (
            "缺 WD 标签表",
            Box::new(|c| c["packs"][0]["components"]["wd"]["base"] = json!({"wd_model_sha256": "a".repeat(64)})),
        ),
        ("CCIP 缺模型", Box::new(|c| c["packs"][0]["components"]["ccip"]["base"] = json!({}))),
        ("缺字段", Box::new(|c| drop(c["packs"][0].as_object_mut().unwrap().remove("min_app")))),
        ("组成部分不是对象", Box::new(move |c| drop(wd(c)))),
        (
            "条目过多",
            Box::new(|c| c["packs"] = json!((1..=65).map(|i| entry(&format!("v{i}"), i)).collect::<Vec<_>>())),
        ),
    ];
    for (what, change) in broken {
        let mut value = good.clone();
        change(&mut value);
        assert!(parse(&value).is_err(), "{what}");
    }
    assert!(Catalog::parse(b"not json").is_err());
}

#[test]
fn only_packs_this_build_can_use_are_offered() {
    let parsed = Catalog::parse(&fixture("packs.json")).unwrap();
    let versions = |app| parsed.packs(&context(app)).into_iter().map(|p| p.version).collect::<Vec<_>>();
    // 最低版本不够（1.1 要 99.0.0）、WD 格式不认识（1.2）、标签表（1.3）或 CCIP 模型（1.4）不是这个构建的、不认识的
    // 标记（1.5）、不认识的种类都不提供。
    assert_eq!(versions("0.2.0"), ["0.2", "1.0"]);
    // 最低版本只比 X.Y.Z：0.2.0 的测试版也能用；更旧的版本一个都不用。
    assert_eq!(versions("0.2.0-beta.1"), ["0.2", "1.0"]);
    assert!(versions("0.1.9").is_empty());
    assert_eq!(versions("99.0.0"), ["0.2", "1.0", "1.1"]);

    // 0.2 是测试版，1.0 是正式版：只是显示不同。
    let offered = parsed.packs(&context("0.2.0"));
    let stages = offered.iter().map(|p| (p.version.as_str(), p.preview)).collect::<Vec<_>>();
    assert_eq!(stages, [("0.2", true), ("1.0", false)]);
    assert_eq!(offered[0].view()["stage"], "preview");
    assert_eq!(offered[1].view()["stage"], "release");
    let v3 = offered[1].clone();
    assert_eq!((v3.revision, v3.heads(), v3.wd.format.as_str(), v3.wd.candidate_only), (3, 5, WD_FORMAT_V2, 1));
    assert_eq!((v3.wd.kind, v3.ccip.kind, v3.wd.revision), (Kind::Wd, Kind::Ccip, 3));
    assert_eq!(v3.note, Some(("合成测试包 3".into(), "Synthetic test pack 3".into())));
    // 组成部分按 SHA-256 放：不同版本的同一个文件放在同一个文件夹。
    assert_eq!(v3.wd.version, format!("wd-extension-{}", &v3.wd.file.sha256[..12]));

    // 基础模型换了的构建（这里用改了内置包的上下文代替），目录里的包都不能用。
    let mut other = context("0.2.0");
    other.builtin.wd.base = Base::Wd { model: "a".repeat(64), tags: WD_TAGS_SHA256.into() };
    assert!(parsed.packs(&other).is_empty());
    let mut other = context("0.2.0");
    other.builtin.ccip.base = Base::Ccip { model: "b".repeat(64) };
    assert!(parsed.packs(&other).is_empty());

    // 组成部分和内置的文件相同时用内置的那个（同一个文件夹、同一个下载地址）。
    let mut same = context("0.2.0");
    let v2 = parsed.packs(&same).remove(0);
    same.builtin.ccip.file.sha256 = v2.ccip.file.sha256.clone();
    let v2 = parsed.packs(&same).remove(0);
    assert_eq!((v2.ccip.version.as_str(), v2.ccip.builtin, v2.ccip.revision), ("ccip-longtail-v1", true, 2));

    // 旧格式的 WD 部分不能带二级头、CCIP 部分没有二级；和内置包同版本名、同修订号或两个文件都相同的条目交给内置的。
    let builtin = builtin().unwrap();
    let mut tiered = entry("1.7", 7);
    tiered["components"]["wd"]["candidate_only"] = 1.into();
    let mut ccip_tier = entry("1.8", 8);
    ccip_tier["components"]["ccip"]["candidate_only"] = 1.into();
    let same_version = entry(BUILTIN_VERSION, 9);
    let same_revision = entry("1.10", builtin.revision);
    let mut same_files = entry("1.11", 11);
    same_files["components"]["wd"]["file"]["sha256"] = builtin.wd.file.sha256.clone().into();
    same_files["components"]["ccip"]["file"]["sha256"] = builtin.ccip.file.sha256.clone().into();
    let parsed = parse(&catalog(vec![tiered, ccip_tier, same_version, same_revision, same_files])).unwrap();
    assert!(parsed.packs(&context("0.2.0")).is_empty());
}

#[test]
fn stage_marks_preview_packs_and_defaults_to_release() {
    // 内置的 0.1 是测试版。
    let builtin = builtin().unwrap();
    assert_eq!((builtin.version.as_str(), builtin.preview, builtin.revision), ("0.1", true, 1));
    assert_eq!(builtin.view()["stage"], "preview");
    let stage = |value: Option<&str>| {
        let mut e = entry("0.2", 2);
        if let Some(value) = value {
            e["stage"] = value.into();
        }
        parse(&catalog(vec![e])).unwrap().packs(&context("0.2.0")).into_iter().map(|p| p.preview).collect::<Vec<_>>()
    };
    // 没写就是正式版；写了 preview 是测试版；release 也是正式版；以后的标记结构照查，本版本不提供。
    assert_eq!(stage(None), [false]);
    assert_eq!(stage(Some("preview")), [true]);
    assert_eq!(stage(Some("release")), [false]);
    assert_eq!(stage(Some("rc")), Vec::<bool>::new());
}

#[test]
fn cache_keeps_the_newest_verified_catalog() {
    let dir = temp();
    let path = dir.join(CACHE_NAME);
    let a = key("key-a.pub");
    let adopt = |name: &str, sig: &str| adopt_at(&path, &fixture(name), &fixture(sig), &a);
    let serial = || read_cache(&path, &a).map(|(c, _)| c.serial);
    assert_eq!(serial(), None);
    // 签名不对的目录整份不用，本机什么都不写。
    assert!(adopt("packs.json", "packs.json.sig-b").is_err());
    assert!(!path.exists());
    assert_eq!(adopt("packs.json", "packs.json.sig").unwrap(), Adopted::Newer);
    assert_eq!(adopt("packs.json", "packs.json.sig").unwrap(), Adopted::Same);
    assert_eq!(serial(), Some(1));
    // 同一个序号、不同内容：不认，留着原来的。
    assert!(adopt("packs-1-alt.json", "packs-1-alt.json.sig").is_err());
    assert_eq!(read_cache(&path, &a).unwrap().1, fixture("packs.json"));
    assert_eq!(adopt("packs-2.json", "packs-2.json.sig").unwrap(), Adopted::Newer);
    assert_eq!(serial(), Some(2));
    // 旧目录重放回来不会盖掉新的。
    assert_eq!(adopt("packs.json", "packs.json.sig").unwrap(), Adopted::Older);
    assert_eq!(serial(), Some(2));
    // 换了密钥（应用更新后公钥不同）或缓存被改过：当作没有缓存。
    assert!(read_cache(&path, &key("key-b.pub")).is_none());
    let mut cache: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let mut bytes = STANDARD.decode(cache["catalog"].as_str().unwrap()).unwrap();
    let at = bytes.iter().position(|&b| b == b'2').unwrap();
    bytes[at] = b'9';
    cache["catalog"] = STANDARD.encode(&bytes).into();
    fs::write(&path, cache.to_string()).unwrap();
    assert!(read_cache(&path, &a).is_none());
    // 坏掉的缓存可以被一份验过签的目录替换。
    assert_eq!(adopt("packs.json", "packs.json.sig").unwrap(), Adopted::Newer);
    fs::remove_dir_all(dir).unwrap();
}

fn pack(version: &str, revision: u32) -> CharacterPack {
    let mut pack = builtin().unwrap();
    pack.version = version.into();
    pack.revision = revision;
    pack.builtin = revision == 1;
    pack.wd.file.sha256 = format!("{revision:064x}");
    pack.ccip.file.sha256 = format!("{:064x}", revision + 100);
    pack
}

#[test]
fn card_shows_the_enabled_or_newest_present_version_and_offers_newer_ones() {
    let packs = vec![pack("v1", 1), pack("v2", 2), pack("v3", 3)];
    let (wd, ccip) = (|r: u32| format!("{r:064x}"), |r: u32| format!("{:064x}", r + 100));
    let enabled = |w: Option<u32>, c: Option<u32>| {
        enabled_pack(&packs, w.map(wd).as_deref(), c.map(ccip).as_deref()).map(|p| p.version.clone())
    };
    // 两个组成部分都对得上的那个；只启用了一个，或换包时一边没换成：先按 WD、再按 CCIP 找含它的最新版本。
    assert_eq!(enabled(Some(2), Some(2)).as_deref(), Some("v2"));
    assert_eq!(enabled(Some(1), None).as_deref(), Some("v1"));
    assert_eq!(enabled(None, Some(3)).as_deref(), Some("v3"));
    assert_eq!(enabled(Some(2), Some(3)).as_deref(), Some("v2"));
    assert_eq!(enabled(Some(9), Some(3)).as_deref(), Some("v3"));
    assert_eq!(enabled(None, None), None);
    // 两个版本共用一个 CCIP 文件时，只启用了 CCIP 部分的图库算较新的那个。
    let mut shared = packs.clone();
    shared[2].ccip.file.sha256 = ccip(2);
    assert_eq!(enabled_pack(&shared, None, Some(&ccip(2))).map(|p| p.version.as_str()), Some("v3"));

    let present = |versions: &'static [&'static str]| move |p: &CharacterPack| versions.contains(&p.version.as_str());
    let card = |on: Option<&str>, here: &'static [&'static str]| {
        current(&packs, packs.iter().find(|p| Some(p.version.as_str()) == on), present(here)).version
    };
    // 启用了哪个就是哪个，不管本机有没有。
    assert_eq!(card(Some("v1"), &[]), "v1");
    assert_eq!(card(Some("v2"), &["v3"]), "v2");
    // 没启用：本机已有的最新一个；都没有就是最新的一个（直接下载它，不用先下旧版本）。
    assert_eq!(card(None, &["v1", "v2"]), "v2");
    assert_eq!(card(None, &[]), "v3");

    let offered =
        |version: &str| offer(&packs, packs.iter().find(|p| p.version == version).unwrap()).map(|p| p.version);
    assert_eq!(offered("v1").as_deref(), Some("v3"));
    assert_eq!(offered("v2").as_deref(), Some("v3"));
    assert_eq!(offered("v3"), None);
    assert_eq!(offer(&packs[..1], &packs[0]), None);

    // 比新旧只看修订号，不看版本名怎么写，也不看是不是测试版。
    let mut named = vec![pack("1.0", 2), pack("0.9", 3), pack("0.2", 1)];
    named[1].preview = true;
    assert_eq!(offer(&named, &named[0]).map(|p| p.version).as_deref(), Some("0.9"));
    assert_eq!(current(&named, None, |_| false).version, "0.9");
    assert_eq!(current(&named, None, |p| p.version != "0.9").version, "1.0");
}

#[test]
fn offline_bundles_carry_their_catalog_and_both_files() {
    for (bundle, files) in [
        ("character-pack-0.2-offline.zip", ["wd-extension-v2.bin", "ccip-longtail-v1.zip"]),
        ("character-pack-1.0-offline.zip", ["wd-extension-v3.bin", "ccip-longtail-v2.zip"]),
    ] {
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(fixture(bundle))).unwrap();
        let mut read = |name: &str| {
            let mut bytes = Vec::new();
            archive.by_name(name).unwrap().read_to_end(&mut bytes).unwrap();
            bytes
        };
        verify(&read(CATALOG_NAME), &read(SIGNATURE_NAME), &key("key-a.pub")).unwrap();
        for file in files {
            assert_eq!(read(file), fixture(file));
        }
    }
}
