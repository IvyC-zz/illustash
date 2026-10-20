//! Line-for-line semantics of web/identity.py _graph / _project, hybrid only.
//! Baseline order is the preserved legacy rowid (seq); representatives are b-side.
use crate::{character_json, character_projection, Error, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap, HashSet};

#[derive(Debug)]
struct Node {
    id: String,
    parent: Option<String>,
    variant: Option<String>,
    kind: String,
    merged: Option<String>,
}
#[derive(Debug)]
pub struct Graph {
    nodes: Vec<Node>,
    mapping: Vec<usize>,
    hierarchy: Vec<Option<usize>>,
    families: Vec<usize>,
}
fn invalid(message: &str) -> Error {
    Error::Invalid(message.into())
}
fn find(parent: &mut [usize], mut i: usize) -> usize {
    while parent[i] != i {
        parent[i] = parent[parent[i]];
        i = parent[i];
    }
    i
}

pub fn baseline(c: &Connection) -> Result<()> {
    // seq is an INTEGER PRIMARY KEY: omitted values append in SELECT row order.
    c.prepare_cached(
        "INSERT OR IGNORE INTO term_identity_baseline(id,parent_id,variant,kind,merged_into)
        SELECT id,parent_id,variant,kind,merged_into FROM entities WHERE kind<>'work'",
    )?
    .execute([])?;
    // 兼容表 identity_baseline（旧代码按原样读它）：按 seq 保留行序跟着追加。
    let compat: bool = c
        .prepare_cached("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='identity_baseline')")?
        .query_row([], |r| r.get(0))?;
    if compat {
        c.prepare_cached(
            "INSERT OR IGNORE INTO identity_baseline(rowid,id,parent_id,variant,kind,merged_into)
             SELECT seq,id,parent_id,variant,kind,merged_into FROM term_identity_baseline t
             WHERE NOT EXISTS(SELECT 1 FROM identity_baseline b WHERE b.id=t.id)",
        )?
        .execute([])?;
    }
    Ok(())
}

pub fn graph(c: &Connection) -> Result<Graph> {
    let nodes: Vec<Node> = c
        .prepare_cached("SELECT id,parent_id,variant,kind,merged_into FROM term_identity_baseline ORDER BY seq")?
        .query_map([], |r| {
            Ok(Node { id: r.get(0)?, parent: r.get(1)?, variant: r.get(2)?, kind: r.get(3)?, merged: r.get(4)? })
        })?
        .collect::<rusqlite::Result<_>>()?;
    let positions: HashMap<&str, usize> = nodes.iter().enumerate().map(|(i, n)| (n.id.as_str(), i)).collect();
    let index = |s: &str| positions.get(s).copied().ok_or_else(|| invalid("关系端点不存在"));
    let accepted: Vec<(String, String, String)> = c
        .prepare_cached(&format!(
            "WITH relations(id,a,b,kind,status) AS ({}) SELECT a,b,kind FROM relations WHERE status='accepted' ORDER BY id",
            crate::legacy_characters::compat_select("identity_relations")?
        ))?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut parent: Vec<usize> = (0..nodes.len()).collect();
    for (i, n) in nodes.iter().enumerate() {
        if let Some(j) = n.merged.as_deref().and_then(|s| positions.get(s)) {
            let a = find(&mut parent, i);
            let b = find(&mut parent, *j);
            parent[a] = b;
        }
    }
    for (a, b, kind) in &accepted {
        if kind == "duplicate_of" {
            let a = find(&mut parent, index(a)?);
            let b = find(&mut parent, index(b)?);
            parent[a] = b;
        }
    }
    let mapping: Vec<usize> = (0..nodes.len()).map(|i| find(&mut parent, i)).collect();
    let mut hierarchy = vec![None; nodes.len()];
    let attach = |a: usize, b: usize, hierarchy: &mut Vec<Option<usize>>| -> Result<()> {
        let (child, person) = (mapping[a], mapping[b]);
        if child == person {
            return Err(invalid("变体不能与其人物重复"));
        }
        if hierarchy[child].is_some_and(|p| p != person) {
            return Err(invalid("变体已有另一个父人物，需先撤销旧关系"));
        }
        hierarchy[child] = Some(person);
        Ok(())
    };
    for (i, n) in nodes.iter().enumerate() {
        if let Some(p) = n.parent.as_deref().filter(|s| !s.is_empty()) {
            attach(i, index(p)?, &mut hierarchy)?;
        }
    }
    for (a, b, kind) in &accepted {
        if kind == "different_from" && mapping[index(a)?] == mapping[index(b)?] {
            return Err(invalid("确认不同与重复等价组冲突"));
        }
        if kind == "form_of" {
            attach(index(a)?, index(b)?, &mut hierarchy)?;
        }
    }
    let mut families = Vec::new();
    for &mapped in &mapping {
        let mut i = mapped;
        let mut seen = HashSet::new();
        while let Some(p) = hierarchy[i] {
            if !seen.insert(i) {
                return Err(invalid("人物—变体关系存在环"));
            }
            i = p;
        }
        families.push(i);
    }
    for (a, b, kind) in &accepted {
        if kind == "different_from" && families[index(a)?] == families[index(b)?] {
            return Err(invalid("确认不同与人物—变体关系冲突"));
        }
    }
    Ok(Graph { nodes, mapping, hierarchy, families })
}

