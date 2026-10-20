//! S3 / D6: optimistic learning publication. The stamp is taken before the Python
//! training snapshot; BEGIN IMMEDIATE checks it before any write. A change between
//! stamp and snapshot conservatively rejects, never publishes stale training.
use crate::character_links::LINKS;
use crate::{character_projection, schema, Error, Result};
use rusqlite::{
    params,
    types::{Value as Sql, ValueRef},
    Connection, OptionalExtension, TransactionBehavior,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

const INPUTS: &[&str] = &[
    "assets",
    "entities",
    "links",
    "assertions",
    "actions",
    "candidates",
    "single_assets",
    "lead_assets",
    "duplicates",
    "relations",
    "identity_baseline",
    "identity_relations",
    "identity_evidence",
    "identity_decisions",
    "identity_decision_evidence",
    "identity_canonical",
    "identity_scopes",
    "identity_suspended",
    "identity_uncertain",
    "identity_anchor_decisions",
    "identity_anchor_history",
    "prototypes",
    "learn_state",
    "proto_claims",
    "weak_claims",
];

fn feed(h: &mut Sha256, bytes: &[u8]) {
    h.update((bytes.len() as u64).to_le_bytes());
    h.update(bytes);
}

/// A versioned, typed, length-delimited stream. Blobs are hashed as bytes, without
/// JSON/base64 expansion; row/column boundaries and missing tables are explicit.
fn stamp(c: &Connection) -> Result<String> {
    character_projection::policy(c)?;
    let mut h = Sha256::new();
    feed(&mut h, b"character-learning-v1");
    for &table in INPUTS {
        feed(&mut h, table.as_bytes());
        // The character links live in `term_links` since core v8 (#201); they are read in the former `links` shape,
        // so the same links give the same stamp.
        let (stored, from) = if table == "links" {
            ("term_links", format!("(SELECT system, key, entity_id, current, source, added_at FROM {LINKS})"))
        } else {
            (table, table.to_owned())
        };
        let exists: bool =
            c.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?)", [stored], |r| {
                r.get(0)
            })?;
        feed(&mut h, &[u8::from(exists)]);
        if !exists {
            continue;
        }
        let cols = c.prepare(&format!("SELECT * FROM {from}"))?.column_count();
        let order = (1..=cols).map(|n| n.to_string()).collect::<Vec<_>>().join(",");
        let mut statement = c.prepare(&format!("SELECT * FROM {from} ORDER BY {order}"))?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            feed(&mut h, b"row");
            for col in 0..cols {
                match row.get_ref(col)? {
                    ValueRef::Null => feed(&mut h, b"null"),
                    ValueRef::Integer(v) => {
                        feed(&mut h, b"int");
                        feed(&mut h, &v.to_le_bytes());
                    }
                    ValueRef::Real(v) => {
                        feed(&mut h, b"real");
                        feed(&mut h, &v.to_bits().to_le_bytes());
                    }
                    ValueRef::Text(v) => {
                        feed(&mut h, b"text");
                        feed(&mut h, v);
                    }
                    ValueRef::Blob(v) => {
                        feed(&mut h, b"blob");
                        feed(&mut h, v);
                    }
                }
            }
        }
    }
    for key in ["identity_policy", "identity_graph_version", "identity_bank"] {
        feed(&mut h, key.as_bytes());
        let value: Option<String> =
            c.query_row("SELECT value FROM meta WHERE key=?", [key], |r| r.get(0)).optional()?;
        feed(&mut h, &[u8::from(value.is_some())]);
        if let Some(value) = value {
            feed(&mut h, value.as_bytes());
        }
    }
    Ok(format!("learning-v1:{:x}", h.finalize()))
}

pub fn snapshot(c: &mut Connection) -> Result<Value> {
    schema::require_current(c)?;
    let tx = c.transaction()?;
    let token = stamp(&tx)?;
    tx.commit()?;
    Ok(json!({"token": token}))
}

