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
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::LazyLock;

#[path = "common/baseline.rs"]
mod baseline;

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
    /// Matches any spelling or segment; text without one is left as it is (most database cells).
    filter: Regex,
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
        let filter = literals(spellings.iter().chain(&segments));
        Temp { spellings, segments, filter }
    }

    /// Text to look for keys in: the directory masked. A key-like segment of its path still left
    /// over means a spelling not handled here; fail plainly rather than count it as a key.
    fn mask(&self, text: &str) -> String {
        if !self.filter.is_match(text) {
            return text.to_owned();
        }
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
struct Roots {
    roots: Vec<(String, String)>,
    /// Real paths, with and without `/private`, longest first: what `norm` replaces, in order.
    pairs: Vec<(String, String)>,
    /// Matches any of `pairs`; text without one is left as it is (most database cells).
    filter: Regex,
}

impl Roots {
    fn new(roots: Vec<(String, String)>) -> Self {
        let mut pairs = Vec::new();
        for (real, token) in &roots {
            pairs.push((real.clone(), token.clone()));
            if let Some(short) = real.strip_prefix("/private") {
                pairs.push((short.to_owned(), token.clone()));
            }
        }
        pairs.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(&b.0)));
        pairs.dedup();
        let filter = literals(pairs.iter().map(|(real, _)| real));
        Roots { roots, pairs, filter }
    }
    /// Real paths -> tokens (recorded spelling).
    fn norm(&self, text: &str) -> String {
        if !self.filter.is_match(text) {
            return text.to_owned();
        }
        let mut text = text.to_owned();
        for (real, token) in &self.pairs {
            text = text.replace(real.as_str(), token);
        }
        text
    }
    /// Tokens -> real paths.
    fn sub(&self, text: &str) -> String {
        let mut text = text.to_owned();
        for (real, token) in &self.roots {
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

/// Any of these strings, as one pattern: a quick test before replacing each in turn.
fn literals<'a>(texts: impl IntoIterator<Item = &'a String>) -> Regex {
    let alternatives: Vec<String> = texts.into_iter().map(|t| regex::escape(t)).collect();
    Regex::new(&alternatives.join("|")).unwrap()
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

/// Replay one case under `under`; returns the first few differences. Every incremental names check the build
/// makes is appended to `verified` (#102).
fn case(
    name: &str,
    calls: &[Value],
    blobs: &Value,
    bin: &Path,
    under: &Path,
    verified: &Path,
    mut record: Option<&mut Recorder>,
) -> Vec<String> {
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
    let roots = Roots::new(
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
    if let Some(r) = record.as_deref_mut() {
        r.roots = roots.roots.iter().map(|(_, token)| token.clone()).collect();
    }
    for (i, call) in calls.iter().enumerate() {
        let at = format!("{name} #{i} {}", op(call));
        if let Some(r) = record.as_deref_mut() {
            r.observe(call, &roots, &mapping);
        }
        environment(call, blobs, &roots, &mapping);
        let sub = |text: &str, mapping: &HashMap<String, String>| roots.sub(&remap(text, mapping));
        let db = call.get("db").and_then(Value::as_str).map(|d| PathBuf::from(sub(d, &mapping)));
        let kept = record.as_deref_mut().map(|r| r.begin(call, &roots, &mapping, db.as_deref(), &counter));
        let mut raw_reply = String::new();
        let before: HashSet<String> =
            db.as_deref().map(|d| volatile(d, &masks)).unwrap_or_default().into_iter().collect();
        let outputs = if call["mode"] == "cli" {
            let args: Vec<String> =
                call["args"].as_array().unwrap().iter().map(|a| sub(a.as_str().unwrap(), &mapping)).collect();
            let mut child = Command::new(bin)
                .args(&args)
                .env("ILLUSTASH_TEST_KEY_COUNTER", &counter)
                .env(VERIFY_INCREMENTAL, verified)
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
                    .env(VERIFY_INCREMENTAL, verified)
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
            raw_reply.clone_from(&reply);
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
        if let (Some(r), Some(kept)) = (record.as_deref_mut(), kept) {
            r.finish(kept, call, &outputs, &raw_reply, db.as_deref(), &roots);
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
    let verified = std::env::temp_dir().join(format!("illustash-replay-{}-{module}.verified", std::process::id()));
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
                        found.extend(case(
                            name,
                            case_data["calls"].as_array().unwrap(),
                            &data["blobs"],
                            bin,
                            under,
                            &verified,
                            None,
                        ));
                    }
                })
            })
            .collect();
        workers.into_iter().flat_map(|w| w.join().unwrap()).collect()
    });
    let checks = verified_checks(&verified);
    let _ = fs::remove_file(&verified);
    eprintln!("{module}: incremental names checks {checks:?}");
    assert!(
        !VERIFIED_MODULES.contains(&module) || !checks.is_empty(),
        "{module}: the incremental names check never ran (built without illustash-core/verify-incremental?)"
    );
    assert!(
        !PROJECTED_MODULES.contains(&module) || checks.get("projection").is_some_and(|n| *n > 0),
        "{module}: the scoped display projection was never checked against a whole rebuild"
    );
    assert!(
        !STRUCTURE_MODULES.contains(&module) || checks.get("projection_structure").is_some_and(|n| *n > 0),
        "{module}: the scoped projection after identity or link changes was never checked against a whole rebuild"
    );
    assert!(
        problems.is_empty(),
        "{module}: {} of {} cases differ:\n{}",
        problems.len(),
        cases.len(),
        problems.join("\n")
    );
}

