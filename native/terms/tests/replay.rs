//! Recorded terms-core call sequences, replayed against this build's `illustash-terms` (#9).
//!
//! Each fixture in `tests/fixtures/replay/` was recorded on 2026-10-07 from the former Python
//! reference tests of the same name, while those tests still passed against their fixed golden
//! snapshots: every core call (CLI arguments and stdin, or a resident `serve` request), the files
//! the test had put in place before it, the outputs, and per-table digests of the database after it.
//! The reference implementation has since been deleted; the recordings cannot be re-recorded.
//!
//! Comparison rules (mirrored from the recorder, keep them identical):
//! - paths under a test's temporary directories are written as `@ROOT@`, `@ROOT_1@`, …;
//! - `ts` and `*_at` columns, `term_packages.manifest`, `term_evidence.fingerprint` and the
//!   `identity_graph_version` meta value are left out (clocks, or hashes over random keys);
//! - clock-like text inside values becomes `<clock>`; evidence versions (`migration/…`), digests
//!   of tables holding the core's keys, become `<derived>`;
//! - the core draws its keys from `ILLUSTASH_TEST_KEY_COUNTER` (a test-only hook in
//!   `store::new_key`), reset to 0 for every case, so keys and their orderings repeat; keys
//!   first seen in a call are still matched by position (database order: table name, rowid,
//!   column; then output order) as a safeguard;
//! - learning stamps hash timestamps: they are substituted into later inputs (two recorded stamps
//!   may be one replayed stamp) and masked as `<stamp>` in output comparisons.
//!
//! Replay side only (#101): a case's temporary directory is masked, in every spelling, before keys
//! are looked for, so UUID-like segments of its path (an agent session's temp directory, say) are
//! not taken for keys the core made; `uuid_segment_in_temp_path` replays one module under such a path.
use flate2::read::GzDecoder;
use regex::Regex;
use rusqlite::{types::ValueRef, Connection};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::LazyLock;

static CLOCK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\d{4}-\d{2}-\d{2}[ T]\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})?").unwrap());
static VOLATILE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}|learning-v1:[0-9a-f]{64}").unwrap()
});
static STAMP: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^learning-v1:[0-9a-f]{64}$").unwrap());
static STAMPS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"learning-v1:[0-9a-f]{64}").unwrap());
static DERIVED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"migration[/:][0-9a-f]{16}").unwrap());
/// The operating system's own error text and number (recorded on macOS; Windows words and numbers it differently).
static OS_ERROR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[^：:\n]*\(os error \d+\)").unwrap());
const SKIP_COLUMNS: [(&str, &str); 2] = [("term_packages", "manifest"), ("term_evidence", "fingerprint")];
const DERIVED_META: [&str; 1] = ["identity_graph_version"];
const COMPANIONS: [&str; 4] = ["", "-wal", "-shm", "-journal"];
/// A v4-shaped path segment for `uuid_segment_in_temp_path` (#101).
const UUID_SEGMENT: &str = "3f2b8c1e-9a4d-4e7b-8c21-5d6e7f809a1b";

/// A case's temporary directory as text may spell it (#101).
struct Temp {
    /// Longest first.
    spellings: Vec<String>,
    /// Key-like values inside the directory's path.
    segments: Vec<String>,
}

impl Temp {
    fn new(dir: &Path) -> Self {
        let mut bases = vec![dir.to_string_lossy().into_owned()];
        if let Ok(canonical) = fs::canonicalize(dir) {
            bases.push(canonical.to_string_lossy().into_owned());
        }
        for base in bases.clone() {
            for prefix in [r"\\?\", "/private"] {
                if let Some(short) = base.strip_prefix(prefix) {
                    bases.push(short.to_owned());
                }
            }
        }
        let mut spellings = Vec::new();
        for base in &bases {
            let forward = base.replace('\\', "/");
            let backward = forward.replace('/', "\\");
            // As written, with `/`, with `\`, and with `\` escaped as in JSON text.
            spellings.extend([base.clone(), forward, backward.replace('\\', "\\\\"), backward]);
        }
        spellings.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
        spellings.dedup();
        let segments = bases
            .iter()
            .flat_map(|b| VOLATILE.find_iter(b).map(|m| m.as_str().to_owned()).collect::<Vec<_>>())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        Temp { spellings, segments }
    }