const PENDING: &str = "SELECT DISTINCT n.character_id
      FROM identity_relations r JOIN identity_nodes n ON n.id=r.a
      JOIN identity_evidence e ON e.relation_id=r.id AND e.source='legacy-hierarchy'
      WHERE r.kind='form_of' AND r.status='accepted' AND NOT EXISTS(
        SELECT 1 FROM identity_decision_evidence x WHERE x.fingerprint=e.fingerprint
        AND x.decision_id=(SELECT max(d.id) FROM identity_decisions d WHERE d.relation_id=r.id))";

pub fn version(c: &Connection) -> Result<String> {
    let rows: Vec<Value> = c
        .prepare_cached("SELECT id,parent_id,canonical_id FROM identity_nodes ORDER BY id")?
        .query_map([], |r| Ok(json!([r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, String>(2)?])))?
        .collect::<rusqlite::Result<_>>()?;
    let pending: BTreeSet<String> =
        c.prepare_cached(PENDING)?.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
    Ok(crate::strict_json::sha256_hex(character_json::dump(&json!([rows, pending]), true).as_bytes()))
}

pub fn affected_images(c: &Connection, ids: &BTreeSet<String>) -> Result<BTreeSet<String>> {
    if ids.is_empty() {
        return Ok(BTreeSet::new());
    }
    c.execute_batch("CREATE TEMP TABLE IF NOT EXISTS reprojected(id TEXT PRIMARY KEY);DELETE FROM reprojected;")?;
    let mut insert = c.prepare_cached("INSERT INTO reprojected VALUES (?)")?;
    for id in ids {
        insert.execute([id])?;
    }
    Ok(c.prepare_cached(
        "SELECT uuid FROM candidates WHERE entity IN (SELECT id FROM reprojected)
        UNION SELECT uuid FROM proto_claims WHERE entity IN (SELECT id FROM reprojected)
        UNION SELECT uuid FROM assertions WHERE entity IN (SELECT id FROM reprojected)",
    )?
    .query_map([], |r| r.get(0))?
    .collect::<rusqlite::Result<_>>()?)
}

fn stored(s: &str) -> Result<Value> {
    crate::strict_json::parse(s).map_err(Error::Invalid)
}
type EntityState = HashMap<String, (Option<String>, Option<String>)>;
fn entity_state(c: &Connection) -> Result<EntityState> {
    Ok(c.prepare_cached("SELECT id,parent_id,merged_into FROM entities")?
        .query_map([], |r| Ok((r.get(0)?, (r.get(1)?, r.get(2)?))))?
        .collect::<rusqlite::Result<_>>()?)
}

