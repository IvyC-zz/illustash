//! The comparison baseline of the first public test release (#201 step 1): what the replayed
//! scenarios must keep producing once the legacy tables are gone (core v8).
//!
//! Recorded from main on core version 7 by the former `tests/replay.rs` (`record_baseline`) and `tests/contracts.rs`
//! (PR #239; both in git history), replayed by `baseline.rs` and `contracts.rs`. Comparison rules (agreed in #201;
//! the Python fixed expectations follow the same rules in `tests/fixed_expectations.py`):
//! - replies are compared as bytes after `Masks::output`: temporary roots, clocks, learning
//!   stamps, operating-system error text and the current core version;
//! - after every call, every table of the call's database except `RETIRED` is compared row for
//!   row (a sorted multiset of typed cells, `Masks::text` applied to text); integer primary keys
//!   are columns and compared like any other, implicit rowids are not;
//! - `meta` keeps its keys; `META_RULES` says how some values are compared;
//! - the database schema itself is not compared (v8 changes it; it has tests of its own).
#![allow(dead_code)] // shared by baseline.rs and contracts.rs; each uses part of it
use regex::Regex;
use rusqlite::{types::ValueRef, Connection};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::sync::LazyLock;

pub const FORMAT: i64 = 1;

/// The legacy tables core v8 retires: their content moves to `term_*` and is compared there.
pub const RETIRED: [&str; 7] =
    ["entities", "links", "work_review", "names", "display_names", "name_aliases", "identity_baseline"];

/// Columns left out: hashes over packaging details, as in the recordings before; and columns core v8 added, which
/// the baseline (recorded on core version 7) cannot hold (rule 5, each named with the reason):
/// - `actions.label_kind` / `label_count`: a counted step's kind and count (#201); what the history shows from them
///   is compared in the replies.
/// - `term_names.seq`: the order of names, filled from the former `names` rowids (#201); the orders the readers take
///   from it are compared in the replies.
pub const SKIP_COLUMNS: [(&str, &str); 5] = [
    ("term_packages", "manifest"),
    ("term_evidence", "fingerprint"),
    ("actions", "label_kind"),
    ("actions", "label_count"),
    ("term_names", "seq"),
];

static CLOCK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\d{4}-\d{2}-\d{2}[ T]\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})?").unwrap());
static STAMP: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"learning-v1:[0-9a-f]{64}").unwrap());
/// Machine paths the Python-era recordings carried in their inputs (a temporary directory of the recording machine,
/// a home directory in a script's first line): not run-specific, but not for a public repository either (#201).
static MACHINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"/var/folders/[^/\s'\x22]+/[^/\s'\x22]+/T/|/Users/[^/\s'\x22]+/").unwrap());

/// Recorded text with machine paths written generically; applied to everything recorded and compared.
pub fn scrub(text: &str) -> String {
    MACHINE
        .replace_all(
            text,
            |c: &regex::Captures| if c[0].starts_with("/var/") { "/tmp/recording/" } else { "/home/user/" },
        )
        .into_owned()
}

/// The operating system's own error text and number (macOS and Windows word and number it differently).
static OS_ERROR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[^：:\n]*\(os error \d+\)").unwrap());

/// How replies and cells are masked: run-specific spellings first (temporary roots), then the rules above.
pub struct Masks<'a> {
    /// Real temporary paths -> `@ROOT@` tokens.
    pub roots: &'a dyn Fn(&str) -> String,
    /// The core version of the build: replies that name it are compared as "the current version".
    pub version: i64,
}

