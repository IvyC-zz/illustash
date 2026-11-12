//! The comparison baseline of the first public test release (#201), replayed against this build.
//!
//! `tests/fixtures/baseline/` holds the scenarios of the former Python-era call recordings, recorded from main on
//! core version 7 (`record_baseline` in the former `tests/replay.rs`, PR #239): every call the core still serves once the legacy
//! tables are gone, its replies, and what it changed in the call's database. Calls that only served the legacy
//! tables (`sync-*`, `verify-*`, `cutover-*`, and calls on a library not yet switched to the core) are not in
//! it: the database they left is a checkpoint, put in place before the next call, and upgraded first when it is a
//! switched library at the core version the baseline was recorded on. What is compared, and how, is in `common/baseline.rs`.
//!
//! Each case runs in its own directory with its own key counter (`ILLUSTASH_TEST_KEY_COUNTER`), restored at
//! checkpoints, so the core makes the same keys as when recorded; learning stamps hash clocks and are passed on
//! from the reply they came from (`<stamp@N>` in an input is the N-th stamp seen in this case's replies).
#![allow(clippy::unwrap_used, reason = "集成测试里 unwrap 就是断言")]
use flate2::read::GzDecoder;
use regex::Regex;
use rusqlite::Connection;
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::LazyLock;

#[path = "common/baseline.rs"]
mod baseline;

static STAMPS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"learning-v1:[0-9a-f]{64}").unwrap());
static STAMP_REF: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<stamp@(\d+)>").unwrap());
const COMPANIONS: [&str; 4] = ["", "-wal", "-shm", "-journal"];

/// A case's temporary roots: `@ROOT@`, `@ROOT_1@`, … and the real directories they stand for.
struct Roots {
    /// (token, real path).
    roots: Vec<(String, String)>,
    /// (real path spelling, token), longest first: what `norm` replaces.
    spellings: Vec<(String, String)>,
}

impl Roots {
    fn new(base: &str, tokens: &[String]) -> Self {
        let mut roots = Vec::new();
        let mut spellings = Vec::new();
        for token in tokens {
            let real = if token == "@ROOT@" { base.to_owned() } else { format!("{base}/{}", token.trim_matches('@')) };
            fs::create_dir_all(&real).unwrap();
            if let Some(short) = real.strip_prefix("/private") {
                spellings.push((short.to_owned(), token.clone()));
            }
            spellings.push((real.clone(), token.clone()));
            roots.push((token.clone(), real));
        }
        spellings.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(&b.0)));
        Roots { roots, spellings }
    }

    fn norm(&self, text: &str) -> String {
        // Every spelling is an absolute path, so text without a `/` (most cells) has none of them.
        if !text.contains('/') {
            return text.to_owned();
        }
        let mut text = text.to_owned();
        for (real, token) in &self.spellings {
            text = text.replace(real.as_str(), token);
        }
        text
    }

    fn sub(&self, text: &str) -> String {
        let mut text = text.to_owned();
        for (token, real) in &self.roots {
            text = text.replace(token.as_str(), real);
        }
        text
    }
}

struct Serve {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Drop for Serve {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn remove(path: &Path) {
    for suffix in COMPANIONS {
        let p = PathBuf::from(format!("{}{suffix}", path.display()));
        if p.exists() {
            fs::remove_file(&p).unwrap();
        }
    }
}

fn open(path: &Path) -> Connection {
    Connection::open(path).unwrap_or_else(|e| {
        let units = path.as_os_str().to_string_lossy().encode_utf16().count();
        panic!("cannot open {} ({units} UTF-16 units; a shorter temporary directory may help): {e}", path.display())
    })
}

fn short(text: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(text.as_bytes()).iter().take(8).map(|b| format!("{b:02x}")).collect()
}

struct Replay<'a> {
    name: &'a str,
    blobs: &'a Value,
    bin: &'a Path,
    /// The core version the baseline was recorded on: checkpoints at it are upgraded to this build's.
    recorded: i64,
    roots: Roots,
    counter: PathBuf,
    verified: PathBuf,
    serve: Option<Serve>,
    expected: HashMap<PathBuf, baseline::State>,
    stamps: Vec<String>,
    /// Checkpoints whose upgrade changed compared content: named when a later comparison fails.
    upgraded: Vec<String>,
}

impl Replay<'_> {
    fn blob(&self, id: &Value) -> &str {
        self.blobs[id.as_str().unwrap()].as_str().unwrap()
    }