    /// Text to look for keys in: the directory masked. A key-like segment of its path still left
    /// over means a spelling not handled here; fail plainly rather than count it as a key.
    fn mask(&self, text: &str) -> String {
        let mut text = text.to_owned();
        for spelling in &self.spellings {
            text = text.replace(spelling.as_str(), "@TEMP@");
        }
        for segment in &self.segments {
            if let Some(at) = text.find(segment.as_str()) {
                let from = text[..at].char_indices().rev().nth(199).map_or(0, |(i, _)| i);
                let near: String = text[from..].chars().take(240 + segment.len()).collect();
                panic!(
                    "{segment} is part of the replay's temporary directory, spelled here in a way \
                     `Temp::new` does not mask (#101): {near:?}"
                );
            }
        }
        text
    }
}

/// Temporary roots of one case: (real path, token), longest real path first when substituting.
struct Roots(Vec<(String, String)>);

impl Roots {
    /// Real paths -> tokens (recorded spelling).
    fn norm(&self, text: &str) -> String {
        let mut pairs: Vec<(String, &str)> = Vec::new();
        for (real, token) in &self.0 {
            pairs.push((real.clone(), token));
            if let Some(short) = real.strip_prefix("/private") {
                pairs.push((short.to_owned(), token));
            }
        }
        pairs.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(&b.0)));
        pairs.dedup();
        let mut text = text.to_owned();
        for (real, token) in pairs {
            text = text.replace(&real, token);
        }
        text
    }
    /// Tokens -> real paths.
    fn sub(&self, text: &str) -> String {
        let mut text = text.to_owned();
        for (real, token) in &self.0 {
            text = text.replace(token.as_str(), real);
        }
        text
    }
}

fn remap(text: &str, mapping: &HashMap<String, String>) -> String {
    if mapping.is_empty() {
        return text.to_owned();
    }
    VOLATILE
        .replace_all(text, |c: &regex::Captures| mapping.get(&c[0]).cloned().unwrap_or_else(|| c[0].to_owned()))
        .into_owned()
}

