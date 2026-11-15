//! 便携 hybrid 身份子图的结构验证；顺序、代表及层级约束与 character_graph::graph 相同。
use super::format::{Policy, Record, Snapshot};
use crate::{error::invalid, Result};
use std::collections::BTreeMap;

fn root(parents: &mut [usize], mut i: usize) -> usize {
    while parents[i] != i {
        parents[i] = parents[parents[i]];
        i = parents[i];
    }
    i
}

pub(super) fn validate(snapshot: &Snapshot) -> Result<()> {
    if snapshot.header.policy != Policy::Hybrid {
        return Ok(());
    }
    let mut nodes: Vec<_> = snapshot
        .records
        .iter()
        .filter_map(|r| match r {
            Record::Entity { key, definition } => definition.baseline.as_ref().map(|b| (key.as_str(), b)),
            _ => None,
        })
        .collect();
    nodes.sort_by_key(|(_, b)| b.order);
    let index: BTreeMap<_, _> = nodes.iter().enumerate().map(|(i, (key, _))| (*key, i)).collect();
    let mut relations = Vec::new();
    for r in &snapshot.records {
        if let Record::RelationVerdict { a, b, kind, order, identity_relation: true, decision, .. } = r {
            let (Some(&a), Some(&b)) = (index.get(a.as_str()), index.get(b.as_str())) else {
                return invalid("身份关系缺少原始基线");
            };
            if kind == "excludes" {
                return invalid("excludes 不属于身份图关系");
            }
            if decision.verdict == "yes" {
                relations.push((*order, a, b, kind.as_str()));
            }
        }
    }
    relations.sort_by_key(|r| r.0);
    let mut parents: Vec<_> = (0..nodes.len()).collect();
    for (i, (_, node)) in nodes.iter().enumerate() {
        if let Some(j) = node.merged_into.as_deref().and_then(|key| index.get(key)) {
            let a = root(&mut parents, i);
            let b = root(&mut parents, *j);
            parents[a] = b;
        }
    }
    for &(_, a, b, kind) in &relations {
        if kind == "same_as" {
            let a = root(&mut parents, a);
            let b = root(&mut parents, b);
            parents[a] = b;
        }
    }
    let mapping: Vec<_> = (0..nodes.len()).map(|i| root(&mut parents, i)).collect();
    let mut hierarchy = vec![None; nodes.len()];
    let mut link_parent = |child: usize, parent: usize| -> Result<()> {
        let (child, parent) = (mapping[child], mapping[parent]);
        if child == parent || hierarchy[child].is_some_and(|old| old != parent) {
            return invalid("身份关系包含自身父级或多个父级");
        }
        hierarchy[child] = Some(parent);
        Ok(())
    };
    for (i, (_, node)) in nodes.iter().enumerate() {
        if let Some(key) = node.parent.as_deref() {
            let Some(&parent) = index.get(key) else { return invalid("身份父级缺少原始基线") };
            link_parent(i, parent)?;
        }
    }
    for &(_, a, b, kind) in &relations {
        if kind == "contains" {
            link_parent(b, a)?;
        }
    }
    // 文件不可信；每个节点只走一次，长父子链不能使格式校验退化成平方工作量。
    let mut families = vec![None; nodes.len()];
    let mut visiting = vec![false; nodes.len()];
    for &mapped in &mapping {
        let mut i = mapped;
        let mut path = Vec::new();
        let family = loop {
            if let Some(family) = families[i] {
                break family;
            }
            if visiting[i] {
                return invalid("身份关系存在层级环");
            }
            visiting[i] = true;
            path.push(i);
            match hierarchy[i] {
                Some(parent) => i = parent,
                None => break i,
            }
        };
        for node in path {
            families[node] = Some(family);
        }
    }
    for &(_, a, b, kind) in &relations {
        if kind == "different_from" && families[mapping[a]] == families[mapping[b]] {
            return invalid("确认不同与身份等价组或层级冲突");
        }
    }
    Ok(())
}