fn bad() -> Error {
    Error::Invalid("学习批次格式不正确".into())
}
fn text(v: &Value) -> Result<String> {
    v.as_str().map(str::to_owned).ok_or_else(bad)
}
fn number(v: &Value) -> Result<f64> {
    v.as_f64().filter(|v| v.is_finite()).ok_or_else(bad)
}
fn integer(v: &Value) -> Result<i64> {
    v.as_i64().filter(|v| *v >= 0).ok_or_else(bad)
}
fn rows<'a>(v: &'a Value, key: &str, n: usize) -> Result<Vec<&'a Vec<Value>>> {
    v.get(key)
        .and_then(Value::as_array)
        .ok_or_else(bad)?
        .iter()
        .map(|r| r.as_array().filter(|r| r.len() == n).ok_or_else(bad))
        .collect()
}
fn blob(v: &Value) -> Result<Vec<u8>> {
    let s = v.as_str().ok_or_else(bad)?;
    if !s.is_ascii() || s.is_empty() || s.len() % 8 != 0 {
        return Err(bad());
    }
    let bytes: Vec<u8> = (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|_| bad()))
        .collect::<Result<_>>()?;
    if bytes.chunks_exact(4).any(|b| !f32::from_le_bytes(b.try_into().expect("four bytes")).is_finite()) {
        return Err(bad());
    }
    Ok(bytes)
}

struct Batch {
    token: String,
    relearned: i64,
    prototypes: Vec<Vec<Sql>>,
    states: Vec<Vec<Sql>>,
    claims: BTreeMap<(String, String), f64>,
    weak: Vec<Vec<Sql>>,
}
impl Batch {
    fn parse(v: &Value) -> Result<Self> {
        let keys = v.as_object().ok_or_else(bad)?.keys().map(String::as_str).collect::<BTreeSet<_>>();
        if keys
            != BTreeSet::from(["v", "token", "relearned", "prototypes", "learn_state", "proto_claims", "weak_claims"])
            || v["v"] != 1
        {
            return Err(bad());
        }
        let prototypes = rows(v, "prototypes", 6)?
            .into_iter()
            .map(|r| {
                Ok(vec![
                    Sql::Text(text(&r[0])?),
                    Sql::Integer(integer(&r[1])?),
                    Sql::Blob(blob(&r[2])?),
                    Sql::Real(number(&r[3])?),
                    Sql::Integer(integer(&r[4])?),
                    Sql::Real(number(&r[5])?),
                ])
            })
            .collect::<Result<_>>()?;
        let states = rows(v, "learn_state", 3)?
            .into_iter()
            .map(|r| r.iter().map(|v| Ok(Sql::Text(text(v)?))).collect::<Result<Vec<_>>>())
            .collect::<Result<_>>()?;
        let mut claims = BTreeMap::new();
        for r in rows(v, "proto_claims", 3)? {
            if claims.insert((text(&r[0])?, text(&r[1])?), number(&r[2])?).is_some() {
                return Err(bad());
            }
        }
        let weak = rows(v, "weak_claims", 4)?
            .into_iter()
            .map(|r| {
                Ok(vec![
                    Sql::Text(text(&r[0])?),
                    Sql::Text(text(&r[1])?),
                    Sql::Text(text(&r[2])?),
                    Sql::Real(number(&r[3])?),
                ])
            })
            .collect::<Result<_>>()?;
        Ok(Self { token: text(&v["token"])?, relearned: integer(&v["relearned"])?, prototypes, states, claims, weak })
    }
}