pub fn project(c: &Connection, g: &Graph) -> Result<Value> {
    let before = entity_state(c)?;
    let mapping: HashMap<&str, &str> =
        g.nodes.iter().enumerate().map(|(i, n)| (n.id.as_str(), g.nodes[g.mapping[i]].id.as_str())).collect();
    let is_split = |v: &Value| -> Result<bool> {
        let mut roots = HashSet::new();
        for id in v.as_array().ok_or_else(|| invalid("身份范围应为数组"))? {
            let id = id.as_str().ok_or_else(|| invalid("身份范围应为字符串"))?;
            roots.insert(mapping.get(id).copied().unwrap_or(id));
        }
        Ok(roots.len() > 1)
    };
    let mut suspended = BTreeSet::new();
    let scopes: Vec<(String, String, String)> = c
        .prepare_cached("SELECT uuid,entity,members_json FROM identity_scopes")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (u, e, members) in scopes {
        if is_split(&stored(&members)?)? {
            c.prepare_cached("INSERT OR REPLACE INTO identity_suspended VALUES (?,?,?,?)")?.execute(params![
                u,
                e,
                "equivalence_split",
                members
            ])?;
            suspended.insert(u);
        }
    }
    c.execute_batch(
        "CREATE TEMP TABLE IF NOT EXISTS graph_projection(
      id TEXT PRIMARY KEY,parent_id TEXT,variant TEXT,kind TEXT,merged_into TEXT);
      DELETE FROM graph_projection;",
    )?;
    let mut put = c.prepare_cached("INSERT INTO graph_projection VALUES (?,?,?,?,?)")?;
    for (i, n) in g.nodes.iter().enumerate() {
        let parent = g.hierarchy[g.mapping[i]].map(|_| g.nodes[g.families[i]].id.as_str());
        let variant = if parent.is_none() {
            String::new()
        } else if let Some(v) = n.variant.as_ref().filter(|v| !v.is_empty()) {
            v.clone()
        } else {
            c.prepare_cached(
                &format!(
                    "SELECT l.key FROM {} l WHERE l.entity_id = ? AND l.core_system = 'danbooru' ORDER BY l.current DESC LIMIT 1",
                    crate::character_links::LINKS
                ),
            )?
            .query_row([&n.id], |r| r.get(0))
            .optional()?
            .unwrap_or_else(|| n.id.clone())
        };
        let kind = if parent.is_some() { "skin" } else { n.kind.as_str() };
        let canonical = &g.nodes[g.mapping[i]].id;
        let merged = (canonical != &n.id).then_some(canonical.as_str());
        put.execute(params![n.id, parent, variant, kind, merged])?;
    }
    drop(put);
    // 代表映射来自同一张完整图，只改增删项和真正改变的映射。
    c.execute_batch(
        "DELETE FROM identity_canonical WHERE entity NOT IN (SELECT id FROM graph_projection);
        INSERT INTO identity_canonical(entity,canonical)
        SELECT id,coalesce(merged_into,id) FROM graph_projection WHERE true
        ON CONFLICT(entity) DO UPDATE SET canonical=excluded.canonical
        WHERE identity_canonical.canonical IS NOT excluded.canonical;",
    )?;
    // These mirrors are private. Keep exact JSON normalization, but avoid no-op writes.
    c.execute(
        "UPDATE term_tree SET parent_id=p.parent_id,merged_into=p.merged_into
      FROM graph_projection p JOIN terms t ON t.key=p.id WHERE term_tree.term_id=t.id
      AND (term_tree.parent_id IS NOT p.parent_id OR term_tree.merged_into IS NOT p.merged_into)",
        [],
    )?;
    c.execute(
        "UPDATE terms SET kind=p.kind,attrs=json_set(terms.attrs,'$.variant',p.variant)
      FROM graph_projection p WHERE terms.key=p.id
      AND (terms.kind IS NOT p.kind OR terms.attrs IS NOT json_set(terms.attrs,'$.variant',p.variant))",
        [],
    )?;
    // 名字版本只需在可见结构改变时失效，无差别更新全部节点会触发全库写放大。
    c.execute(
        "UPDATE entities SET parent_id=p.parent_id,variant=p.variant,kind=p.kind,merged_into=p.merged_into
      FROM graph_projection p WHERE entities.id=p.id
      AND (entities.parent_id IS NOT p.parent_id OR entities.variant IS NOT p.variant
        OR entities.kind IS NOT p.kind OR entities.merged_into IS NOT p.merged_into)",
        [],
    )?;
    let version = version(c)?;
    let previous: Option<String> = c
        .prepare_cached("SELECT value FROM meta WHERE key='identity_graph_version'")?
        .query_row([], |r| r.get(0))
        .optional()?;
    let changed = previous.as_deref() != Some(version.as_str());
    let mut affected = BTreeSet::new();
    if changed {
        let current = entity_state(c)?;
        let ids =
            current.iter().filter(|(id, state)| before.get(*id) != Some(*state)).map(|(id, _)| id.clone()).collect();
        affected = affected_images(c, &ids)?;
        affected.extend(suspended);
        let uuids: Vec<String> = affected.iter().cloned().collect();
        character_projection::evidence(c, Some(&uuids))?;
        character_projection::memberships(c, Some(&uuids))?;
    }
    c.prepare_cached("INSERT OR REPLACE INTO meta VALUES ('identity_graph_version',?)")?.execute([&version])?;
    Ok(json!({"version":version,"changed":changed,"affected":affected}))
}

pub fn rebuild(c: &Connection) -> Result<Value> {
    if !character_projection::policy(c)? {
        return Err(invalid("请先完成两级身份迁移"));
    }
    baseline(c)?;
    let g = graph(c)?;
    project(c, &g)
}