    fn state(&self, path: &Path) -> baseline::State {
        self.state_at(path, illustash_terms::SCHEMA_VERSION)
    }

    /// The compared content, `version` being the current core version (a checkpoint as recorded: the recorded one).
    fn state_at(&self, path: &Path, version: i64) -> baseline::State {
        if !path.exists() {
            return baseline::State::new();
        }
        let norm = |t: &str| self.roots.norm(t);
        baseline::state(&open(path), &baseline::Masks { roots: &norm, version })
    }

    fn output(&self, text: &str) -> String {
        let norm = |t: &str| self.roots.norm(t);
        baseline::Masks { roots: &norm, version: illustash_terms::SCHEMA_VERSION }.output(text)
    }

    /// Puts a call's files and checkpoints in place.
    fn environment(&mut self, call: &Value) -> Result<(), String> {
        for entry in call.get("env").and_then(Value::as_array).into_iter().flatten() {
            let path = PathBuf::from(self.roots.sub(entry["path"].as_str().unwrap()));
            remove(&path);
            if entry.get("delete").is_some() {
                self.expected.remove(&path);
                continue;
            }
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            if let Some(from) = entry.get("copy").and_then(Value::as_str) {
                let from = PathBuf::from(self.roots.sub(from));
                let source = open(&from);
                let mut target = open(&path);
                rusqlite::backup::Backup::new(&source, &mut target)
                    .unwrap()
                    .run_to_completion(1024, Default::default(), None)
                    .unwrap();
                match self.expected.get(&from).cloned() {
                    Some(state) => self.expected.insert(path, state),
                    None => self.expected.remove(&path),
                };
            } else if let Some(parts) = entry.get("db").and_then(Value::as_array) {
                let [tables, rows, objects] = [&parts[0], &parts[1], &parts[2]].map(|p| self.roots.sub(self.blob(p)));
                baseline::restore(&open(&path), &tables, &rows, &objects);
                let base = self.state_at(&path, self.recorded);
                // A checkpoint no later call of the baseline uses can be a library not yet switched: not upgradable.
                if entry["terms"].as_i64() == Some(self.recorded) && !baseline::before_cutover(&open(&path)) {
                    if let Some(change) = self.upgrade(&path, &base)? {
                        self.upgraded.push(change);
                    }
                }
                self.expected.insert(path, base);
            } else {
                let text = self.blob(&entry["file"]);
                let bytes = match text.strip_prefix("x:") {
                    Some(h) => (0..h.len()).step_by(2).map(|i| u8::from_str_radix(&h[i..i + 2], 16).unwrap()).collect(),
                    None => self.roots.sub(text).into_bytes(),
                };
                fs::write(&path, bytes).unwrap();
                self.expected.remove(&path);
            }
        }
        Ok(())
    }

