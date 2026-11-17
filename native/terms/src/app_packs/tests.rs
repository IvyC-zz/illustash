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
    Context { app: Version::parse(app).unwrap(), wd: builtin_wd().unwrap(), ccip: builtin_ccip().unwrap() }
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

fn entry(version: &str, revision: u32) -> Value {
    json!({
        "kind":"wd-extension","revision":revision,"version":version,"format":WD_FORMAT_V1,"heads":2,
        "file":{"name":format!("{version}.bin"),"size":8,"sha256":format!("{:064x}", revision),
            "url":format!("{RELEASE_PREFIX}{version}/{version}.bin")},
        "base":{"wd_model_sha256":"ae25f5f12b2d5891728fb7768fa63d65bf13a83d6d4823e3a1bf7b42875c74ff",
            "wd_tags_sha256":WD_TAGS_SHA256},
        "min_app":"0.2.0","license":"Apache-2.0"
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
    assert_eq!((parsed.serial, parsed.entries.len()), (1, 7));
    let good = catalog(vec![entry("wd-extension-v2", 2), entry("wd-extension-v3", 3)]);
    assert_eq!(parse(&good).unwrap().entries.len(), 2);
    // 不认识的字段照常读（以后的目录加字段时，要改含义就换格式或提高最低版本）。
    let mut extra = good.clone();
    extra["generated_by"] = "tools/pack_catalog.py".into();
    extra["packs"][0]["unknown"] = json!({"x": 1});
    assert!(parse(&extra).is_ok());

    let broken: Vec<(&str, Box<dyn Fn(&mut Value)>)> = vec![
        ("目录格式", Box::new(|c| c["format"] = "illustash-packs/2".into())),
        ("序号为零", Box::new(|c| c["serial"] = 0.into())),
        ("序号不是整数", Box::new(|c| c["serial"] = "3".into())),
        ("条目不是列表", Box::new(|c| c["packs"] = json!({}))),
        ("版本重复", Box::new(|c| c["packs"][1]["version"] = "wd-extension-v2".into())),
        ("修订号重复", Box::new(|c| c["packs"][1]["revision"] = 2.into())),
        ("SHA-256 重复", Box::new(|c| c["packs"][1]["file"]["sha256"] = c["packs"][0]["file"]["sha256"].clone())),
        ("SHA-256 不是十六进制", Box::new(|c| c["packs"][0]["file"]["sha256"] = "x".repeat(64).into())),
        ("SHA-256 大写", Box::new(|c| c["packs"][0]["file"]["sha256"] = "A".repeat(64).into())),
        ("大小为零", Box::new(|c| c["packs"][0]["file"]["size"] = 0.into())),
        ("文件过大", Box::new(|c| c["packs"][0]["file"]["size"] = (FILE_LIMIT + 1).into())),
        (
            "地址不在本项目的 Release",
            Box::new(|c| {
                c["packs"][0]["file"]["url"] = "https://example.com/wd-extension-v2/wd-extension-v2.bin".into()
            }),
        ),
        (
            "地址里的文件名和 name 不同",
            Box::new(|c| c["packs"][0]["file"]["url"] = format!("{RELEASE_PREFIX}wd-extension-v2/other.bin").into()),
        ),
        (
            "地址没有 tag",
            Box::new(|c| c["packs"][0]["file"]["url"] = format!("{RELEASE_PREFIX}wd-extension-v2.bin").into()),
        ),
        ("文件名带路径", Box::new(|c| c["packs"][0]["file"]["name"] = "../wd-extension-v2.bin".into())),
        ("版本名带路径", Box::new(|c| c["packs"][0]["version"] = "wd/extension".into())),
        ("版本名有 ..", Box::new(|c| c["packs"][0]["version"] = "wd..v2".into())),
        ("修订号为零", Box::new(|c| c["packs"][0]["revision"] = 0.into())),
        ("头数为零", Box::new(|c| c["packs"][0]["heads"] = 0.into())),
        ("二级头比头还多", Box::new(|c| c["packs"][0]["candidate_only"] = 3.into())),
        ("最低版本不是 X.Y.Z", Box::new(|c| c["packs"][0]["min_app"] = "0.3".into())),
        ("缺少许可", Box::new(|c| c["packs"][0]["license"] = " ".into())),
        ("说明带控制字符", Box::new(|c| c["packs"][0]["note"] = json!({"zh-CN":"一\n行","en":"x"}))),
        ("缺 WD 标签表", Box::new(|c| c["packs"][0]["base"] = json!({"wd_model_sha256": "a".repeat(64)}))),
        ("CCIP 缺模型", Box::new(|c| c["packs"][0]["kind"] = "ccip-longtail".into())),
        ("缺字段", Box::new(|c| drop(c["packs"][0].as_object_mut().unwrap().remove("min_app")))),
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
fn only_entries_this_build_can_use_are_offered() {
    let parsed = Catalog::parse(&fixture("packs.json")).unwrap();
    let versions = |kind, app| parsed.packs(kind, &context(app)).into_iter().map(|p| p.version).collect::<Vec<_>>();
    // 版本过低（v4 要 99.0.0）、格式不认识（v5）、标签表不是这个构建的（v6）、不认识的种类都不提供。
    assert_eq!(versions(Kind::Wd, "0.2.0"), ["wd-extension-v2", "wd-extension-v3"]);
    assert_eq!(versions(Kind::Ccip, "0.2.0"), ["ccip-longtail-v2"]);
    // 最低版本只比 X.Y.Z：0.2.0 的测试版也能用；更旧的版本一个都不用。
    assert_eq!(versions(Kind::Wd, "0.2.0-beta.1"), ["wd-extension-v2", "wd-extension-v3"]);
    assert!(versions(Kind::Wd, "0.1.9").is_empty() && versions(Kind::Ccip, "0.1.9").is_empty());
    assert_eq!(versions(Kind::Wd, "99.0.0"), ["wd-extension-v2", "wd-extension-v3", "wd-extension-v4"]);

    let v3 = parsed.packs(Kind::Wd, &context("0.2.0")).pop().unwrap();
    assert_eq!((v3.revision, v3.format.as_str(), v3.heads, v3.candidate_only), (3, WD_FORMAT_V2, 3, 1));
    assert_eq!(v3.note, Some(("合成测试包 3".into(), "Synthetic test pack 3".into())));

    // 基础模型换了的构建（这里用改了内置包的上下文代替），目录里的 WD 包都不能用。
    let mut other = context("0.2.0");
    other.wd.base = Base::Wd { model: "a".repeat(64), tags: WD_TAGS_SHA256.into() };
    assert!(parsed.packs(Kind::Wd, &other).is_empty());
    let mut other = context("0.2.0");
    other.ccip.base = Base::Ccip { model: "b".repeat(64) };
    assert!(parsed.packs(Kind::Ccip, &other).is_empty());

    // 旧格式的包不能带二级头；和内置包同版本名、同修订号或同文件的条目交给内置的那条。
    let builtin = builtin_wd().unwrap();
    let mut tiered = entry("wd-extension-v7", 7);
    tiered["candidate_only"] = 1.into();
    let mut same_version = entry(&builtin.version, 8);
    same_version["file"]["url"] = format!("{RELEASE_PREFIX}x/{}.bin", builtin.version).into();
    let same_revision = entry("wd-extension-v9", builtin.revision);
    let mut same_file = entry("wd-extension-v10", 10);
    same_file["file"]["sha256"] = builtin.file.sha256.clone().into();
    let parsed = parse(&catalog(vec![tiered, same_version, same_revision, same_file])).unwrap();
    assert!(parsed.packs(Kind::Wd, &context("0.2.0")).is_empty());
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

fn pack(version: &str, revision: u32) -> Pack {
    let mut pack = builtin_wd().unwrap();
    pack.version = version.into();
    pack.revision = revision;
    pack.builtin = revision == 1;
    pack.file.sha256 = format!("{revision:064x}");
    pack
}

#[test]
fn card_shows_the_enabled_or_newest_present_version_and_offers_newer_ones() {
    let packs = vec![pack("v1", 1), pack("v2", 2), pack("v3", 3)];
    let sha = |revision: u32| format!("{revision:064x}");
    let present = |versions: &'static [&'static str]| move |p: &Pack| versions.contains(&p.version.as_str());
    let card = |enabled: Option<&str>, here: &'static [&'static str]| current(&packs, enabled, present(here)).version;
    // 启用了哪个就是哪个，不管本机有没有。
    assert_eq!(card(Some(&sha(1)), &[]), "v1");
    assert_eq!(card(Some(&sha(2)), &["v3"]), "v2");
    // 没启用：本机已有的最新一个；都没有就是最新的一个（直接下载它，不用先下旧版本）。
    assert_eq!(card(None, &["v1", "v2"]), "v2");
    assert_eq!(card(None, &[]), "v3");
    // 启用的版本不在可用列表里（目录不再列它）：按没启用算。
    assert_eq!(card(Some(&sha(9)), &["v1"]), "v1");

    let offered =
        |version: &str| offer(&packs, packs.iter().find(|p| p.version == version).unwrap()).map(|p| p.version);
    assert_eq!(offered("v1").as_deref(), Some("v3"));
    assert_eq!(offered("v2").as_deref(), Some("v3"));
    assert_eq!(offered("v3"), None);
    assert_eq!(offer(&packs[..1], &packs[0]), None);
}

#[test]
fn offline_bundles_carry_their_catalog() {
    // 离线包里的目录成员按名字读出来，交给 adopt；这里只核对夹具本身是一份完整的离线包。
    for bundle in ["wd-extension-v2-offline.zip", "ccip-longtail-v2-offline.zip"] {
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(fixture(bundle))).unwrap();
        let mut read = |name: &str| {
            let mut bytes = Vec::new();
            archive.by_name(name).unwrap().read_to_end(&mut bytes).unwrap();
            bytes
        };
        verify(&read(CATALOG_NAME), &read(SIGNATURE_NAME), &key("key-a.pub")).unwrap();
    }
}
