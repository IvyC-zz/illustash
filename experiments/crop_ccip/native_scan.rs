//! 用生产索引的最近 9 个锚点、别名跳转和阈值，扫描公开实验向量。
use illustash_terms::index_compute::numeric;
use illustash_terms::recognition_compute::anchors::{self, Metadata};
use serde::Deserialize;
use std::{
    collections::{HashMap, HashSet},
    env, fs,
    io::{BufWriter, Write},
    path::Path,
};

const DIM: usize = 768;

#[derive(Deserialize)]
struct Entry {
    id: String,
    p50: f32,
    p75: f32,
    p90: f32,
    skin: bool,
    post_count: i64,
    gender_conf: f64,
    patched: bool,
}

fn matrix(path: &Path) -> Result<Vec<Vec<f32>>, Box<dyn std::error::Error>> {
    let data = fs::read(path)?;
    if data.len() % (DIM * 4) != 0 {
        return Err("向量文件长度不符".into());
    }
    Ok(data
        .chunks_exact(DIM * 4)
        .map(|row| {
            row.chunks_exact(4)
                .map(|value| f32::from_le_bytes(value.try_into().expect("每个 f32 为 4 字节")))
                .collect()
        })
        .collect())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = env::args_os().skip(1).collect();
    let [index, feature_path, output_path] = args.as_slice() else {
        return Err("用法：驱动 索引目录 测试向量.f32 最近锚点.tsv".into());
    };
    let folder = Path::new(index);
    let entries: Vec<Entry> = serde_json::from_slice(&fs::read(folder.join("entries.json"))?)?;
    let ids: Vec<_> = entries.iter().map(|r| r.id.clone()).collect();
    let vectors = matrix(&folder.join("anchors.f32"))?;
    let features = matrix(Path::new(feature_path))?;
    let metadata: HashMap<_, _> = entries
        .iter()
        .map(|row| {
            (
                row.id.clone(),
                Metadata {
                    p50: row.p50,
                    p75: row.p75,
                    p90: row.p90,
                    skin: row.skin,
                    post_count: row.post_count,
                    gender_conf: row.gender_conf,
                },
            )
        })
        .collect();
    let patches: HashSet<_> = entries
        .iter()
        .filter(|r| r.patched)
        .map(|r| r.id.clone())
        .collect();
    let index = anchors::anchor_index(&vectors, &ids, &metadata)?;
    let limits = anchors::claim_thresholds(&ids, &index, &patches)?;
    let mut output = BufWriter::new(fs::File::create(output_path)?);
    let mut live_difference_count = 0;
    let mut live_images = 0;
    for (chunk, group) in features.chunks(4000).enumerate() {
        let distances = anchors::cross(&vectors, group)?;
        for (i, feature) in group.iter().enumerate() {
            let index_id = chunk * 4000 + i;
            let col: Vec<_> = distances.iter().map(|row| row[i]).collect();
            let mut seen = HashSet::new();
            let mut scan_claims = HashSet::new();
            for anchor in numeric::nearest_nine(&col) {
                let entity = &index.entities[anchor];
                if !seen.insert(entity) {
                    continue;
                }
                let distance = col[anchor];
                let threshold = limits[anchor];
                if threshold > 0. && distance <= threshold {
                    scan_claims.insert(entity.as_str());
                }
                writeln!(
                    output,
                    "{}\t{}\t{}\t{}\t{}\t{:.4}",
                    index_id,
                    ids[anchor],
                    entity,
                    distance,
                    threshold,
                    f64::from(distance)
                )?;
            }
            // 每 101 张核对一次单图识别的直接调用。旧库全量重打分沿用上面的扫描入口。
            if index_id % 101 == 0 {
                live_images += 1;
                let live = anchors::claims("public", &vectors, feature, &index, &limits)?;
                let live_claims: HashSet<_> =
                    live.iter().map(|claim| claim.character.as_str()).collect();
                live_difference_count += usize::from(live_claims != scan_claims);
            }
        }
    }
    output.flush()?;
    let receipt = serde_json::json!({
        "anchors": ids.len(),
        "features": features.len(),
        "alias_redirected": ids.iter().zip(&index.entities).filter(|(a, b)| a != b).count(),
        "live_checked": live_images,
        "live_claim_difference_images": live_difference_count
    });
    fs::write(
        Path::new(output_path).with_extension("json"),
        serde_json::to_vec_pretty(&receipt)?,
    )?;
    Ok(())
}