/// The core's incremental-vs-full check (#102), compiled into test builds only and switched on per process.
const VERIFY_INCREMENTAL: &str = "ILLUSTASH_TEST_VERIFY_INCREMENTAL";
/// Recordings whose commits take the incremental names path: the check must have run in each of them.
const VERIFIED_MODULES: [&str; 21] = [
    "test_terms_core",
    "test_terms_labels",
    "test_terms_artists",
    "test_terms_character_commands",
    "test_terms_character_history",
    "test_terms_character_identity",
    "test_terms_character_import",
    "test_terms_character_learning",
    "test_terms_character_refusals",
    "test_terms_character_structure",
    "test_terms_characters_contract",
    "test_terms_characters_cutover",
    "test_terms_golden",
    "test_terms_names_writes",
    "test_identity",
    "test_identity_hybrid",
    "test_lead",
    "test_web_v2",
    "test_works",
    "test_terms_artists_provenance",
    "test_web",
];
/// Recordings that commit name changes without changing identities or links: the scoped display projection (#103)
/// must have been checked against a whole rebuild in each of them.
const PROJECTED_MODULES: [&str; 18] = [
    "test_terms_labels",
    "test_terms_artists",
    "test_terms_character_commands",
    "test_terms_character_history",
    "test_terms_character_identity",
    "test_terms_character_learning",
    "test_terms_character_refusals",
    "test_terms_character_structure",
    "test_terms_characters_contract",
    "test_terms_characters_cutover",
    "test_terms_golden",
    "test_terms_names_writes",
    "test_identity",
    "test_lead",
    "test_web_v2",
    "test_works",
    "test_terms_artists_provenance",
    "test_web",
];

/// Recordings that commit identity or link changes: the scoped display projection on both sides of the change (#104)
/// must have been checked against a whole rebuild in each of them.
const STRUCTURE_MODULES: [&str; 19] = [
    "test_terms_labels",
    "test_terms_artists",
    "test_terms_character_history",
    "test_terms_character_identity",
    "test_terms_character_import",
    "test_terms_character_learning",
    "test_terms_character_refusals",
    "test_terms_character_structure",
    "test_terms_characters_contract",
    "test_terms_characters_cutover",
    "test_terms_golden",
    "test_terms_names_writes",
    "test_identity",
    "test_identity_hybrid",
    "test_lead",
    "test_web_v2",
    "test_works",
    "test_terms_artists_provenance",
    "test_web",
];

/// Passed checks by kind.
fn verified_checks(path: &Path) -> BTreeMap<String, usize> {
    let mut checks = BTreeMap::new();
    for line in fs::read_to_string(path).unwrap_or_default().lines() {
        *checks.entry(line.to_owned()).or_default() += 1;
    }
    checks
}