/// Opens a database, naming the path and its length when that fails: a long temporary directory
/// can push recorded paths past what SQLite opens on Windows (MAX_PATH, 260 UTF-16 units).
fn open(path: &Path) -> Connection {
    Connection::open(path).unwrap_or_else(|e| {
        let units = path.as_os_str().to_string_lossy().encode_utf16().count();
        panic!("cannot open {} ({units} UTF-16 units; a shorter temporary directory may help): {e}", path.display())
    })
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn short(text: &str) -> String {
    hex(&Sha256::digest(text.as_bytes()))[..16].to_owned()
}

fn encode(value: ValueRef, roots: &Roots, back: &HashMap<String, String>) -> String {
    match value {
        ValueRef::Null => "n".into(),
        ValueRef::Integer(v) => format!("i{v}"),
        ValueRef::Real(v) => format!("r{:016x}", v.to_bits()),
        ValueRef::Text(v) => {
            let text = remap(&roots.norm(&String::from_utf8_lossy(v)), back);
            let text = CLOCK.replace_all(&text, "<clock>");
            format!("t{}", hex(DERIVED.replace_all(&text, "<derived>").as_bytes()))
        }
        ValueRef::Blob(v) => format!("b{}", hex(v)),
    }
}

fn tables(c: &Connection) -> Vec<String> {
    let mut s = c
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
        .unwrap();
    s.query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect()
}

fn rows(c: &Connection, query: &str, keep: &[usize], roots: &Roots, back: &HashMap<String, String>) -> Vec<String> {
    let mut s = c.prepare(query).unwrap();
    let mut q = s.query([]).unwrap();
    let mut out = Vec::new();
    while let Some(row) = q.next().unwrap() {
        out.push(keep.iter().map(|&i| encode(row.get_ref(i).unwrap(), roots, back)).collect::<Vec<_>>().join(","));
    }
    out.sort();
    out
}

/// Per-table digests; `back` turns this replay's random values into the recorded ones first.
fn digests(db: &Path, roots: &Roots, back: &HashMap<String, String>) -> BTreeMap<String, String> {
    let c = open(db);
    let mut out = BTreeMap::new();
    let schema = rows(
        &c,
        "SELECT type,name,tbl_name,sql FROM sqlite_master WHERE name NOT LIKE 'sqlite_%'",
        &[0, 1, 2, 3],
        roots,
        back,
    );
    out.insert("@schema".to_owned(), short(&schema.join("\n")));
    for name in tables(&c) {
        let cols: Vec<String> = {
            let mut s = c.prepare(&format!("PRAGMA table_info(\"{name}\")")).unwrap();
            s.query_map([], |r| r.get::<_, String>(1)).unwrap().map(Result::unwrap).collect()
        };
        let keep: Vec<usize> = (0..cols.len())
            .filter(|&i| {
                cols[i] != "ts"
                    && !cols[i].ends_with("_at")
                    && !SKIP_COLUMNS.contains(&(name.as_str(), cols[i].as_str()))
            })
            .collect();
        let query = if name == "meta" {
            let derived = DERIVED_META.iter().map(|k| format!("'{k}'")).collect::<Vec<_>>().join(",");
            format!("SELECT key, CASE WHEN key IN ({derived}) THEN '<derived>' ELSE value END FROM meta")
        } else {
            format!("SELECT * FROM \"{name}\"")
        };
        let header = keep.iter().map(|&i| cols[i].as_str()).collect::<Vec<_>>().join(",");
        let body = rows(&c, &query, &keep, roots, back);
        out.insert(name, short(&format!("{header}\n{}", body.join("\n"))));
    }
    out
}

/// Volatile values in the database, in recording order.
fn volatile(db: &Path, temp: &Temp) -> Vec<String> {
    if !db.exists() {
        return Vec::new();
    }
    let c = open(db);
    let (mut seen, mut order) = (HashSet::new(), Vec::new());
    for name in tables(&c) {
        let sql: Option<String> =
            c.query_row("SELECT sql FROM sqlite_master WHERE name=?", [&name], |r| r.get(0)).unwrap();
        let without_rowid = sql.unwrap_or_default().to_uppercase().contains("WITHOUT ROWID");
        let query = format!("SELECT * FROM \"{name}\"{}", if without_rowid { "" } else { " ORDER BY rowid" });
        let mut s = c.prepare(&query).unwrap();
        let n = s.column_count();
        let mut q = s.query([]).unwrap();
        while let Some(row) = q.next().unwrap() {
            for i in 0..n {
                if let ValueRef::Text(v) = row.get_ref(i).unwrap() {
                    for m in VOLATILE.find_iter(&temp.mask(&String::from_utf8_lossy(v))) {
                        if seen.insert(m.as_str().to_owned()) {
                            order.push(m.as_str().to_owned());
                        }
                    }
                }
            }
        }
    }
    order
}

/// Keys not yet mapped (first occurrence each), then every stamp in order.
fn fresh(text: &str, known: &HashSet<&String>) -> Vec<String> {
    let (mut keys, mut stamps): (Vec<String>, Vec<String>) = (Vec::new(), Vec::new());
    for m in VOLATILE.find_iter(text) {
        let m = m.as_str().to_owned();
        if STAMP.is_match(&m) {
            stamps.push(m);
        } else if !known.contains(&m) && !keys.contains(&m) {
            keys.push(m);
        }
    }
    keys.extend(stamps);
    keys
}

fn extend(
    mapping: &mut HashMap<String, String>,
    recorded: &[String],
    actual: &[String],
    at: &str,
) -> Result<(), String> {
    let split =
        |values: &[String]| -> (Vec<String>, Vec<String>) { values.iter().cloned().partition(|v| STAMP.is_match(v)) };
    let ((stamps, recorded), (actual_stamps, actual)) = (split(recorded), split(actual));
    for (r, a) in stamps.into_iter().zip(actual_stamps) {
        mapping.insert(r, a);
    }
    if recorded.len() != actual.len() {
        return Err(format!("{at}: {} recorded volatile values, {} replayed", recorded.len(), actual.len()));
    }
    for (r, a) in recorded.into_iter().zip(actual) {
        if let Some(old) = mapping.get(&r) {
            if *old != a {
                return Err(format!("{at}: {r} already maps to {old}, now {a}"));
            }
        }
        mapping.insert(r, a);
    }
    Ok(())
}

/// Replay value -> recorded value, keys only (stamps are many-to-one).
fn inverse(mapping: &HashMap<String, String>) -> HashMap<String, String> {
    mapping.iter().filter(|(k, _)| !STAMP.is_match(k)).map(|(k, v)| (v.clone(), k.clone())).collect()
}

/// Output text as compared: stamps, derived versions and operating-system error text masked.
fn masked(text: &str) -> String {
    let text = DERIVED.replace_all(&STAMPS.replace_all(text, "<stamp>"), "<derived>").into_owned();
    OS_ERROR.replace_all(&text, "<os error>").into_owned()
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

/// Put the files a call expects in place (recorded spelling -> this replay's paths and keys).
fn environment(call: &Value, blobs: &Value, roots: &Roots, mapping: &HashMap<String, String>) {
    let Some(env) = call.get("env").and_then(Value::as_object) else { return };
    let sub = |text: &str| roots.sub(&remap(text, mapping));
    for (key, value) in env {
        let path = PathBuf::from(sub(key));
        remove(&path);
        if value.is_null() {
            continue;
        }
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        if let Some(from) = value.get("copy").and_then(Value::as_str) {
            let source = open(Path::new(&sub(from)));
            let mut target = open(&path);
            rusqlite::backup::Backup::new(&source, &mut target)
                .unwrap()
                .run_to_completion(1024, Default::default(), None)
                .unwrap();
        } else if let Some(sql) = value.get("sql").and_then(Value::as_str) {
            // As recorded (Python's sqlite3): foreign keys off while the dump is restored.
            let c = open(&path);
            c.execute_batch("PRAGMA foreign_keys=OFF").unwrap();
            c.execute_batch(&sub(blobs[sql].as_str().unwrap())).unwrap();
        } else {
            let text = blobs[value["file"].as_str().unwrap()].as_str().unwrap();
            let bytes = match text.strip_prefix("x:") {
                Some(h) => (0..h.len()).step_by(2).map(|i| u8::from_str_radix(&h[i..i + 2], 16).unwrap()).collect(),
                None => sub(text).into_bytes(),
            };
            fs::write(&path, bytes).unwrap();
        }
    }
}

fn op(call: &Value) -> String {
    call.get("args").and_then(|a| a[0].as_str()).or_else(|| call["input"]["op"].as_str()).unwrap_or("").to_owned()
}

/// Replay one case; returns the first few differences.
fn case(name: &str, calls: &[Value], blobs: &Value, bin: &Path, under: &Path) -> Vec<String> {
    let temp = under.join(format!("illustash-replay-{}-{}", std::process::id(), short(name)));
    let _ = fs::remove_dir_all(&temp);
    fs::create_dir_all(temp.join("root")).unwrap();
    let masks = Temp::new(&temp);
    let counter = temp.join("key-counter");
    fs::write(&counter, "0").unwrap();
    // Recorded paths use `/`, often inside JSON text. On Windows the root is written with `/` too (a
    // canonical path would carry the verbatim `\\?\` prefix, and `\` would break JSON escapes).
    let base = if cfg!(windows) {
        temp.join("root").to_string_lossy().replace('\\', "/")
    } else {
        fs::canonicalize(temp.join("root")).unwrap().to_string_lossy().into_owned()
    };
    let text = serde_json::to_string(calls).unwrap();
    let mut tokens: Vec<String> =
        Regex::new(r"@ROOT(?:_\d+)?@").unwrap().find_iter(&text).map(|m| m.as_str().to_owned()).collect();
    tokens.push("@ROOT@".into());
    tokens.sort();
    tokens.dedup();
    let roots = Roots(
        tokens
            .into_iter()
            .map(|t| {
                let real = if t == "@ROOT@" { base.clone() } else { format!("{base}/{}", t.trim_matches('@')) };
                fs::create_dir_all(&real).unwrap();
                (real, t)
            })
            .collect(),
    );
    let mut mapping = HashMap::new();
    let mut serve: Option<Serve> = None;
    let mut problems = Vec::new();
    for (i, call) in calls.iter().enumerate() {
        let at = format!("{name} #{i} {}", op(call));
        environment(call, blobs, &roots, &mapping);
        let sub = |text: &str, mapping: &HashMap<String, String>| roots.sub(&remap(text, mapping));
        let db = call.get("db").and_then(Value::as_str).map(|d| PathBuf::from(sub(d, &mapping)));
        let before: HashSet<String> =
            db.as_deref().map(|d| volatile(d, &masks)).unwrap_or_default().into_iter().collect();
        let outputs = if call["mode"] == "cli" {
            let args: Vec<String> =
                call["args"].as_array().unwrap().iter().map(|a| sub(a.as_str().unwrap(), &mapping)).collect();
            let mut child = Command::new(bin)
                .args(&args)
                .env("ILLUSTASH_TEST_KEY_COUNTER", &counter)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            if let Some(input) = call["stdin"].as_str() {
                child.stdin.take().unwrap().write_all(sub(input, &mapping).as_bytes()).unwrap();
            }
            drop(child.stdin.take());
            let done = child.wait_with_output().unwrap();
            vec![
                ("code", Value::from(done.status.code().unwrap_or(-1))),
                ("stdout", Value::from(String::from_utf8_lossy(&done.stdout).into_owned())),
                ("stderr", Value::from(String::from_utf8_lossy(&done.stderr).into_owned())),
            ]
        } else {
            let s = serve.get_or_insert_with(|| {
                let mut child = Command::new(bin)
                    .arg("serve")
                    .env("ILLUSTASH_TEST_KEY_COUNTER", &counter)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .spawn()
                    .unwrap();
                let (stdin, stdout) = (child.stdin.take().unwrap(), BufReader::new(child.stdout.take().unwrap()));
                Serve { child, stdin, stdout }
            });
            let input: Value = serde_json::from_str(&sub(&call["input"].to_string(), &mapping)).unwrap();
            let line = serde_json::json!({"db": db.as_ref().unwrap().to_string_lossy(), "input": input});
            writeln!(s.stdin, "{line}").unwrap();
            s.stdin.flush().unwrap();
            let mut reply = String::new();
            s.stdout.read_line(&mut reply).unwrap();
            let reply: Value = serde_json::from_str(&reply).unwrap();
            vec![("code", reply["code"].clone()), ("result", Value::from(reply["result"].to_string()))]
        };
        let mut check = || -> Result<(), String> {
            if let Some(db) = &db {
                let new: Vec<String> = volatile(db, &masks).into_iter().filter(|v| !before.contains(v)).collect();
                let keys: Vec<String> = call
                    .get("keys")
                    .and_then(Value::as_array)
                    .map(|k| k.iter().map(|v| v.as_str().unwrap().to_owned()).collect())
                    .unwrap_or_default();
                extend(&mut mapping, &keys, &new, &format!("{at} database"))?;
            }
            for (key, got) in &outputs {
                let Some(got) = got.as_str() else { continue };
                let recorded = match *key {
                    "result" if call.get("result").is_some() => call["result"].to_string(),
                    "result" => continue,
                    _ => call[*key].as_str().unwrap_or("").to_owned(),
                };
                let rec = {
                    let known: HashSet<&String> = mapping.keys().collect();
                    fresh(&recorded, &known)
                };
                let act = {
                    let known: HashSet<&String> = mapping.values().collect();
                    fresh(&masks.mask(got), &known)
                };
                extend(&mut mapping, &rec, &act, &format!("{at} {key}"))?;
            }
            let back = inverse(&mapping);
            for (key, got) in &outputs {
                match (*key, got) {
                    ("code", got) if call["mode"] == "cli" => {
                        if *got != call["code"] {
                            return Err(format!("{at}: exit {got}, recorded {}", call["code"]));
                        }
                    }
                    ("code", got) => {
                        let failed = got.as_i64() != Some(0);
                        if failed != call.get("error").is_some() {
                            return Err(format!(
                                "{at}: code {got}, recorded {}",
                                call.get("error").unwrap_or(&Value::Null)
                            ));
                        }
                    }
                    ("result", got) => {
                        if let Some(want) = call.get("result") {
                            let got: Value =
                                serde_json::from_str(&masked(&remap(&roots.norm(got.as_str().unwrap()), &back)))
                                    .unwrap();
                            let want: Value = serde_json::from_str(&masked(&want.to_string())).unwrap();
                            if got != want {
                                return Err(format!("{at}: result {got}, recorded {want}"));
                            }
                        }
                    }
                    (key, got) => {
                        let got = masked(&remap(&roots.norm(got.as_str().unwrap()), &back));
                        if Some(got.as_str()) != call[key].as_str().map(masked).as_deref() {
                            return Err(format!("{at}: {key} {got:?}, recorded {:?}", call[key]));
                        }
                    }
                }
            }
            if let (Some(db), Some(want)) = (&db, call.get("after").and_then(Value::as_object)) {
                let got = digests(db, &roots, &back);
                let want: BTreeMap<String, String> =
                    want.iter().map(|(k, v)| (k.clone(), v.as_str().unwrap().to_owned())).collect();
                if got != want {
                    let tables: std::collections::BTreeSet<&String> =
                        got.keys().chain(want.keys()).filter(|k| got.get(*k) != want.get(*k)).collect();
                    return Err(format!("{at}: state differs in {tables:?}"));
                }
            }
            Ok(())
        };
        if let Err(problem) = check() {
            problems.push(problem);
            break; // later steps would only repeat the same divergence
        }
    }
    drop(serve);
    let _ = fs::remove_dir_all(&temp);
    problems
}

/// Replays a module with case directories under the system temporary directory.
fn replay(module: &str) {
    replay_in(module, &std::env::temp_dir());
}

fn replay_in(module: &str, under: &Path) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("tests/fixtures/replay/{module}.json.gz"));
    let mut text = String::new();
    GzDecoder::new(fs::File::open(&path).unwrap()).read_to_string(&mut text).unwrap();
    let data: Value = serde_json::from_str(&text).unwrap();
    let bin = Path::new(env!("CARGO_BIN_EXE_illustash-terms"));
    let cases = data["cases"].as_object().unwrap();
    assert!(!cases.is_empty(), "{module}: empty recording");
    // Cases are independent (own directory, key counter and serve process): run them on a few threads.
    let work: Vec<(&String, &Value)> = cases.iter().collect();
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
                        found.extend(case(name, case_data["calls"].as_array().unwrap(), &data["blobs"], bin, under));
                    }
                })
            })
            .collect();
        workers.into_iter().flat_map(|w| w.join().unwrap()).collect()
    });
    assert!(
        problems.is_empty(),
        "{module}: {} of {} cases differ:\n{}",
        problems.len(),
        cases.len(),
        problems.join("\n")
    );
}

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
    test_terms_names_sync,
    test_terms_names_writes,
    test_identity,
    test_identity_hybrid,
    test_lead,
    test_web_v2,
    test_works,
    test_terms_artists_provenance,
    test_web,
);

/// #101: a v4-shaped segment in the temporary path (an agent session's directory, say) is not a key
/// the core made.
#[test]
fn uuid_segment_in_temp_path() {
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    let outer = Cleanup(std::env::temp_dir().join(format!("illustash-replay-{}-uuid", std::process::id())));
    replay_in("test_terms_core", &outer.0.join(UUID_SEGMENT));
}
