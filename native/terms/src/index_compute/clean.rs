use super::*;
use crate::recognition_compute::anchors;
pub(super) fn reject(review: &Path, row: &Value, reason: &str) -> Result<()> {
    let cid = field(row, "short_tag")?;
    if !safe(cid) {
        return Err(Error::invalid("索引角色ID不正确"));
    }
    let folder = review.join("char-patch/rejected");
    fs::create_dir_all(&folder)?;
    let mut meta = row.clone();
    meta["rejected"] = json!(reason);
    write(&folder.join(format!("{cid}.json")), &meta)
}
pub(super) fn drop(review: &Path, selected: &HashSet<String>, reason: &str) -> Result<()> {
    for cid in selected {
        let path = review.join(format!("char-index/{cid}.json"));
        let meta = read(&path)?;
        if meta["source"] != "danbooru-patch" {
            return Err(Error::invalid(format!("{cid} 不是补充角色，不能在这里放弃")));
        }
        reject(review, &meta, &format!("人工看图后放弃：{reason}"))?;
        fs::remove_file(path)?;
        fs::remove_file(review.join(format!("char-index/{cid}.npy")))?;
    }
    Ok(())
}
pub(super) fn existing(review: &Path, selected: &HashSet<String>) -> Result<(Vec<String>, Vec<Vec<f32>>)> {
    let (mut ids, mut vectors) = (vec![], vec![]);
    for path in files(&review.join("char-index"), "json")? {
        let Ok(meta) = read(&path) else { continue };
        let cid = meta["short_tag"]
            .as_str()
            .filter(|s| !s.is_empty())
            .unwrap_or(path.file_stem().and_then(|s| s.to_str()).ok_or_else(|| Error::invalid("索引文件名不是UTF8"))?);
        let feat = path.with_extension("npy");
        if selected.contains(cid) || !feat.try_exists()? {
            continue;
        }
        let Some(rows) = optional_reference(&feat)? else { continue };
        if rows.len() >= 3 {
            ids.push(cid.into());
            vectors.push(rows.merged()?);
        }
    }
    // 基础角色索引（#607）的锚点就是同一公式的合并结果；共享目录里同名的条目已在上面。
    let listed = files(&review.join("char-index"), "json")?
        .iter()
        .filter_map(|p| p.file_stem().and_then(|s| s.to_str()).map(String::from))
        .collect::<HashSet<_>>();
    for entry in crate::character_index::load_job(review)?.into_iter().flatten() {
        if !selected.contains(&entry.id) && !listed.contains(&entry.id) {
            ids.push(entry.id);
            vectors.push(entry.anchor);
        }
    }
    Ok((ids, vectors))
}
pub(super) fn clean(review: &Path, selected: &HashSet<String>) -> Result<()> {
    let raw = review.join("char-patch/raw");
    fs::create_dir_all(&raw)?;
    let (known_ids, known) = existing(review, selected)?;
    let mut accepted_ids: Vec<String> = vec![];
    let mut accepted: Vec<Vec<f32>> = vec![];
    let plan = read(&review.join("char-patch/plan.json"))?;
    let mut plan = plan.as_array().ok_or_else(|| Error::invalid("索引plan不正确"))?.clone();
    plan.sort_by_key(|r| std::cmp::Reverse(r["post_count"].as_i64().unwrap_or(0)));
    for row in plan {
        let cid = field(&row, "short_tag")?;
        if !selected.contains(cid) {
            continue;
        }
        let path = review.join(format!("char-index/{cid}.json"));
        if !path.try_exists()? {
            continue;
        }
        let mut meta = read(&path)?;
        if meta["source"] != "danbooru-patch" {
            continue;
        }
        let feat = path.with_extension("npy");
        let original = raw.join(format!("{cid}.npy"));
        if !original.try_exists()? {
            fs::rename(&feat, &original)?;
        }
        let rows = reference(&original)?;
        let keep = rows.core()?;
        let posts = meta["posts_all"]
            .as_array()
            .filter(|p| !p.is_empty())
            .or_else(|| meta["posts"].as_array())
            .ok_or_else(|| Error::invalid("索引参考posts不正确"))?
            .clone();
        if posts.len() != rows.len() {
            return Err(Error::invalid("索引参考向量与posts长度不匹配"));
        }
        meta["posts_all"] = json!(posts);
        let mut reason = None;
        if let Some(keep) = &keep {
            let anchor = rows.selected(keep).merged()?;
            let distance = anchors::cross(std::slice::from_ref(&anchor), &known)?.remove(0);
            let closest = distance.iter().enumerate().min_by(|(i, a), (j, b)| a.total_cmp(b).then(i.cmp(j)));
            if let Some((i, d)) = closest.filter(|(_, d)| **d < 0.03) {
                reason = Some(format!(
                    "与已有角色 {id} 几乎相同（距离 {distance}）：多为其伴生物或皮肤",
                    id = known_ids[i],
                    distance = format_args!("{d:.3}")
                ));
            } else if !accepted.is_empty() {
                let d = anchors::cross(&[anchor], &accepted)?.remove(0);
                if let Some((i, d)) = d
                    .iter()
                    .enumerate()
                    .min_by(|(i, a), (j, b)| a.total_cmp(b).then(i.cmp(j)))
                    .filter(|(_, d)| **d < 0.03)
                {
                    reason = Some(format!(
                        "与补充角色 {id} 是同一人（距离 {distance}）：变身前后、本体与皮肤等，并入其别名",
                        id = accepted_ids[i],
                        distance = format_args!("{d:.3}")
                    ));
                    meta["same_as"] = json!(accepted_ids[i]);
                }
            }
        } else {
            reason = Some("参考图没有一致的主体".into());
        }
        if let Some(reason) = reason {
            reject(review, &meta, &reason)?;
            if feat.try_exists()? {
                fs::remove_file(&feat)?;
            }
            fs::remove_file(&path)?;
            continue;
        }
        let keep = keep.ok_or_else(|| Error::invalid("清洗核心缺失"))?;
        let selected = rows.selected(&keep);
        accepted.push(selected.merged()?);
        accepted_ids.push(cid.into());
        meta["posts"] = json!(keep.iter().map(|&i| &posts[i]).collect::<Vec<_>>());
        meta["refs_total"] = json!(rows.len());
        meta["refs_kept"] = json!(keep.len());
        save(&feat, &selected.raw32())?;
        let gender_keys: Vec<_> = build::ordered_gender(&path)?.into_iter().map(|(key, _)| key).collect();
        write_meta(&path, &meta, &gender_keys)?;
    }
    Ok(())
}