macro_rules! modules {
    ($($name:ident),* $(,)?) => {
        $(
            #[test]
            fn $name() {
                replay(stringify!($name));
            }
        )*
        /// Every recorded module, for `record_baseline`.
        const MODULES: &[&str] = &[$(stringify!($name)),*];
    };
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

// ---- Recording the new baseline (#201 step 1) ----
//
// Explicit only: `ILLUSTASH_RECORD_BASELINE=1 cargo test --test replay record_baseline -- --ignored`, on main
// while main still matches the recordings above (every case is checked against them as it is recorded). Writes
// `tests/fixtures/baseline/<module>.json.gz`, replayed by `tests/baseline.rs`; see `common/baseline.rs` for what
// is compared.

/// Commands that serve only the legacy tables: run while recording, left out of the baseline (#201).
const LEGACY_OPS: [&str; 9] = [
    "sync-artists",
    "sync-characters",
    "sync-names",
    "verify-characters",
    "verify-names",
    "cutover-artists",
    "cutover-labels",
    "cutover-characters",
    "cutover-names",
];

/// A kept call between `begin` and `finish`.
struct Pending {
    entry: serde_json::Map<String, Value>,
    before: baseline::State,
}

/// One case's baseline as it is recorded.
#[derive(Default)]
struct Recorder {
    roots: Vec<String>,
    calls: Vec<Value>,
    blobs: BTreeMap<String, String>,
    /// Paths whose state the next kept call must put in place (loaded, written or deleted by the recording's
    /// environment, or changed by a left-out call).
    dirty: std::collections::BTreeSet<PathBuf>,
    /// Copies from databases in the state the baseline accounts for, in order, with that state.
    copies: Vec<(PathBuf, PathBuf, Option<baseline::State>)>,
    counter_moved: bool,
    /// Learning stamps in replies, in order of appearance (inputs refer to them by position).
    stamps: Vec<String>,
    expected: HashMap<PathBuf, baseline::State>,
    left_out: usize,
    rebased: usize,
}

fn version_masks<'a>(roots: &'a dyn Fn(&str) -> String) -> baseline::Masks<'a> {
    baseline::Masks { roots, version: illustash_terms::SCHEMA_VERSION }
}

fn db_state(path: &Path, roots: &Roots) -> baseline::State {
    if !path.exists() {
        return baseline::State::new();
    }
    let norm = |t: &str| roots.norm(t);
    baseline::state(&open(path), &version_masks(&norm))
}

impl Recorder {
    fn blob(&mut self, text: String) -> String {
        let id = short(&text);
        self.blobs.insert(id.clone(), text);
        id
    }

    /// Notes the recording's file operations for `call`, before `environment` applies them.
    fn observe(&mut self, call: &Value, roots: &Roots, mapping: &HashMap<String, String>) {
        let Some(env) = call.get("env").and_then(Value::as_object) else { return };
        let sub = |text: &str| PathBuf::from(roots.sub(&remap(text, mapping)));
        for (key, value) in env {
            let path = sub(key);
            match value.get("copy").and_then(Value::as_str) {
                Some(from) if !self.dirty.contains(&sub(from)) => {
                    let from = sub(from);
                    let state =
                        self.expected.get(&from).cloned().or_else(|| from.exists().then(|| db_state(&from, roots)));
                    self.dirty.remove(&path);
                    self.copies.push((from, path, state));
                }
                _ => {
                    self.dirty.insert(path);
                }
            }
        }
    }

