//! Minimal writes for replacing a machine cache table (#31, docs/ARCHITECTURE_DIRECTION.md).
//!
//! Research and provenance used to publish a cache by deleting the whole table and inserting every
//! row again, so one changed row cost a rewrite of the table. [`plan`] finds the longest prefix of the
//! desired rows that the current table already holds in the same rowid order (matched by key); those
//! rows stay where they are, every other current row is deleted and the remaining desired rows are
//! appended in order. The table then holds exactly the desired rows in the desired rowid order, which
//! is what the full rewrite left; only the rowid numbers are no longer renumbered from 1.
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
use crate::{Error, Result};
use rusqlite::{
    functions::FunctionFlags,
    types::{Value as Sql, ValueRef},
    Connection,
};
use sha2::{Digest, Sha256};
use std::{collections::HashMap, hash::Hash};

/// Which current rows stay and which go.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Plan {
    /// For desired row `j < kept.len()`, the index of the current row that stays in its place.
    pub kept: Vec<usize>,
    /// Current rows that are deleted, ascending. Desired rows from `kept.len()` on are appended.
    pub delete: Vec<usize>,
}

/// `same` decides whether a key-matched current row may stay for the desired row (rewritten in place
/// by the caller if its columns differ); the first desired row that cannot stay ends the kept prefix.
pub(crate) fn plan<R, K: Eq + Hash>(
    current: &[R],
    desired: &[R],
    key: impl Fn(&R) -> K,
    same: impl Fn(&R, &R) -> bool,
) -> Plan {
    let index: HashMap<K, usize> = current.iter().enumerate().map(|(i, r)| (key(r), i)).collect();
    let mut kept = vec![];
    for row in desired {
        match index.get(&key(row)) {
            Some(&i) if kept.last().is_none_or(|l| i > *l) && same(&current[i], row) => kept.push(i),
            _ => break,
        }
    }
    let mut stays = vec![false; current.len()];
    for &i in &kept {
        stays[i] = true;
    }
    Plan { kept, delete: (0..current.len()).filter(|i| !stays[*i]).collect() }
}

