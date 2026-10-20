//! Narrow adapter for the explicit disposable snapshot, never an application Store.
use crate::{Error, Result};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
};

pub struct Snapshot {
    pub source: PathBuf,
    library: PathBuf,
}
impl Snapshot {
    pub fn open(root: &Path) -> Result<Self> {
        let root = fs::canonicalize(root)?;
        let locate = |name: &str| -> Result<PathBuf> {
            let path = root.join(name);
            if !path.is_file() || fs::symlink_metadata(&path)?.file_type().is_symlink() {
                return Err(Error::Invalid("research snapshot databases missing or redirected".into()));
            }
            Ok(path)
        };
        Ok(Self { source: locate("source.sqlite")?, library: locate("library.sqlite")? })
    }
    pub fn read(&self) -> Result<Connection> {
        let c = crate::sqlite::open_with_flags(&self.library, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        c.busy_timeout(std::time::Duration::from_secs(20))?;
        Ok(c)
    }
    pub fn resolve(c: &Connection, ident: &str) -> Result<String> {
        let mut ident = ident.to_owned();
        for _ in 0..16 {
            let merged: Option<Option<String>> = c
                .query_row(
                    &format!(
                        "SELECT merged_into FROM {ENTITIES} WHERE id=?",
                        ENTITIES = crate::character_entities::ENTITIES
                    ),
                    [&ident],
                    |r| r.get(0),
                )
                .optional()?;
            match merged.flatten().filter(|s| !s.is_empty()) {
                Some(next) => ident = next,
                None => break,
            }
        }
        Ok(ident)
    }
    pub fn publish_learning(&self, batch: &Value) -> Result<Value> {
        // Only the existing validated core publication can write the isolated library.
        // The application repeats validation/publication against the original token and fingerprint.
        let mut c = crate::sqlite::open_with_flags(&self.library, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
        crate::configure(&c)?;
        crate::character_learning::publish(&mut c, batch)
    }
}

pub fn strings(c: &Connection, sql: &str, args: &[&dyn rusqlite::ToSql]) -> Result<Vec<String>> {
    Ok(c.prepare(sql)?.query_map(args, |r| r.get(0))?.collect::<rusqlite::Result<_>>()?)
}
pub fn table(c: &Connection, name: &str) -> Result<Vec<Value>> {
    // Result extraction is restricted to the only cache tables owned by this issue.
    if !["prototypes", "learn_state"].contains(&name) {
        return Err(Error::Invalid("unknown learning table".into()));
    }
    let mut q = c.prepare(&format!("SELECT * FROM {name}"))?;
    let n = q.column_count();
    let rows = q
        .query_map([], |r| {
            use rusqlite::types::ValueRef;
            let mut out = vec![];
            for i in 0..n {
                out.push(match r.get_ref(i)? {
                    ValueRef::Null => Value::Null,
                    ValueRef::Integer(v) => v.into(),
                    ValueRef::Real(v) => v.into(),
                    ValueRef::Text(v) => String::from_utf8_lossy(v).into_owned().into(),
                    ValueRef::Blob(v) => v.iter().map(|b| format!("{b:02x}")).collect::<String>().into(),
                });
            }
            Ok(Value::Array(out))
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}