    /// A checkpoint at the recorded core version is upgraded to this build's before the next call, as the user's
    /// libraries are. Backups it makes are removed, so the directory looks as it did when recorded. Returns what the
    /// upgrade changed in the compared content, if anything.
    fn upgrade(&self, path: &Path, base: &baseline::State) -> Result<Option<String>, String> {
        let backups = path.parent().unwrap().join("library-backups");
        let existing: Vec<PathBuf> =
            fs::read_dir(&backups).map(|d| d.map(|e| e.unwrap().path()).collect()).unwrap_or_default();
        let done = Command::new(self.bin)
            .args(["migrate", "--db"])
            .arg(path)
            .env("ILLUSTASH_TEST_KEY_COUNTER", &self.counter)
            .output()
            .unwrap();
        if !done.status.success() {
            return Err(format!(
                "upgrading checkpoint {}: {}",
                self.roots.norm(&path.to_string_lossy()),
                String::from_utf8_lossy(&done.stderr)
            ));
        }
        if let Ok(entries) = fs::read_dir(&backups) {
            for entry in entries {
                let made = entry.unwrap().path();
                if !existing.contains(&made) {
                    fs::remove_file(&made).unwrap();
                }
            }
            if existing.is_empty() {
                let _ = fs::remove_dir(&backups);
            }
        }
        // What the upgrade changed (repairing an outdated mirror, say) is not a failure by itself: every later call
        // is compared against the baseline, so it fails there if the change shows; then the change is named too.
        let after = self.state(path);
        Ok((after != *base).then(|| {
            format!(
                "upgrading checkpoint {} changed {:?}",
                self.roots.norm(&path.to_string_lossy()),
                baseline::differences(base, &after, 5)
            )
        }))
    }

    fn input(&self, text: &str) -> Result<String, String> {
        let text = self.roots.sub(text);
        let mut missing = None;
        let out = STAMP_REF
            .replace_all(&text, |c: &regex::Captures| {
                let n: usize = c[1].parse().unwrap();
                self.stamps.get(n).cloned().unwrap_or_else(|| {
                    missing = Some(n);
                    String::new()
                })
            })
            .into_owned();
        match missing {
            Some(n) => Err(format!("input refers to stamp {n}, only {} seen", self.stamps.len())),
            None => Ok(out),
        }
    }

    fn call(&mut self, at: &str, call: &Value) -> Result<(), String> {
        self.environment(call)?;
        if let Some(n) = call.get("counter").and_then(Value::as_u64) {
            fs::write(&self.counter, n.to_string()).unwrap();
        }
        let db = call.get("db").and_then(Value::as_str).map(|d| PathBuf::from(self.roots.sub(d)));
        let mut before = db.as_ref().and_then(|d| self.expected.get(d).cloned()).unwrap_or_default();
        if let (Some(d), Some(change)) = (&db, call.get("before")) {
            before = baseline::apply(&before, change).map_err(|e| format!("{at}: before: {e}"))?;
            let actual = self.state(d);
            if actual != before {
                return Err(format!("{at}: state before the call: {:?}", baseline::differences(&before, &actual, 5)));
            }
        }
        if call["mode"] == "cli" {
            let args = call["args"]
                .as_array()
                .unwrap()
                .iter()
                .map(|a| self.input(a.as_str().unwrap()))
                .collect::<Result<Vec<_>, _>>()?;
            let stdin = call.get("stdin").and_then(Value::as_str).map(|i| self.input(i)).transpose()?;
            let mut child = Command::new(self.bin)
                .args(&args)
                .env("ILLUSTASH_TEST_KEY_COUNTER", &self.counter)
                .env(VERIFY_INCREMENTAL, &self.verified)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            if let Some(input) = stdin {
                child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
            }
            drop(child.stdin.take());
            let done = child.wait_with_output().unwrap();
            let code = done.status.code().unwrap_or(-1);
            if Some(i64::from(code)) != call["code"].as_i64() {
                return Err(format!("{at}: exit {code}, baseline {}", call["code"]));
            }
            for (key, bytes) in [("stdout", &done.stdout), ("stderr", &done.stderr)] {
                let text = String::from_utf8_lossy(bytes);
                self.stamps.extend(STAMPS.find_iter(&text).map(|m| m.as_str().to_owned()));
                let got = self.output(&text);
                if Some(got.as_str()) != call[key].as_str() {
                    return Err(format!("{at}: {key} {got:?}, baseline {:?}", call[key]));
                }
            }
        } else {
            let input: Value = serde_json::from_str(&self.input(&call["input"].to_string())?).unwrap();
            let line = serde_json::json!({"db": db.as_ref().unwrap().to_string_lossy(), "input": input});
            let (bin, counter, verified) = (self.bin, &self.counter, &self.verified);
            let s = self.serve.get_or_insert_with(|| {
                let mut child = Command::new(bin)
                    .arg("serve")
                    .env("ILLUSTASH_TEST_KEY_COUNTER", counter)
                    .env(VERIFY_INCREMENTAL, verified)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .spawn()
                    .unwrap();
                let (stdin, stdout) = (child.stdin.take().unwrap(), BufReader::new(child.stdout.take().unwrap()));
                Serve { child, stdin, stdout }
            });
            writeln!(s.stdin, "{line}").unwrap();
            s.stdin.flush().unwrap();
            let mut reply = String::new();
            s.stdout.read_line(&mut reply).unwrap();
            self.stamps.extend(STAMPS.find_iter(&reply).map(|m| m.as_str().to_owned()));
            let got = self.output(reply.trim_end_matches(['\r', '\n']));
            if Some(got.as_str()) != call["reply"].as_str() {
                return Err(format!("{at}: reply {got}, baseline {}", call["reply"]));
            }
        }
        if let Some(d) = db {
            let want = match call.get("change") {
                Some(change) => baseline::apply(&before, change).map_err(|e| format!("{at}: {e}"))?,
                None => before,
            };
            let got = self.state(&d);
            if got != want {
                return Err(format!("{at}: state differs: {:?}", baseline::differences(&want, &got, 5)));
            }
            self.expected.insert(d, want);
        }
        Ok(())
    }
}