    /// A kept call: the environment it needs and its inputs; `None` for a call left out of the baseline.
    fn begin(
        &mut self,
        call: &Value,
        roots: &Roots,
        mapping: &HashMap<String, String>,
        db: Option<&Path>,
        counter: &Path,
    ) -> Option<Pending> {
        let left_out = LEGACY_OPS.contains(&op(call).as_str())
            || db.is_some_and(|d| baseline::is_database(d) && baseline::before_cutover(&open(d)));
        if left_out {
            self.left_out += 1;
            return None;
        }
        let tokens = |text: &str| baseline::scrub(&roots.norm(text));
        let mut env = Vec::new();
        for (from, to, state) in std::mem::take(&mut self.copies) {
            env.push(json!({"path": tokens(&to.to_string_lossy()), "copy": tokens(&from.to_string_lossy())}));
            match state {
                Some(state) => self.expected.insert(to, state),
                None => self.expected.remove(&to),
            };
        }
        for path in std::mem::take(&mut self.dirty) {
            let at = tokens(&path.to_string_lossy());
            if !path.exists() {
                env.push(json!({"path": at, "delete": true}));
                self.expected.remove(&path);
            } else if baseline::is_database(&path) {
                let c = open(&path);
                let dump = baseline::dump(&c, &tokens);
                let check = Connection::open_in_memory().unwrap();
                baseline::restore(&check, &roots.sub(&dump.tables), &roots.sub(&dump.rows), &roots.sub(&dump.objects));
                let as_dumped = |t: &str| roots.sub(&tokens(t));
                let (rebuilt, original) = (baseline::exact(&check, &|t| t.to_owned()), baseline::exact(&c, &as_dumped));
                if rebuilt != original {
                    let differ: Vec<String> = original
                        .iter()
                        .filter(|(k, v)| rebuilt.get(*k) != Some(*v))
                        .map(|(k, v)| {
                            format!(
                                "{k}: {:?} / rebuilt {:?}",
                                v.iter().take(3).collect::<Vec<_>>(),
                                rebuilt.get(k).map(|r| r.iter().take(3).collect::<Vec<_>>())
                            )
                        })
                        .chain(
                            rebuilt.keys().filter(|k| !original.contains_key(*k)).map(|k| format!("{k}: only rebuilt")),
                        )
                        .collect();
                    panic!("{at}: the dump does not rebuild the database: {differ:?}");
                }
                let terms = baseline::terms_version(&c);
                drop(c);
                let parts = [self.blob(dump.tables), self.blob(dump.rows), self.blob(dump.objects)];
                env.push(json!({"path": at, "db": parts, "terms": terms}));
                self.expected.insert(path.clone(), db_state(&path, roots));
            } else {
                let bytes = fs::read(&path).unwrap();
                let text = match String::from_utf8(bytes) {
                    Ok(text) => tokens(&text),
                    Err(e) => format!("x:{}", hex(e.as_bytes())),
                };
                let id = self.blob(text);
                env.push(json!({"path": at, "file": id}));
                self.expected.remove(&path);
            }
        }
        let mut entry = serde_json::Map::new();
        entry.insert("mode".into(), call["mode"].clone());
        if !env.is_empty() {
            entry.insert("env".into(), Value::Array(env));
        }
        if std::mem::take(&mut self.counter_moved) {
            let n: u64 = fs::read_to_string(counter).unwrap().trim().parse().unwrap();
            entry.insert("counter".into(), json!(n));
        }
        let actual = |text: &str| self.input(&baseline::scrub(&roots.norm(&roots.sub(&remap(text, mapping)))));
        if call["mode"] == "cli" {
            let args: Vec<Value> =
                call["args"].as_array().unwrap().iter().map(|a| json!(actual(a.as_str().unwrap()))).collect();
            entry.insert("args".into(), Value::Array(args));
            if let Some(stdin) = call["stdin"].as_str() {
                entry.insert("stdin".into(), json!(actual(stdin)));
            }
        } else {
            entry.insert("input".into(), serde_json::from_str(&actual(&call["input"].to_string())).unwrap());
        }
        if let Some(d) = db {
            entry.insert("db".into(), json!(tokens(&d.to_string_lossy())));
        }
        let before = match db {
            Some(d) => {
                let actual = db_state(d, roots);
                let want = self.expected.get(d).cloned().unwrap_or_default();
                if actual != want {
                    // Changed outside the calls the baseline keeps (another kept call's database, say): recorded.
                    entry.insert("before".into(), baseline::delta(&want, &actual));
                    self.rebased += 1;
                }
                actual
            }
            None => baseline::State::new(),
        };
        Some(Pending { entry, before })
    }

    /// Input text with learning stamps written as the reply they came from (`<stamp@N>`).
    fn input(&self, text: &str) -> String {
        STAMPS
            .replace_all(text, |c: &regex::Captures| {
                let at = self.stamps.iter().rposition(|s| *s == c[0]).unwrap_or_else(|| {
                    panic!("input stamp {} appears in no earlier kept reply", &c[0]);
                });
                format!("<stamp@{at}>")
            })
            .into_owned()
    }

    fn finish(
        &mut self,
        pending: Option<Pending>,
        call: &Value,
        outputs: &[(&str, Value)],
        raw_reply: &str,
        db: Option<&Path>,
        roots: &Roots,
    ) {
        let Some(Pending { mut entry, before }) = pending else {
            // Left out: whatever it may have written is put in place before the next kept call.
            self.counter_moved = true;
            if let Some(d) = db {
                self.dirty.insert(d.to_path_buf());
            }
            for arg in call.get("args").and_then(Value::as_array).into_iter().flatten() {
                let path = PathBuf::from(arg.as_str().unwrap());
                if path.is_absolute() && baseline::is_database(&path) {
                    self.dirty.insert(path);
                }
            }
            return;
        };
        let norm = |t: &str| roots.norm(t);
        let masks = version_masks(&norm);
        let mut seen = |text: &str| self.stamps.extend(STAMPS.find_iter(text).map(|m| m.as_str().to_owned()));
        if call["mode"] == "cli" {
            for (key, value) in outputs {
                match *key {
                    "code" => {
                        entry.insert("code".into(), value.clone());
                    }
                    key => {
                        let text = value.as_str().unwrap();
                        seen(text);
                        entry.insert(key.into(), json!(masks.output(text)));
                    }
                }
            }
        } else {
            seen(raw_reply);
            entry.insert("reply".into(), json!(masks.output(raw_reply.trim_end_matches(['\r', '\n']))));
        }
        if let Some(d) = db {
            let after = db_state(d, roots);
            let change = baseline::delta(&before, &after);
            if change.as_object().is_some_and(|o| !o.is_empty()) {
                entry.insert("change".into(), change);
            }
            self.expected.insert(d.to_path_buf(), after);
        }
        self.calls.push(Value::Object(entry));
    }
}