/// Publication owns all four tables, the derived ownership and folding, in one
/// transaction. No effects event: the original learner does not emit one either.
pub fn publish(c: &mut Connection, input: &Value) -> Result<Value> {
    schema::require_current(c)?;
    let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let result = publish_tx(&tx, input)?;
    if result["status"] != "stale" {
        schema::commit_writing(tx)?;
    }
    Ok(result)
}
pub fn publish_tx(tx: &rusqlite::Transaction<'_>, input: &Value) -> Result<Value> {
    let batch = Batch::parse(input)?;
    if stamp(tx)? != batch.token {
        return Ok(json!({"status": "stale", "relearned": 0, "changed": 0}));
    }
    schema::begin_writing(tx)?;
    let old: BTreeMap<(String, String), f64> = tx
        .prepare("SELECT uuid,entity,score FROM proto_claims")?
        .query_map([], |r| Ok(((r.get(0)?, r.get(1)?), r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut changed = BTreeSet::new();
    for ((u, e), score) in &batch.claims {
        if old.get(&(u.clone(), e.clone())).is_none_or(|s| (s - score).abs() > 1e-6) {
            changed.insert(u.clone());
        }
    }
    for key in old.keys() {
        if !batch.claims.contains_key(key) {
            changed.insert(key.0.clone());
        }
    }
    for (table, values, marks) in [
        ("prototypes", &batch.prototypes, "?,?,?,?,?,?"),
        ("learn_state", &batch.states, "?,?,?"),
        ("weak_claims", &batch.weak, "?,?,?,?"),
    ] {
        tx.execute(&format!("DELETE FROM {table}"), [])?;
        let mut insert = tx.prepare(&format!("INSERT INTO {table} VALUES ({marks})"))?;
        for row in values {
            insert.execute(rusqlite::params_from_iter(row))?;
        }
    }
    tx.execute("DELETE FROM proto_claims", [])?;
    for ((uuid, entity), score) in &batch.claims {
        tx.execute("INSERT INTO proto_claims VALUES (?,?,?)", params![uuid, entity, score])?;
    }
    if !changed.is_empty() {
        let uuids = changed.into_iter().collect::<Vec<_>>();
        character_projection::evidence(tx, Some(&uuids))?;
        character_projection::memberships(tx, Some(&uuids))?;
        let out = json!({"status": "published", "relearned": batch.relearned, "changed": uuids.len()});
        return Ok(out);
    }
    Ok(json!({"status": "published", "relearned": batch.relearned, "changed": 0}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> Value {
        json!({"v":1,"token":"snapshot","relearned":1,"prototypes":[],"learn_state":[],"proto_claims":[],"weak_claims":[]})
    }

    #[test]
    fn batches_are_typed_and_duplicate_claims_are_rejected() {
        assert!(Batch::parse(&input()).is_ok());
        let mut value = input();
        value["proto_claims"] = json!([["u", "e", 0.9], ["u", "e", 0.8]]);
        assert!(Batch::parse(&value).is_err());
        value = input();
        value["extra"] = json!(true);
        assert!(Batch::parse(&value).is_err());
        value = input();
        value["prototypes"] = json!([["e", 0, "0000803f", 0.03, 6, 0.045]]);
        assert!(Batch::parse(&value).is_ok());
        for hex in ["", "f", "123456xx", "0000807f", "0000c07f", "你好"] {
            value["prototypes"][0][2] = json!(hex);
            assert!(Batch::parse(&value).is_err(), "{hex}");
        }
    }

    #[test]
    fn stamp_is_stable_typed_and_sensitive_to_undo_history() {
        let mut c = Connection::open_in_memory().unwrap();
        crate::migrate(&mut c).unwrap();
        c.execute_batch(
            "CREATE TABLE assets(uuid TEXT PRIMARY KEY, value); INSERT INTO assets VALUES ('b',2),('a',1);",
        )
        .unwrap();
        let first = snapshot(&mut c).unwrap();
        c.execute_batch("DELETE FROM assets; INSERT INTO assets VALUES ('a',1),('b',2);").unwrap();
        assert_eq!(first, snapshot(&mut c).unwrap());
        c.execute("UPDATE assets SET value='1' WHERE uuid='a'", []).unwrap();
        assert_ne!(first, snapshot(&mut c).unwrap());
        c.execute("UPDATE assets SET value=1 WHERE uuid='a'", []).unwrap();
        assert_eq!(first, snapshot(&mut c).unwrap());
        c.execute("INSERT INTO actions(label,before_json,undone) VALUES ('undo','[]',1)", []).unwrap();
        assert_ne!(first, snapshot(&mut c).unwrap());
        let current = stamp(&c).unwrap();
        let tx = c.transaction().unwrap();
        tx.execute("DELETE FROM assets", []).unwrap();
        assert_ne!(current, stamp(&tx).unwrap());
        tx.rollback().unwrap();
        assert_eq!(current, stamp(&c).unwrap());
    }

    #[test]
    fn stale_batch_leaves_no_writer_key() {
        let mut c = Connection::open_in_memory().unwrap();
        crate::migrate(&mut c).unwrap();
        let result = publish(&mut c, &input()).unwrap();
        assert_eq!(result["status"], "stale");
        assert!(!c
            .query_row("SELECT EXISTS(SELECT 1 FROM meta WHERE key='terms_writer')", [], |r| r.get::<_, bool>(0))
            .unwrap());
    }
}