/// The core's incremental-vs-full names check (#102; test builds only), on for every call: a divergence fails the
/// call it happens in.
const VERIFY_INCREMENTAL: &str = "ILLUSTASH_TEST_VERIFY_INCREMENTAL";

fn case(name: &str, data: &Value, blobs: &Value, recorded: i64, bin: &Path, verified: &Path) -> Option<String> {
    let temp = std::env::temp_dir().join(format!("illustash-baseline-{}-{}", std::process::id(), short(name)));
    let _ = fs::remove_dir_all(&temp);
    fs::create_dir_all(temp.join("root")).unwrap();
    // Paths in the baseline use `/`, often inside JSON text; on Windows the root is written with `/` too.
    let base = if cfg!(windows) {
        temp.join("root").to_string_lossy().replace('\\', "/")
    } else {
        fs::canonicalize(temp.join("root")).unwrap().to_string_lossy().into_owned()
    };
    let tokens: Vec<String> =
        data["roots"].as_array().unwrap().iter().map(|t| t.as_str().unwrap().to_owned()).collect();
    let counter = temp.join("key-counter");
    fs::write(&counter, "0").unwrap();
    let mut replay = Replay {
        name,
        blobs,
        bin,
        recorded,
        roots: Roots::new(&base, &tokens),
        counter,
        verified: verified.to_path_buf(),
        serve: None,
        expected: HashMap::new(),
        stamps: Vec::new(),
        upgraded: Vec::new(),
    };
    let mut problem = None;
    for (i, call) in data["calls"].as_array().unwrap().iter().enumerate() {
        let op = call.get("args").and_then(|a| a[0].as_str()).or_else(|| call["input"]["op"].as_str()).unwrap_or("");
        let at = format!("{} #{i} {op}", replay.name);
        if let Err(e) = replay.call(&at, call) {
            problem =
                Some(if replay.upgraded.is_empty() { e } else { format!("{e}\n  ({})", replay.upgraded.join("; ")) });
            break; // later calls would only repeat the divergence
        }
    }
    drop(replay);
    let _ = fs::remove_dir_all(&temp);
    problem
}