/// Records one module's baseline; panics if the recording above no longer matches (the baseline must be main's
/// behaviour while main still behaves as recorded).
fn record_module(module: &str, out: &Path) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("tests/fixtures/replay/{module}.json.gz"));
    let mut text = String::new();
    GzDecoder::new(fs::File::open(&path).unwrap()).read_to_string(&mut text).unwrap();
    let data: Value = serde_json::from_str(&text).unwrap();
    let bin = Path::new(env!("CARGO_BIN_EXE_illustash-terms"));
    let verified = std::env::temp_dir().join(format!("illustash-record-{}-{module}.verified", std::process::id()));
    let work: Vec<(&String, &Value)> = data["cases"].as_object().unwrap().iter().collect();
    let threads = std::thread::available_parallelism().map_or(2, |n| n.get().min(4));
    let next = std::sync::atomic::AtomicUsize::new(0);
    let results: Vec<(String, Recorder, Vec<String>)> = std::thread::scope(|s| {
        let workers: Vec<_> = (0..threads)
            .map(|_| {
                s.spawn(|| {
                    let mut found = Vec::new();
                    loop {
                        let i = next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        let Some((name, case_data)) = work.get(i) else { break found };
                        let mut r = Recorder::default();
                        let problems = case(
                            name,
                            case_data["calls"].as_array().unwrap(),
                            &data["blobs"],
                            bin,
                            &std::env::temp_dir(),
                            &verified,
                            Some(&mut r),
                        );
                        found.push(((*name).clone(), r, problems));
                    }
                })
            })
            .collect();
        workers.into_iter().flat_map(|w| w.join().unwrap()).collect()
    });
    let _ = fs::remove_file(&verified);
    let problems: Vec<&String> = results.iter().flat_map(|(_, _, p)| p).collect();
    assert!(problems.is_empty(), "{module}: main no longer matches the recordings, not recording:\n{problems:?}");
    let mut blobs = BTreeMap::new();
    let mut cases = serde_json::Map::new();
    let mut retired = Vec::new();
    let (mut kept, mut left_out, mut rebased) = (0, 0, 0);
    for (name, r, _) in results {
        left_out += r.left_out;
        rebased += r.rebased;
        if r.calls.is_empty() {
            retired.push(name);
            continue;
        }
        kept += r.calls.len();
        blobs.extend(r.blobs);
        cases.insert(name, json!({"roots": r.roots, "calls": r.calls}));
    }
    retired.sort();
    let summary = json!({"module": module, "cases": cases.len(), "kept_calls": kept, "left_out_calls": left_out,
        "rebased_calls": rebased, "retired_cases": retired});
    eprintln!("{summary}");
    let target = out.join(format!("{module}.json.gz"));
    if cases.is_empty() {
        // Nothing the core still serves once the legacy tables are gone: the module retires.
        let _ = fs::remove_file(&target);
        return summary;
    }
    let document = json!({
        "format": baseline::FORMAT,
        "module": module,
        "recorded": {"terms_version": illustash_terms::SCHEMA_VERSION, "commit": head()},
        "retired_cases": summary["retired_cases"],
        "left_out_calls": left_out,
        "blobs": blobs,
        "cases": cases,
    });
    let file = fs::File::create(target).unwrap();
    let mut gz = flate2::write::GzEncoder::new(file, flate2::Compression::best());
    gz.write_all(serde_json::to_string(&document).unwrap().as_bytes()).unwrap();
    gz.finish().unwrap();
    summary
}

fn head() -> String {
    let out = Command::new("git").args(["rev-parse", "HEAD"]).current_dir(env!("CARGO_MANIFEST_DIR")).output().unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

#[test]
#[ignore = "records the #201 baseline; explicit only"]
fn record_baseline() {
    assert!(std::env::var_os("ILLUSTASH_RECORD_BASELINE").is_some(), "set ILLUSTASH_RECORD_BASELINE to record");
    let out = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/baseline");
    fs::create_dir_all(&out).unwrap();
    let only = std::env::var("ILLUSTASH_RECORD_ONLY").ok();
    let mut summaries = Vec::new();
    for module in MODULES {
        if only.as_deref().is_some_and(|o| o != *module) {
            continue;
        }
        summaries.push(record_module(module, &out));
    }
    eprintln!("{}", serde_json::to_string_pretty(&summaries).unwrap());
}
