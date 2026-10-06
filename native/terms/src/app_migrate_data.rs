//! Source rows and historical derived groups; no filesystem scan or worker execution.
use crate::source_rows::cell;
pub(crate) use crate::source_rows::{rows, rows_with};
use crate::{Error, Result};
use rusqlite::{params, Connection};
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
pub(crate) fn index(c: &Connection) -> Result<Value> {
    let mut q = c.prepare("SELECT * FROM characters")?;
    let cols = q.column_names().iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let chars = q
        .query_map([], |r| {
            let mut o = Map::new();
            for (i, k) in cols.iter().enumerate() {
                o.insert(k.clone(), cell(r.get_ref(i)?)?);
            }
            Ok(Value::Object(o))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let tag =
        if cols.iter().any(|s| s == "danbooru_tag") { "coalesce(nullif(danbooru_tag,''),char_id)" } else { "char_id" };
    Ok(
        json!({"characters":chars,"aliases":rows(c,"SELECT char_id,alias FROM character_aliases")?,"tags":rows(c,&format!("SELECT char_id,{tag} FROM characters"))?,"new_ids":{}}),
    )
}
#[derive(Default)]
struct Groups {
    names: Vec<String>,
    ids: HashMap<String, usize>,
    parent: Vec<usize>,
}
impl Groups {
    fn get(&mut self, u: &str) -> usize {
        if let Some(i) = self.ids.get(u) {
            *i
        } else {
            let i = self.names.len();
            self.names.push(u.into());
            self.ids.insert(u.into(), i);
            self.parent.push(i);
            i
        }
    }
    fn find(&mut self, mut i: usize) -> usize {
        while self.parent[i] != i {
            self.parent[i] = self.parent[self.parent[i]];
            i = self.parent[i];
        }
        i
    }
    fn join(&mut self, a: &str, b: &str) {
        let a = self.get(a);
        let a = self.find(a);
        let b = self.get(b);
        let b = self.find(b);
        self.parent[a] = b;
    }
    fn grouped(&mut self) -> Vec<Vec<String>> {
        let mut order = Vec::new();
        let mut groups = HashMap::<usize, Vec<String>>::new();
        for i in 0..self.names.len() {
            let root = self.find(i);
            if !groups.contains_key(&root) {
                order.push(root);
            }
            groups.entry(root).or_default().push(self.names[i].clone());
        }
        order.into_iter().map(|r| groups.remove(&r).expect("app_migrate_data internal invariant")).collect()
    }
}
pub(crate) fn duplicates(c: &Connection, src: &Connection, assets: &[Value]) -> Result<Vec<Value>> {
    let mut shape = HashMap::new();
    for r in assets {
        let id = r[0].as_str().ok_or_else(|| Error::Schema("无效资产 id".into()))?;
        shape.insert(
            id.to_owned(),
            (r[5].as_str().unwrap_or("").to_owned(), r[3].as_i64().unwrap_or(0) * r[4].as_i64().unwrap_or(0)),
        );
    }
    let mut groups = Groups::default();
    let mut first = HashMap::<String, String>::new();
    let mut hashes = HashMap::<String, String>::new();
    if src.prepare("SELECT stable_hash FROM assets LIMIT 0").is_ok() {
        for r in rows(src, "SELECT uuid,stable_hash FROM assets WHERE coalesce(stable_hash,'')<>''")? {
            let (u, key) = (
                r[0].as_str().expect("validated internal text field"),
                r[1].as_str().expect("validated internal text field"),
            );
            if shape.contains_key(u) {
                hashes.insert(u.into(), key.into());
                let prev = first.entry(key.into()).or_insert_with(|| u.into());
                groups.join(u, prev);
            }
        }
    }
    let kinds: Option<String> =
        c.query_row("SELECT value FROM prefs WHERE key='fold_kinds'", [], |r| r.get(0)).optional()?;
    let kinds: Value = match kinds {
        Some(k) => serde_json::from_str(&k).map_err(|e| Error::Invalid(e.to_string()))?,
        None => json!(["identical", "duplicate"]),
    };
    for r in rows(c, "SELECT a,b,kind FROM relations")? {
        let (a, b) = (
            r[0].as_str().expect("validated internal text field"),
            r[1].as_str().expect("validated internal text field"),
        );
        if kinds.as_array().is_some_and(|k| k.contains(&r[2])) && shape.contains_key(a) && shape.contains_key(b) {
            groups.join(a, b);
        }
    }
    let mut out = Vec::new();
    for mut g in groups.grouped() {
        if g.len() < 2 {
            continue;
        }
        let key = g
            .iter()
            .filter_map(|u| hashes.get(u))
            .min()
            .cloned()
            .unwrap_or_else(|| format!("sim:{}", g.iter().min().expect("app_migrate_data internal invariant")));
        g.sort_by_key(|u| (shape[u].0.clone(), u.clone()));
        let rep = g
            .iter()
            .min_by_key(|u| (-shape[*u].1, shape[*u].0.clone(), (*u).clone()))
            .expect("app_migrate_data internal invariant")
            .clone();
        for (pos, u) in g.iter().enumerate() {
            out.push(json!([
                u,
                key,
                g.len(),
                pos + 1,
                shape[g.last().expect("app_migrate_data internal invariant")].0,
                u == &rep
            ]));
        }
    }
    Ok(out)
}
use rusqlite::OptionalExtension;
pub(crate) fn early_groups(c: &Connection, src: &Connection) -> Result<()> {
    let assets = rows(c, "SELECT uuid,filename,src_path,width,height,added_at,src_kind FROM assets")?;
    c.execute("DELETE FROM duplicates", [])?;
    for r in duplicates(c, src, &assets)? {
        c.execute(
            "INSERT INTO duplicates(uuid,grp,size,pos,grp_added,rep) VALUES (?,?,?,?,?,?)",
            params![r[0].as_str(), r[1].as_str(), r[2].as_i64(), r[3].as_i64(), r[4].as_str(), r[5].as_bool()],
        )?;
    }
    c.execute("DELETE FROM similar_groups", [])?;
    let present = assets.iter().filter_map(|r| r[0].as_str()).collect::<HashSet<_>>();
    let mut groups = Groups::default();
    for r in
        rows(c, "SELECT a,b FROM relations WHERE kind IN ('identical','duplicate','subtle','crop','variant','edit')")?
    {
        let (a, b) = (
            r[0].as_str().expect("validated internal text field"),
            r[1].as_str().expect("validated internal text field"),
        );
        if present.contains(a) && present.contains(b) {
            groups.join(a, b);
        }
    }
    let mut g = groups.grouped().into_iter().filter(|g| g.len() > 1).collect::<Vec<_>>();
    for g in &mut g {
        g.sort();
    }
    g.sort_by_key(|g| (-(g.len() as i64), g[0].clone()));
    for (i, g) in g.into_iter().enumerate() {
        for u in g {
            c.execute("INSERT INTO similar_groups VALUES (?,?)", params![u, i + 1])?;
        }
    }
    Ok(())
}