/// SQLite counts every allocation under one process-wide mutex unless told not to (`SQLITE_CONFIG_MEMSTATUS`). Each
/// state read opens the databases and parses their schemas again, so the replay threads of all modules queued on that
/// mutex for most of the replay's time. Nothing here reads those counts.
static SQLITE_CONFIG: std::sync::Once = std::sync::Once::new();

fn replay(module: &str) {
    SQLITE_CONFIG.call_once(|| {
        // SAFETY: before any connection of this process (every test starts here); takes one int argument.
        let done = unsafe { rusqlite::ffi::sqlite3_config(rusqlite::ffi::SQLITE_CONFIG_MEMSTATUS, 0) };
        assert_eq!(done, rusqlite::ffi::SQLITE_OK, "SQLITE_CONFIG_MEMSTATUS");
    });
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("tests/fixtures/baseline/{module}.json.gz"));
    let mut text = String::new();
    GzDecoder::new(fs::File::open(&path).unwrap()).read_to_string(&mut text).unwrap();
    let data: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(data["format"], baseline::FORMAT, "{module}: baseline format");
    let recorded = data["recorded"]["terms_version"].as_i64().unwrap();
    let bin = Path::new(env!("CARGO_BIN_EXE_illustash-terms"));
    let work: Vec<(&String, &Value)> = data["cases"].as_object().unwrap().iter().collect();
    assert!(!work.is_empty(), "{module}: empty baseline");
    // Cases are independent (own directory, key counter and serve process): run them on a few threads.
    let verified = std::env::temp_dir().join(format!("illustash-baseline-{}-{module}.verified", std::process::id()));
    let _ = fs::remove_file(&verified);
    let threads = std::thread::available_parallelism().map_or(2, |n| n.get().min(4));
    let next = std::sync::atomic::AtomicUsize::new(0);
    let problems: Vec<String> = std::thread::scope(|s| {
        let workers: Vec<_> = (0..threads)
            .map(|_| {
                s.spawn(|| {
                    let mut found = Vec::new();
                    loop {
                        let i = next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        let Some((name, case_data)) = work.get(i) else { break found };
                        found.extend(case(name, case_data, &data["blobs"], recorded, bin, &verified));
                    }
                })
            })
            .collect();
        workers.into_iter().flat_map(|w| w.join().unwrap()).collect()
    });
    let checks = fs::read_to_string(&verified).unwrap_or_default().lines().count();
    let _ = fs::remove_file(&verified);
    eprintln!("{module}: {checks} incremental names checks");
    assert!(
        UNVERIFIED.contains(&module) || checks > 0,
        "{module}: the incremental names check never ran (built without illustash-core/verify-incremental?)"
    );
    assert!(
        problems.is_empty(),
        "{module}: {} of {} cases differ:\n{}",
        problems.len(),
        work.len(),
        problems.join("\n")
    );
}

/// Modules that commit no name changes on the incremental path (searches only, whole rebuilds, or only aliases, which
/// the core writes directly since #201): every other module must have run the core's incremental-vs-full check at
/// least once.
const UNVERIFIED: [&str; 3] = ["test_terms_core", "test_terms_names_contract", "test_terms_names_projection"];

macro_rules! modules {
    ($($name:ident),* $(,)?) => {$(
        #[test]
        fn $name() {
            replay(stringify!($name));
        }
    )*};
}

modules!(
    test_terms_core,
    test_terms_labels,
    test_terms_artists,
    test_terms_character_commands,
    test_terms_character_history,
    test_terms_character_identity,
    test_terms_character_import,
    test_terms_character_learning,
    test_terms_character_refusals,
    test_terms_character_structure,
    test_terms_characters_contract,
    test_terms_characters_cutover,
    test_terms_golden,
    test_terms_names_contract,
    test_terms_names_projection,
    test_terms_names_writes,
    test_identity,
    test_identity_hybrid,
    test_lead,
    test_web_v2,
    test_works,
    test_terms_artists_provenance,
    test_web,
);