impl Masks<'_> {
    pub fn text(&self, text: &str) -> String {
        let text = scrub(&(self.roots)(text));
        let text = CLOCK.replace_all(&text, "<clock>");
        STAMP.replace_all(&text, "<stamp>").into_owned()
    }

    /// A reply as compared: `text`, operating-system error text, and the current core version where a reply
    /// states it (`migrate`'s `from`/`to`, the version refusals, upgrade backup names).
    pub fn output(&self, text: &str) -> String {
        let text = OS_ERROR.replace_all(&self.text(text), "<os error>").into_owned();
        let v = self.version;
        let numbers = Regex::new(&format!(r#""(from|to)":{v}([,}}])"#)).unwrap();
        let text = numbers.replace_all(&text, r#""$1":"<terms-version>"$2"#);
        text.replace(&format!("程序是第 {v} 版"), "程序是第 <terms-version> 版")
            .replace(&format!("程序支持的第 {v} 版"), "程序支持的第 <terms-version> 版")
            .replace(&format!("-before-v{v}-"), "-before-v<terms-version>-")
    }
}

/// `meta` values compared by what they mean rather than as written.
/// - `terms_schema`: the current version is `<terms-version>` (7 when recorded, 8 in core v8);
/// - `names_version` counts writes to the tables its triggers sit on, which v8 moves: only whether the projections
///   are up to date with it is compared;
/// - `identity_graph_version` hashes run-specific values, as left out before.
fn meta_value(key: &str, value: &str, names_version: Option<&str>, version: i64) -> String {
    match key {
        "terms_schema" if value == version.to_string() => "<terms-version>".into(),
        "names_version" => "<names_version>".into(),
        "names_projected" | "names_display_projected" if Some(value) == names_version => "<names_version>".into(),
        "names_projected" | "names_display_projected" => "<stale>".into(),
        "identity_graph_version" => "<derived>".into(),
        _ => value.to_owned(),
    }
}

/// One table as compared: its columns and its rows (canonical JSON text of the masked cells) with multiplicity.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Table {
    pub columns: Vec<String>,
    pub rows: BTreeMap<String, usize>,
}

pub type State = BTreeMap<String, Table>;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn cell(value: ValueRef, masks: &Masks) -> Value {
    match value {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(v) => json!(v),
        ValueRef::Real(v) => json!({"real": format!("{:016x}", v.to_bits())}),
        ValueRef::Text(v) => json!(masks.text(&String::from_utf8_lossy(v))),
        ValueRef::Blob(v) => json!({"blob": hex(v)}),
    }
}

pub fn tables(c: &Connection) -> Vec<String> {
    let mut s = c
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
        .unwrap();
    s.query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect()
}

/// The compared content of a database.
pub fn state(c: &Connection, masks: &Masks) -> State {
    let names_version: Option<String> = c
        .query_row(
            "SELECT value FROM meta WHERE key='names_version' AND EXISTS(SELECT 1 FROM sqlite_master WHERE name='meta')",
            [],
            |r| r.get(0),
        )
        .ok();
    let mut out = State::new();
    for name in tables(c) {
        if RETIRED.contains(&name.as_str()) {
            continue;
        }
        let all: Vec<String> = {
            let mut s = c.prepare(&format!("PRAGMA table_info(\"{name}\")")).unwrap();
            s.query_map([], |r| r.get::<_, String>(1)).unwrap().map(Result::unwrap).collect()
        };
        let keep: Vec<usize> =
            (0..all.len()).filter(|&i| !SKIP_COLUMNS.contains(&(name.as_str(), all[i].as_str()))).collect();
        let mut table = Table { columns: keep.iter().map(|&i| all[i].clone()).collect(), rows: BTreeMap::new() };
        let mut s = c.prepare(&format!("SELECT * FROM \"{name}\"")).unwrap();
        let mut q = s.query([]).unwrap();
        while let Some(row) = q.next().unwrap() {
            let mut cells: Vec<Value> = keep.iter().map(|&i| cell(row.get_ref(i).unwrap(), masks)).collect();
            if name == "meta" {
                if let (Some(Value::String(key)), Some(Value::String(value))) = (cells.first(), cells.get(1)) {
                    cells[1] = json!(meta_value(key, value, names_version.as_deref(), masks.version));
                }
            }
            *table.rows.entry(Value::Array(cells).to_string()).or_default() += 1;
        }
        out.insert(name, table);
    }
    out
}

/// What a call changed: per table, rows removed and added (with repeats), columns when the table is new or
/// its columns changed; tables that disappeared.
pub fn delta(before: &State, after: &State) -> Value {
    let mut tables = Map::new();
    for (name, table) in after {
        let old = before.get(name);
        let mut entry = Map::new();
        if old.map(|t| &t.columns) != Some(&table.columns) {
            entry.insert("columns".into(), json!(table.columns));
        }
        let (removed, added) = match old {
            Some(t) if t.columns == table.columns => difference(&t.rows, &table.rows),
            // Columns changed: everything that was there goes, everything there now comes.
            Some(t) => (expand(&t.rows), expand(&table.rows)),
            None => (Vec::new(), expand(&table.rows)),
        };
        if !removed.is_empty() {
            entry.insert("-".into(), json!(removed));
        }
        if !added.is_empty() {
            entry.insert("+".into(), json!(added));
        }
        if !entry.is_empty() {
            tables.insert(name.clone(), Value::Object(entry));
        }
    }
    let gone: Vec<&String> = before.keys().filter(|k| !after.contains_key(*k)).collect();
    let mut out = Map::new();
    if !tables.is_empty() {
        out.insert("tables".into(), Value::Object(tables));
    }
    if !gone.is_empty() {
        out.insert("gone".into(), json!(gone));
    }
    Value::Object(out)
}

fn expand(rows: &BTreeMap<String, usize>) -> Vec<String> {
    rows.iter().flat_map(|(r, n)| std::iter::repeat_n(r.clone(), *n)).collect()
}

/// Rows only in `old`, rows only in `new` (multiset difference), each in canonical order.
fn difference(old: &BTreeMap<String, usize>, new: &BTreeMap<String, usize>) -> (Vec<String>, Vec<String>) {
    let mut removed = Vec::new();
    let mut added = Vec::new();
    for (row, &n) in old {
        let m = new.get(row).copied().unwrap_or(0);
        removed.extend(std::iter::repeat_n(row.clone(), n.saturating_sub(m)));
    }
    for (row, &n) in new {
        let m = old.get(row).copied().unwrap_or(0);
        added.extend(std::iter::repeat_n(row.clone(), n.saturating_sub(m)));
    }
    (removed, added)
}

/// `before` with a recorded `delta` applied.
pub fn apply(before: &State, delta: &Value) -> Result<State, String> {
    let mut out = before.clone();
    if let Some(gone) = delta.get("gone").and_then(Value::as_array) {
        for name in gone {
            out.remove(name.as_str().unwrap());
        }
    }
    if let Some(tables) = delta.get("tables").and_then(Value::as_object) {
        for (name, change) in tables {
            let table = out.entry(name.clone()).or_default();
            if let Some(columns) = change.get("columns") {
                table.columns = columns.as_array().unwrap().iter().map(|c| c.as_str().unwrap().to_owned()).collect();
            }
            for row in change.get("-").and_then(Value::as_array).into_iter().flatten() {
                let row = row.as_str().unwrap();
                match table.rows.get_mut(row) {
                    Some(n) if *n > 1 => *n -= 1,
                    Some(_) => {
                        table.rows.remove(row);
                    }
                    None => return Err(format!("{name}: recorded removal of a row that is not there: {row}")),
                }
            }
            for row in change.get("+").and_then(Value::as_array).into_iter().flatten() {
                *table.rows.entry(row.as_str().unwrap().to_owned()).or_default() += 1;
            }
        }
    }
    Ok(out)
}

/// The first differences between an expected and an actual state, for a failure message.
pub fn differences(want: &State, got: &State, limit: usize) -> Vec<String> {
    let mut out = Vec::new();
    for name in want.keys().chain(got.keys().filter(|k| !want.contains_key(*k))) {
        let (w, g) = (want.get(name), got.get(name));
        match (w, g) {
            (Some(_), None) => out.push(format!("{name}: table missing")),
            (None, Some(_)) => out.push(format!("{name}: table not in the baseline")),
            (Some(w), Some(g)) if w.columns != g.columns => {
                out.push(format!("{name}: columns {:?}, baseline {:?}", g.columns, w.columns))
            }
            (Some(w), Some(g)) => {
                let (missing, extra) = difference(&w.rows, &g.rows);
                for row in missing.iter().take(limit) {
                    out.push(format!("{name}: missing {row}"));
                }
                for row in extra.iter().take(limit) {
                    out.push(format!("{name}: extra {row}"));
                }
            }
            (None, None) => unreachable!(),
        }
    }
    out
}

/// A database as SQL that rebuilds it with the same rowids, in three parts kept apart so that recordings share
/// the schema: tables; rows (explicit rowids where a table has implicit ones, and `sqlite_sequence`); then
/// indexes, views and triggers in schema order, so write-guard triggers do not see the restore. Text goes through
/// `text` (temporary roots to tokens).
pub struct Dump {
    pub tables: String,
    pub rows: String,
    pub objects: String,
}

pub fn dump(c: &Connection, text: &dyn Fn(&str) -> String) -> Dump {
    let mut out = Dump { tables: String::new(), rows: String::new(), objects: String::new() };
    let user_version: i64 = c.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
    if user_version != 0 {
        out.tables.push_str(&format!("PRAGMA user_version={user_version};\n"));
    }
    let schema: Vec<(String, String, Option<String>)> = c
        .prepare("SELECT type,name,sql FROM sqlite_master WHERE name NOT LIKE 'sqlite_autoindex_%' ORDER BY rowid")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    for (kind, name, sql) in &schema {
        let Some(sql) = sql else { continue };
        let part = if kind != "table" {
            &mut out.objects
        } else if name != "sqlite_sequence" {
            &mut out.tables
        } else {
            continue;
        };
        part.push_str(&text(sql));
        part.push_str(";\n");
    }
    for (kind, name, sql) in &schema {
        if kind != "table" {
            continue;
        }
        let sql = sql.as_deref().unwrap_or_default().to_uppercase();
        let implicit = !sql.contains("WITHOUT ROWID") && !integer_key(c, name) && name != "sqlite_sequence";
        let select = if implicit { "SELECT rowid,* FROM" } else { "SELECT * FROM" };
        let mut s = c.prepare(&format!("{select} \"{name}\"")).unwrap();
        let columns: Vec<String> = s.column_names().iter().map(|n| format!("\"{}\"", n.replace('"', "\"\""))).collect();
        let n = columns.len();
        let mut q = s.query([]).unwrap();
        while let Some(row) = q.next().unwrap() {
            let values: Vec<String> = (0..n).map(|i| literal(row.get_ref(i).unwrap(), text)).collect();
            if name == "sqlite_sequence" {
                out.rows.push_str(&format!(
                    "DELETE FROM sqlite_sequence WHERE name={0}; INSERT INTO sqlite_sequence(name,seq) VALUES ({0},{1});\n",
                    values[0], values[1]
                ));
            } else {
                out.rows.push_str(&format!(
                    "INSERT INTO \"{name}\"({}) VALUES ({});\n",
                    columns.join(","),
                    values.join(",")
                ));
            }
        }
    }
    out
}

fn integer_key(c: &Connection, table: &str) -> bool {
    // A rowid alias: exactly one primary-key column, declared INTEGER.
    let keys: Vec<String> = c
        .prepare(&format!("SELECT type FROM pragma_table_info('{}') WHERE pk>0", table.replace('\'', "''")))
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    keys.len() == 1 && keys[0].eq_ignore_ascii_case("INTEGER")
}

fn literal(value: ValueRef, text: &dyn Fn(&str) -> String) -> String {
    match value {
        ValueRef::Null => "NULL".into(),
        ValueRef::Integer(v) => v.to_string(),
        ValueRef::Real(v) => real_literal(v),
        ValueRef::Text(v) => format!("'{}'", text(&String::from_utf8_lossy(v)).replace('\'', "''")),
        ValueRef::Blob(v) => format!("x'{}'", hex(v)),
    }
}

/// A REAL that SQLite reads back to the same bits: Rust's shortest round-trip form, which SQLite parses exactly.
fn real_literal(v: f64) -> String {
    if v.is_nan() {
        "NULL".into()
    } else if v.is_infinite() {
        if v > 0.0 {
            "9e999".into()
        } else {
            "-9e999".into()
        }
    } else {
        let s = format!("{v:?}");
        if s.contains('.') || s.contains('e') || s.contains("inf") {
            s
        } else {
            format!("{s}.0")
        }
    }
}

/// Every table and schema object exactly (typed cells, rowids), text through `text`: to check that `dump`
/// rebuilds a database (as `dump` wrote its text).
pub fn exact(c: &Connection, text: &dyn Fn(&str) -> String) -> BTreeMap<String, Vec<String>> {
    let mut out = BTreeMap::new();
    let mut s = c
        .prepare(
            "SELECT type,name,tbl_name,coalesce(sql,'') FROM sqlite_master WHERE name NOT LIKE 'sqlite_autoindex_%'",
        )
        .unwrap();
    let mut schema: Vec<String> = s
        .query_map([], |r| {
            Ok(format!(
                "{}|{}|{}|{}",
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                text(&r.get::<_, String>(3)?)
            ))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect();
    schema.sort();
    out.insert("@schema".into(), schema);
    let tables: Vec<(String, String)> = c
        .prepare("SELECT name,coalesce(sql,'') FROM sqlite_master WHERE type='table'")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    for (name, sql) in tables {
        // `sqlite_sequence` rows are restored in the order SQLite first needs them: their rowids carry nothing.
        let plain = sql.to_uppercase().contains("WITHOUT ROWID") || name == "sqlite_sequence";
        let select = if plain { "*" } else { "rowid,*" };
        let mut s = c.prepare(&format!("SELECT {select} FROM \"{name}\"")).unwrap();
        let n = s.column_count();
        let mut q = s.query([]).unwrap();
        let mut rows = Vec::new();
        while let Some(row) = q.next().unwrap() {
            rows.push(
                (0..n)
                    .map(|i| match row.get_ref(i).unwrap() {
                        ValueRef::Null => "n".to_owned(),
                        ValueRef::Integer(v) => format!("i{v}"),
                        ValueRef::Real(v) => format!("r{:016x}", v.to_bits()),
                        ValueRef::Text(v) => format!("t{}", hex(text(&String::from_utf8_lossy(v)).as_bytes())),
                        ValueRef::Blob(v) => format!("b{}", hex(v)),
                    })
                    .collect::<Vec<_>>()
                    .join(","),
            );
        }
        rows.sort();
        out.insert(name, rows);
    }
    out.insert(
        "@user_version".into(),
        vec![c.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0)).unwrap().to_string()],
    );
    out
}

/// Rebuild a database from `dump` output (foreign keys off while the rows go in, as the recordings were restored).
pub fn restore(c: &Connection, tables: &str, rows: &str, objects: &str) {
    c.execute_batch("PRAGMA foreign_keys=OFF").unwrap();
    c.execute_batch(&format!("BEGIN;\n{tables}{rows}{objects}COMMIT;")).unwrap();
}

/// Whether a file is an SQLite database (by its header).
pub fn is_database(path: &std::path::Path) -> bool {
    let mut head = [0u8; 16];
    std::fs::File::open(path).and_then(|mut f| std::io::Read::read_exact(&mut f, &mut head)).is_ok()
        && &head == b"SQLite format 3\0"
}

/// The core version a database is at (0: no terms tables).
pub fn terms_version(c: &Connection) -> i64 {
    c.query_row("SELECT CAST(value AS INTEGER) FROM meta WHERE key='terms_schema'", [], |r| r.get(0)).unwrap_or(0)
}

/// A library some domain of which has not been switched to the core yet: calls on it build a scenario up rather than
/// exercise the switched core, and become part of a checkpoint.
pub fn before_cutover(c: &Connection) -> bool {
    let has_entities: bool = c
        .query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='entities')", [], |r| {
            r.get(0)
        })
        .unwrap_or(false);
    if !has_entities {
        return false;
    }
    let switched: i64 = c
        .query_row(
            "SELECT count(*) FROM meta WHERE key IN ('terms_artists','terms_labels','terms_characters','terms_names') AND value='core'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    switched != 4
}
