//! Existing SQLite-to-JSON source decoding shared by read adapters and migrations.
use crate::Result;
use rusqlite::{types::ValueRef, Connection};
use serde_json::Value;
pub fn rows(c: &Connection, sql: &str) -> Result<Vec<Value>> {
    rows_with(c, sql, [])
}
pub fn rows_with(c: &Connection, sql: &str, params: impl rusqlite::Params) -> Result<Vec<Value>> {
    let mut q = c.prepare_cached(sql)?;
    let n = q.column_count();
    let result = q
        .query_map(params, |r| {
            let mut row = Vec::new();
            for i in 0..n {
                row.push(cell(r.get_ref(i)?)?);
            }
            Ok(Value::Array(row))
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(result)
}
pub fn cell(v: ValueRef<'_>) -> rusqlite::Result<Value> {
    Ok(match v {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(i) => i.into(),
        ValueRef::Real(f) => f.into(),
        ValueRef::Text(t) => std::str::from_utf8(t)
            .map_err(|e| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e)))?
            .into(),
        ValueRef::Blob(_) => {
            return Err(rusqlite::Error::InvalidColumnType(0, "migration source".into(), rusqlite::types::Type::Blob))
        }
    })
}