/// One stored value, hashable; reals compare by their bits.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Cell {
    Null,
    Int(i64),
    Real(u64),
    Text(Vec<u8>),
    Blob(Vec<u8>),
}
impl From<ValueRef<'_>> for Cell {
    fn from(v: ValueRef<'_>) -> Self {
        match v {
            ValueRef::Null => Self::Null,
            ValueRef::Integer(i) => Self::Int(i),
            ValueRef::Real(f) => Self::Real(f.to_bits()),
            ValueRef::Text(t) => Self::Text(t.to_vec()),
            ValueRef::Blob(b) => Self::Blob(b.to_vec()),
        }
    }
}
impl From<Sql> for Cell {
    fn from(v: Sql) -> Self {
        match v {
            Sql::Null => Self::Null,
            Sql::Integer(i) => Self::Int(i),
            Sql::Real(f) => Self::Real(f.to_bits()),
            Sql::Text(t) => Self::Text(t.into_bytes()),
            Sql::Blob(b) => Self::Blob(b),
        }
    }
}
impl rusqlite::ToSql for Cell {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        Ok(rusqlite::types::ToSqlOutput::Borrowed(match self {
            Self::Null => ValueRef::Null,
            Self::Int(i) => ValueRef::Integer(*i),
            Self::Real(f) => ValueRef::Real(f64::from_bits(*f)),
            Self::Text(t) => ValueRef::Text(t),
            Self::Blob(b) => ValueRef::Blob(b),
        }))
    }
}
fn feed(h: &mut Sha256, tag: &[u8], bytes: &[u8]) {
    h.update(tag);
    h.update((bytes.len() as u64).to_le_bytes());
    h.update(bytes);
}
fn digest_cells<'a>(cells: impl Iterator<Item = ValueRef<'a>>) -> [u8; 32] {
    let mut h = Sha256::new();
    for v in cells {
        match v {
            ValueRef::Null => feed(&mut h, b"n", &[]),
            ValueRef::Integer(i) => feed(&mut h, b"i", &i.to_le_bytes()),
            ValueRef::Real(f) => feed(&mut h, b"r", &f.to_bits().to_le_bytes()),
            ValueRef::Text(t) => feed(&mut h, b"t", t),
            ValueRef::Blob(b) => feed(&mut h, b"b", b),
        }
    }
    h.finalize().into()
}
fn digest(row: &[Cell]) -> [u8; 32] {
    digest_cells(row.iter().map(|c| match c {
        Cell::Null => ValueRef::Null,
        Cell::Int(i) => ValueRef::Integer(*i),
        Cell::Real(f) => ValueRef::Real(f64::from_bits(*f)),
        Cell::Text(t) => ValueRef::Text(t),
        Cell::Blob(b) => ValueRef::Blob(b),
    }))
}
const DIGEST: &str = "illustash_row_digest";
/// Rows are compared by a digest of their typed cells computed inside SQLite, so a large table (BLOB
/// signatures, cached bodies) is never held in memory twice.
fn register(c: &Connection) -> Result<()> {
    c.create_scalar_function(DIGEST, -1, FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC, |ctx| {
        Ok(digest_cells((0..ctx.len()).map(|i| ctx.get_raw(i))).to_vec())
    })?;
    Ok(())
}
fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Columns of one table, its primary key and the affinity SQLite applies when storing.
pub(crate) struct Shape {
    schema: String,
    table: String,
    columns: Vec<String>,
    /// Positions of the primary-key columns in key order; the whole row when the table has none.
    key: Vec<usize>,
    keyed: bool,
    affinity: Vec<Affinity>,
    declared: Vec<String>,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Affinity {
    Integer,
    Real,
    Other,
}
impl Shape {
    pub fn of(c: &Connection, schema: &str, table: &str) -> Result<Self> {
        let mut columns = vec![];
        let mut pk = vec![];
        let mut affinity = vec![];
        let mut types = vec![];
        let mut q = c.prepare("SELECT name,type,pk FROM pragma_table_info(?1,?2) ORDER BY cid")?;
        let mut rows = q.query([table, schema])?;
        while let Some(r) = rows.next()? {
            let declared = r.get::<_, String>(1)?.to_ascii_uppercase();
            let n: i64 = r.get(2)?;
            if n > 0 {
                pk.push((n, columns.len()));
            }
            columns.push(r.get::<_, String>(0)?);
            types.push(declared.clone());
            // SQLite's affinity rules, the parts that change a stored number.
            affinity.push(if declared.contains("INT") {
                Affinity::Integer
            } else if ["CHAR", "CLOB", "TEXT", "BLOB"].iter().any(|t| declared.contains(t)) || declared.is_empty() {
                Affinity::Other
            } else if ["REAL", "FLOA", "DOUB"].iter().any(|t| declared.contains(t)) {
                Affinity::Real
            } else {
                Affinity::Integer // NUMERIC stores a lossless real as an integer, like INTEGER.
            });
        }
        if columns.is_empty() {
            return Err(Error::Invalid(format!("缓存表不存在：{schema}.{table}")));
        }
        pk.sort();
        let keyed = !pk.is_empty();
        let key = if keyed { pk.into_iter().map(|(_, i)| i).collect() } else { (0..columns.len()).collect() };
        Ok(Self { schema: schema.into(), table: table.into(), columns, key, keyed, affinity, declared: types })
    }
    pub fn width(&self) -> usize {
        self.columns.len()
    }
    fn name(&self) -> String {
        format!("{}.{}", quote(&self.schema), quote(&self.table))
    }
    /// The value SQLite would store for `cell` in column `i`, so a row compares equal to its stored form.
    fn stored(&self, i: usize, cell: Cell) -> Cell {
        match (self.affinity[i], cell) {
            (Affinity::Real, Cell::Int(v)) => Cell::Real((v as f64).to_bits()),
            (Affinity::Integer, Cell::Real(bits)) => {
                let f = f64::from_bits(bits);
                if f.fract() == 0.0 && (-9.2e18..=9.2e18).contains(&f) && (f as i64) as f64 == f {
                    Cell::Int(f as i64)
                } else {
                    Cell::Real(bits)
                }
            }
            (_, cell) => cell,
        }
    }
    /// An `INTEGER PRIMARY KEY` column is the rowid itself.
    fn alias(&self) -> bool {
        self.keyed && self.key.len() == 1 && self.declared[self.key[0]] == "INTEGER"
    }
    fn select_list(&self) -> String {
        self.columns.iter().map(|c| quote(c)).collect::<Vec<_>>().join(",")
    }
    fn key_where(&self) -> String {
        self.key
            .iter()
            .enumerate()
            .map(|(n, &i)| format!("{}=?{}", quote(&self.columns[i]), n + 1))
            .collect::<Vec<_>>()
            .join(" AND ")
    }
}

/// Rows written by one table publication; `old` holds the previous version of each deleted or rewritten
/// row and `touched` the rowids of rows inserted or rewritten, for the caller's checks.
#[derive(Debug, Default)]
pub(crate) struct Written {
    pub deleted: usize,
    pub updated: usize,
    pub inserted: usize,
    pub old: Vec<Vec<Cell>>,
    pub touched: Vec<i64>,
}
impl Written {
    pub fn rows(&self) -> usize {
        self.deleted + self.updated + self.inserted
    }
}

struct Entry {
    rowid: i64,
    key: Vec<Cell>,
    digest: [u8; 32],
}
fn entries(c: &Connection, shape: &Shape, check: &dyn Fn() -> Result<()>) -> Result<Vec<Entry>> {
    let keys = shape.key.iter().map(|&i| quote(&shape.columns[i])).collect::<Vec<_>>().join(",");
    let mut q = c.prepare(&format!(
        "SELECT rowid,{DIGEST}({}),{keys} FROM {} ORDER BY rowid",
        shape.select_list(),
        shape.name()
    ))?;
    let mut rows = q.query([])?;
    let mut out = vec![];
    while let Some(r) = rows.next()? {
        check()?;
        let digest: Vec<u8> = r.get(1)?;
        out.push(Entry {
            rowid: r.get(0)?,
            digest: digest.try_into().map_err(|_| Error::Invalid("行摘要长度不对".into()))?,
            key: (0..shape.key.len()).map(|i| r.get_ref(i + 2).map(Cell::from)).collect::<rusqlite::Result<_>>()?,
        });
    }
    Ok(out)
}
fn capture(c: &Connection, shape: &Shape, rowid: i64) -> Result<Vec<Cell>> {
    Ok(c.query_row(&format!("SELECT {} FROM {} WHERE rowid=?", shape.select_list(), shape.name()), [rowid], |r| {
        (0..shape.width()).map(|i| r.get_ref(i).map(Cell::from)).collect()
    })?)
}

/// Make `main.table` hold `rows` in this rowid order, writing only rows that differ.
/// `keep_old`: also return the previous version of rows that are deleted or rewritten.
pub(crate) fn sync(
    c: &Connection,
    table: &str,
    rows: Vec<Vec<Cell>>,
    keep_old: bool,
    check: &dyn Fn() -> Result<()>,
) -> Result<Written> {
    register(c)?;
    let shape = Shape::of(c, "main", table)?;
    let current = entries(c, &shape, check)?;
    let mut stored = Vec::with_capacity(rows.len());
    let mut wanted = Vec::with_capacity(rows.len());
    for row in rows {
        if row.len() != shape.width() {
            return Err(Error::Invalid(format!("缓存行列数不对：{table}")));
        }
        let row: Vec<Cell> = row.into_iter().enumerate().map(|(i, v)| shape.stored(i, v)).collect();
        wanted.push(Entry { rowid: 0, key: shape.key.iter().map(|&i| row[i].clone()).collect(), digest: digest(&row) });
        stored.push(row);
    }
    let plan = plan(&current, &wanted, |e| e.key.clone(), |_, _| true);
    let mut out = Written::default();
    let name = shape.name();
    let mut delete = c.prepare(&format!("DELETE FROM {name} WHERE rowid=?"))?;
    for &i in &plan.delete {
        check()?;
        if keep_old {
            out.old.push(capture(c, &shape, current[i].rowid)?);
        }
        delete.execute([current[i].rowid])?;
        out.deleted += 1;
    }
    let width = shape.width();
    let assign = shape.columns.iter().enumerate().map(|(i, c)| format!("{}=?{}", quote(c), i + 1));
    let mut update =
        c.prepare(&format!("UPDATE {name} SET {} WHERE rowid=?{}", assign.collect::<Vec<_>>().join(","), width + 1))?;
    for (j, &i) in plan.kept.iter().enumerate() {
        if current[i].digest == wanted[j].digest {
            continue;
        }
        check()?;
        if keep_old {
            out.old.push(capture(c, &shape, current[i].rowid)?);
        }
        update.execute(rusqlite::params_from_iter(stored[j].iter().chain([&Cell::Int(current[i].rowid)])))?;
        out.updated += 1;
        out.touched.push(current[i].rowid);
    }
    let mut insert = c.prepare(&format!("INSERT INTO {name} VALUES ({})", vec!["?"; width].join(",")))?;
    for row in &stored[plan.kept.len()..] {
        check()?;
        insert.execute(rusqlite::params_from_iter(row.iter()))?;
        out.inserted += 1;
        out.touched.push(c.last_insert_rowid());
    }
    Ok(out)
}

/// Make `main.table` equal to the same table of the attached database `source`, rowids included: rows whose
/// rowid and typed values match stay, every other row is deleted and the source's row inserted under its rowid.
/// The comparison runs inside SQLite (EXCEPT over rowid, binary-collated values and storage classes), so neither
/// table is loaded into memory. The source must not change meanwhile.
pub(crate) fn mirror(c: &Connection, table: &str, source: &str) -> Result<Written> {
    let shape = Shape::of(c, "main", table)?;
    let from = Shape::of(c, source, table)?;
    if from.columns != shape.columns {
        return Err(Error::Invalid(format!("暂存缓存表结构不同：{table}")));
    }
    let typed = shape
        .columns
        .iter()
        .map(|col| format!("{0} COLLATE BINARY,typeof({0})", quote(col)))
        .collect::<Vec<_>>()
        .join(",");
    let differing = |a: &Shape, b: &Shape| -> Result<Vec<i64>> {
        let sql = format!(
            "SELECT r FROM (SELECT rowid AS r,{typed} FROM {} EXCEPT SELECT rowid,{typed} FROM {})",
            a.name(),
            b.name()
        );
        Ok(c.prepare(&sql)?.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?)
    };
    let gone = differing(&shape, &from)?;
    let incoming = differing(&from, &shape)?;
    let mut out = Written::default();
    let mut delete = c.prepare(&format!("DELETE FROM {} WHERE rowid=?", shape.name()))?;
    for rowid in gone {
        out.deleted += delete.execute([rowid])?;
    }
    // A rowid alias column carries the rowid by itself.
    let list = shape.select_list();
    let target = if shape.alias() { list.clone() } else { format!("rowid,{list}") };
    let mut insert = c.prepare(&format!(
        "INSERT INTO {}({target}) SELECT {target} FROM {} WHERE rowid=?",
        shape.name(),
        from.name()
    ))?;
    for rowid in incoming {
        out.inserted += insert.execute([rowid])?;
        out.touched.push(rowid);
    }
    Ok(out)
}

/// Apply a change list computed against the same table contents (research worker deltas): delete the
/// rows with the given primary keys, each of which must exist, then append `append` in order.
pub(crate) fn apply(
    c: &Connection,
    table: &str,
    delete: &[Vec<Cell>],
    append: Vec<Vec<Cell>>,
    keep_old: bool,
    check: &dyn Fn() -> Result<()>,
) -> Result<Written> {
    let shape = Shape::of(c, "main", table)?;
    if !shape.keyed {
        return Err(Error::Invalid(format!("缓存表没有主键：{table}")));
    }
    let name = shape.name();
    let mut out = Written::default();
    let returning = if keep_old { shape.select_list() } else { "rowid".into() };
    let mut remove = c.prepare(&format!("DELETE FROM {name} WHERE {} RETURNING {returning}", shape.key_where()))?;
    for key in delete {
        check()?;
        if key.len() != shape.key.len() {
            return Err(Error::Invalid(format!("缓存键列数不对：{table}")));
        }
        let key: Vec<Cell> = key.iter().zip(&shape.key).map(|(v, &i)| shape.stored(i, v.clone())).collect();
        let mut gone = remove.query(rusqlite::params_from_iter(key.iter()))?;
        let Some(row) = gone.next()? else {
            return Err(Error::Invalid(format!("要删除的缓存行不存在：{table}")));
        };
        if keep_old {
            out.old.push((0..shape.width()).map(|i| row.get_ref(i).map(Cell::from)).collect::<rusqlite::Result<_>>()?);
        }
        out.deleted += 1;
    }
    let mut insert = c.prepare(&format!("INSERT INTO {name} VALUES ({})", vec!["?"; shape.width()].join(",")))?;
    for row in append {
        check()?;
        if row.len() != shape.width() {
            return Err(Error::Invalid(format!("缓存行列数不对：{table}")));
        }
        insert.execute(rusqlite::params_from_iter(row.iter()))?;
        out.inserted += 1;
        out.touched.push(c.last_insert_rowid());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    //! #31: synthetic tables only. The reference is the former publication: delete the whole table, then
    //! insert the desired rows in order.
    use super::*;

    fn ordered(c: &Connection, table: &str) -> Vec<Vec<Cell>> {
        let mut q = c.prepare(&format!("SELECT * FROM {table} ORDER BY rowid")).unwrap();
        let n = q.column_count();
        q.query_map([], |r| (0..n).map(|i| r.get_ref(i).map(Cell::from)).collect::<rusqlite::Result<Vec<_>>>())
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    }
    fn scanned(c: &Connection, table: &str) -> Vec<Vec<Cell>> {
        let mut q = c.prepare(&format!("SELECT * FROM {table}")).unwrap();
        let n = q.column_count();
        q.query_map([], |r| (0..n).map(|i| r.get_ref(i).map(Cell::from)).collect::<rusqlite::Result<Vec<_>>>())
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    }
    const SCHEMA: &str = "CREATE TABLE sigs(uuid TEXT PRIMARY KEY,ratio REAL,grid BLOB,ok INTEGER NOT NULL);
        CREATE TABLE pairs(a TEXT NOT NULL,b TEXT NOT NULL,PRIMARY KEY(a,b));
        CREATE TABLE info(cid INTEGER PRIMARY KEY,size INTEGER NOT NULL,built_at TEXT NOT NULL);";
    /// Deterministic generator; no external randomness.
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self, n: usize) -> usize {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((self.0 >> 33) % n as u64) as usize
        }
    }
    fn sig(id: usize, ratio: f64, ok: bool) -> Vec<Cell> {
        vec![
            Cell::Text(format!("u{id:05}").into_bytes()),
            if ok { Cell::Real(ratio.to_bits()) } else { Cell::Null },
            if ok { Cell::Blob(vec![(id % 251) as u8; 256]) } else { Cell::Null },
            Cell::Int(i64::from(ok)),
        ]
    }
    fn insert(c: &Connection, table: &str, rows: &[Vec<Cell>]) {
        for row in rows {
            c.execute(
                &format!("INSERT INTO {table} VALUES ({})", vec!["?"; row.len()].join(",")),
                rusqlite::params_from_iter(row.iter()),
            )
            .unwrap();
        }
    }
    /// A table changed the ways the calculators change their arrays: rows replaced and moved to the end,
    /// rewritten in place, removed and appended.
    fn mutate(rng: &mut Lcg, rows: &[Vec<Cell>], steps: usize, next_id: &mut usize) -> Vec<Vec<Cell>> {
        let mut rows = rows.to_vec();
        for _ in 0..steps {
            match rng.next(4) {
                0 if !rows.is_empty() => {
                    let i = rng.next(rows.len());
                    let mut row = rows.remove(i);
                    row[1] = Cell::Real((rng.next(1000) as f64 / 7.0).to_bits());
                    rows.push(row);
                }
                1 if !rows.is_empty() => {
                    let i = rng.next(rows.len());
                    rows[i][3] = Cell::Int(1 - matches!(rows[i][3], Cell::Int(1)) as i64);
                }
                2 if !rows.is_empty() => {
                    rows.remove(rng.next(rows.len()));
                }
                _ => {
                    *next_id += 1;
                    rows.push(sig(*next_id, 1.25, rng.next(3) > 0));
                }
            }
        }
        rows
    }
    fn open(rows: &[Vec<Cell>]) -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(SCHEMA).unwrap();
        insert(&c, "sigs", rows);
        c
    }

    #[test]
    fn plan_keeps_the_longest_prefix_in_order() {
        let key = |r: &(char, i32)| r.0;
        let same = |_: &(char, i32), _: &(char, i32)| true;
        let current = [('a', 1), ('b', 1), ('c', 1), ('d', 1)];
        // Nothing changed: everything stays.
        assert_eq!(plan(&current, &current, key, same), Plan { kept: vec![0, 1, 2, 3], delete: vec![] });
        // b replaced and moved to the end; c rewritten in place.
        let desired = [('a', 1), ('c', 2), ('d', 1), ('b', 2)];
        assert_eq!(plan(&current, &desired, key, same), Plan { kept: vec![0, 2, 3], delete: vec![1] });
        // A row that may not stay ends the prefix; later rows are appended again.
        let exact = plan(&current, &desired, key, |a, b| a == b);
        assert_eq!(exact, Plan { kept: vec![0], delete: vec![1, 2, 3] });
        assert_eq!(plan(&current, &[], key, same).delete, vec![0, 1, 2, 3]);
    }

    #[test]
    fn sync_and_apply_leave_the_full_rewrite_state_with_only_differing_writes() {
        let mut rng = Lcg(31);
        for round in 0..60 {
            let mut next = 0;
            let start: Vec<_> = (0..rng.next(40))
                .map(|_| {
                    next += 1;
                    sig(next, 1.0 + next as f64 / 3.0, rng.next(4) > 0)
                })
                .collect();
            // The current table has its own rowid history: some rows were replaced before.
            let history = rng.next(6);
            let current = mutate(&mut rng, &start, history, &mut next);
            let many = rng.next(20);
            let steps = [0, 1, many][round % 3];
            let desired = mutate(&mut rng, &current, steps, &mut next);
            let reference = open(&current);
            reference.execute("DELETE FROM sigs", []).unwrap();
            insert(&reference, "sigs", &desired);
            // Rows in memory (research results).
            let c = open(&current);
            let before = c.total_changes();
            let written = sync(&c, "sigs", desired.clone(), true, &|| Ok(())).unwrap();
            assert_eq!(c.total_changes() - before, written.rows() as u64);
            assert_eq!(ordered(&c, "sigs"), ordered(&reference, "sigs"), "round {round}");
            assert_eq!(scanned(&c, "sigs"), scanned(&reference, "sigs"), "round {round}");
            if steps == 0 {
                assert_eq!(written.rows(), 0, "round {round}");
            }
            assert!(written.rows() <= current.len() + desired.len());
            assert_eq!(written.old.len(), written.deleted + written.updated);
            assert_eq!(written.touched.len(), written.updated + written.inserted);
            // A change list computed against the stored rows (research worker): exact rows stay.
            let p = plan(&current, &desired, |r| r[0].clone(), |a, b| a == b);
            let c = open(&current);
            let delete: Vec<_> = p.delete.iter().map(|&i| vec![current[i][0].clone()]).collect();
            let applied = apply(&c, "sigs", &delete, desired[p.kept.len()..].to_vec(), false, &|| Ok(())).unwrap();
            assert_eq!(ordered(&c, "sigs"), ordered(&reference, "sigs"), "round {round}");
            assert_eq!(applied.updated, 0);
            if steps == 0 {
                assert_eq!(applied.rows(), 0, "round {round}");
            }
        }
    }

    #[test]
    fn mirror_makes_the_table_equal_to_the_staged_one_writing_only_differing_rows() {
        let mut rng = Lcg(51);
        let root = std::env::temp_dir().join(format!("illustash-mirror-{}", crate::store::new_key().unwrap()));
        std::fs::create_dir_all(&root).unwrap();
        for round in 0..40 {
            let mut next = 0;
            let rows: Vec<_> = (0..rng.next(40))
                .map(|_| {
                    next += 1;
                    sig(next, 1.0 + next as f64 / 3.0, rng.next(4) > 0)
                })
                .collect();
            let main = root.join(format!("main-{round}.sqlite"));
            let stage = root.join(format!("stage-{round}.sqlite"));
            let c = Connection::open(&main).unwrap();
            c.execute_batch(SCHEMA).unwrap();
            insert(&c, "sigs", &rows);
            c.execute("DELETE FROM sigs WHERE rowid % 7 = 3", []).unwrap();
            c.backup(rusqlite::DatabaseName::Main, &stage, None).unwrap();
            // What a worker does to its staged copy: in-place updates, replacements, deletions, new rows.
            let s = Connection::open(&stage).unwrap();
            let steps = [0, 1, rng.next(20)][round % 3];
            for _ in 0..steps {
                let id = format!("u{:05}", rng.next(next + 3));
                match rng.next(4) {
                    0 => s.execute("UPDATE sigs SET ratio=ratio+1 WHERE uuid=?", [&id]).unwrap(),
                    1 => s.execute("INSERT OR REPLACE INTO sigs VALUES (?,2.5,NULL,0)", [&id]).unwrap(),
                    2 => s.execute("DELETE FROM sigs WHERE uuid=?", [&id]).unwrap(),
                    _ => s.execute("INSERT OR IGNORE INTO sigs VALUES (?,NULL,x'00',1)", [&id]).unwrap(),
                };
            }
            let staged: Vec<(i64, Vec<Cell>)> = s
                .prepare("SELECT rowid,* FROM sigs ORDER BY rowid")
                .unwrap()
                .query_map([], |r| {
                    Ok((r.get(0)?, (1..5).map(|i| r.get_ref(i).map(Cell::from)).collect::<rusqlite::Result<_>>()?))
                })
                .unwrap()
                .map(|r| r.unwrap())
                .collect();
            drop(s);
            // The former publication, for the row order it left.
            let reference = Connection::open_in_memory().unwrap();
            reference.execute_batch(SCHEMA).unwrap();
            insert(&reference, "sigs", &staged.iter().map(|(_, r)| r.clone()).collect::<Vec<_>>());
            c.execute("ATTACH DATABASE ? AS stage", [stage.to_str().unwrap()]).unwrap();
            let before = c.total_changes();
            let written = mirror(&c, "sigs", "stage").unwrap();
            assert_eq!(c.total_changes() - before, written.rows() as u64);
            let mirrored: Vec<(i64, Vec<Cell>)> = c
                .prepare("SELECT rowid,* FROM main.sigs ORDER BY rowid")
                .unwrap()
                .query_map([], |r| {
                    Ok((r.get(0)?, (1..5).map(|i| r.get_ref(i).map(Cell::from)).collect::<rusqlite::Result<_>>()?))
                })
                .unwrap()
                .map(|r| r.unwrap())
                .collect();
            assert_eq!(mirrored, staged, "round {round}");
            assert_eq!(ordered(&reference, "sigs"), mirrored.into_iter().map(|(_, r)| r).collect::<Vec<_>>());
            if steps == 0 {
                assert_eq!(written.rows(), 0, "round {round}");
            }
            assert!(written.rows() <= 2 * steps, "round {round}: {written:?}");
        }
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn stored_affinity_composite_and_integer_keys() {
        let c = open(&[]);
        // An integer ratio is stored as REAL: the same row, nothing written.
        c.execute("INSERT INTO sigs VALUES ('x',2,NULL,1)", []).unwrap();
        let row = vec![Cell::Text(b"x".to_vec()), Cell::Int(2), Cell::Null, Cell::Real(1f64.to_bits())];
        assert_eq!(sync(&c, "sigs", vec![row], false, &|| Ok(())).unwrap().rows(), 0);
        // Composite keys; covering-index scans see the same rows as the full rewrite.
        insert(&c, "pairs", &[vec![Cell::Text(b"b".to_vec()), Cell::Text(b"c".to_vec())]]);
        let pairs = vec![
            vec![Cell::Text(b"a".to_vec()), Cell::Text(b"b".to_vec())],
            vec![Cell::Text(b"b".to_vec()), Cell::Text(b"c".to_vec())],
        ];
        let written = sync(&c, "pairs", pairs.clone(), false, &|| Ok(())).unwrap();
        assert_eq!((written.deleted, written.inserted), (1, 2));
        assert_eq!(scanned(&c, "pairs"), pairs);
        // INTEGER PRIMARY KEY rows keep their ids; a rewritten built_at is an in-place update.
        insert(&c, "info", &[vec![Cell::Int(1), Cell::Int(3), Cell::Text(b"t1".to_vec())]]);
        let info = vec![
            vec![Cell::Int(1), Cell::Int(3), Cell::Text(b"t2".to_vec())],
            vec![Cell::Int(2), Cell::Int(2), Cell::Text(b"t2".to_vec())],
        ];
        let written = sync(&c, "info", info.clone(), false, &|| Ok(())).unwrap();
        assert_eq!((written.deleted, written.updated, written.inserted), (0, 1, 1));
        assert_eq!(ordered(&c, "info"), info);
        // A change list removes by primary key, here the whole row; a re-added row moves to the end.
        let applied = apply(&c, "pairs", &[pairs[0].clone()], vec![pairs[0].clone()], false, &|| Ok(())).unwrap();
        assert_eq!((applied.deleted, applied.inserted), (1, 1));
        let pairs = vec![pairs[1].clone(), pairs[0].clone()];
        assert_eq!(scanned(&c, "pairs"), pairs);
        // A change list naming a row that is not stored, or of the wrong width, is refused.
        assert!(apply(
            &c,
            "pairs",
            &[vec![Cell::Text(b"z".to_vec()), Cell::Text(b"z".to_vec())]],
            vec![],
            false,
            &|| Ok(())
        )
        .is_err());
        assert!(apply(&c, "pairs", &[vec![Cell::Text(b"a".to_vec())]], vec![], false, &|| Ok(())).is_err());
        assert_eq!(scanned(&c, "pairs"), pairs);
        // A wrong row width is refused before anything is written.
        assert!(sync(&c, "pairs", vec![vec![Cell::Null]], false, &|| Ok(())).is_err());
        assert_eq!(scanned(&c, "pairs"), pairs);
    }
}
