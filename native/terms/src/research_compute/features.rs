//! 研究部分负责的特征加载。识别提供原始的小端 float32 向量。
use super::numeric;
use crate::{Error, Result};
use rusqlite::OpenFlags;
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::Path,
};

pub fn bytes(row: &[f32]) -> Vec<u8> {
    row.iter().flat_map(|v| v.to_le_bytes()).collect()
}
pub fn decode(blob: &[u8]) -> Result<Vec<f32>> {
    if blob.is_empty() || blob.len() % 4 != 0 {
        return Err(Error::invalid("Invalid CCIP feature vector"));
    }
    Ok(blob.chunks_exact(4).map(|b| f32::from_le_bytes(b.try_into().expect("four bytes"))).collect())
}

#[derive(Default)]
pub struct Matrix {
    pub uuids: Vec<String>,
    pub rows: Vec<Vec<f32>>,
    pub row: HashMap<String, usize>,
    pub versions: HashMap<String, Vec<u8>>,
    pub dimension: usize,
}
impl Matrix {
    pub fn new(uuids: Vec<String>, mut rows: Vec<Vec<f32>>, dimension: usize) -> Result<Self> {
        if dimension == 0
            || uuids.len() != rows.len()
            || uuids.iter().collect::<HashSet<_>>().len() != uuids.len()
            || rows.iter().any(|v| v.len() != dimension)
        {
            return Err(Error::invalid("Feature manifest/matrix mismatch or duplicate UUID"));
        }
        for v in &mut rows {
            numeric::normalise(v).map_err(|m| Error::invalid(&m))?;
        }
        Ok(Self::normalised(uuids, rows, dimension))
    }
    fn normalised(uuids: Vec<String>, rows: Vec<Vec<f32>>, dimension: usize) -> Self {
        let row = uuids.iter().enumerate().map(|(i, u)| (u.clone(), i)).collect();
        let versions = uuids.iter().zip(&rows).map(|(u, v)| (u.clone(), Sha256::digest(bytes(v)).to_vec())).collect();
        Self { uuids, rows, row, versions, dimension }
    }
    pub fn matrix(&self, ids: &[String]) -> (Vec<String>, Vec<Vec<f32>>) {
        let mut known = vec![];
        let mut rows = vec![];
        for u in ids {
            if let Some(&i) = self.row.get(u) {
                known.push(u.clone());
                rows.push(self.rows[i].clone());
            }
        }
        (known, rows)
    }
}

/// 为这个一次性 worker 加载一次磁盘上的基础矩阵和 SQLite 里的 CCIP 覆盖层。
/// 每个研究进程最多加载一次，所以没有跨调用的缓存（#32）。
/// 所有读取和校验都在返回矩阵之前完成；任何一行无效都会让加载失败。
pub fn load(source: &Path, base: &Path) -> Result<Matrix> {
    let c = crate::sqlite::open_with_flags(source, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    load_from(&c, base)
}

pub(crate) fn load_from(c: &rusqlite::Connection, base: &Path) -> Result<Matrix> {
    let paths = [base.join("full-manifest.csv"), base.join("full-features.npy")];
    let present = paths
        .iter()
        .map(|p| match fs::metadata(p) {
            Ok(_) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        })
        .collect::<std::io::Result<Vec<_>>>()?;
    let exists: bool =
        c.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='embeddings' AND type='table')", [], |r| {
            r.get(0)
        })?;
    let overlay: Vec<(String, Vec<u8>)> = if exists {
        c.prepare("SELECT uuid,vec FROM embeddings WHERE model='ccip'")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?
    } else {
        vec![]
    };
    let mut matrix = match present[..] {
        [true, true] => {
            let mut csv = csv::Reader::from_path(&paths[0]).map_err(|e| Error::invalid(e.to_string()))?;
            let col = csv
                .headers()
                .map_err(|e| Error::invalid(e.to_string()))?
                .iter()
                .position(|h| h == "uuid")
                .ok_or_else(|| Error::invalid("Feature manifest missing uuid"))?;
            let uuids = csv
                .records()
                .map(|r| r.map(|r| r.get(col).unwrap_or("").trim().to_uppercase()))
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|e| Error::invalid(e.to_string()))?;
            read_npy(&paths[1])?.into_matrix(uuids)?
        }
        [false, false] => Matrix { dimension: 768, ..Matrix::default() },
        _ => return Err(Error::invalid("Both feature matrix and manifest are required")),
    };
    for (u, raw) in overlay {
        let mut v = decode(&raw)?;
        numeric::normalise(&mut v).map_err(|m| Error::invalid(&m))?;
        if v.len() != matrix.dimension {
            return Err(Error::invalid("CCIP feature dimension mismatch"));
        }
        let index = if let Some(&i) = matrix.row.get(&u) {
            i
        } else {
            let i = matrix.rows.len();
            matrix.uuids.push(u.clone());
            matrix.rows.push(vec![]);
            matrix.row.insert(u.clone(), i);
            i
        };
        matrix.versions.insert(u, Sha256::digest(bytes(&v)).to_vec());
        matrix.rows[index] = v;
    }
    Ok(matrix)
}

// 在参考实现按行求范数并相除之前，保持原来的 dtype 和布局。
// F 顺序且有多行时，沿轴 1 的归约是跨步的，不是两两求和。
pub struct NpyMatrix {
    pub(crate) dimension: usize,
    pub(crate) fortran_order: bool,
    pub(crate) values: NpyValues,
}
pub(crate) enum NpyValues {
    Float32(Vec<Vec<f32>>),
    Float64(Vec<Vec<f64>>),
}
impl NpyMatrix {
    pub fn len(&self) -> usize {
        match &self.values {
            NpyValues::Float32(rows) => rows.len(),
            NpyValues::Float64(rows) => rows.len(),
        }
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub(super) fn into_anchor_matrices(mut self, ids: Vec<String>) -> Result<(Matrix, Matrix)> {
        // 录制的别名距离算法先把原始别名输入转成连续的 f32；
        // patch_anchors 则先选行，按原 dtype 归一化，再转换类型。
        let alias_rows = match &self.values {
            NpyValues::Float32(rows) => rows.clone(),
            NpyValues::Float64(rows) => rows.iter().map(|r| r.iter().map(|&x| x as f32).collect()).collect(),
        };
        let aliases = Matrix::new(ids.clone(), alias_rows, self.dimension)?;
        self.fortran_order = false; // NumPy 的高级行索引 A[keep] 得到 C 顺序的行。
        Ok((aliases, self.into_matrix(ids)?))
    }
    pub fn into_matrix(self, uuids: Vec<String>) -> Result<Matrix> {
        if uuids.len() != self.len() || uuids.iter().collect::<HashSet<_>>().len() != uuids.len() {
            return Err(Error::invalid("Feature manifest/matrix mismatch or duplicate UUID"));
        }
        let contiguous = !self.fortran_order || self.len() <= 1 || self.dimension <= 1;
        let rows = match self.values {
            NpyValues::Float32(mut rows) => {
                for row in &mut rows {
                    numeric::normalise_layout(row, contiguous).map_err(|m| Error::invalid(&m))?;
                }
                rows
            }
            NpyValues::Float64(rows) => rows
                .into_iter()
                .map(|row| numeric::normalise64(&row, contiguous).map_err(|m| Error::invalid(&m)))
                .collect::<Result<_>>()?,
        };
        Ok(Matrix::normalised(uuids, rows, self.dimension))
    }
}

/// 有界的 NPY 解析器，用于实数 float32/float64 矩阵，包括字节序和 C/F 布局。
/// 其他 dtype 显式拒绝；不做 pickle，也不求值表达式。
pub fn read_npy(path: &Path) -> Result<NpyMatrix> {
    let raw = fs::read(path)?;
    let bad = || Error::invalid("Invalid CCIP NPY matrix");
    if raw.get(..6) != Some(b"\x93NUMPY") {
        return Err(bad());
    }
    let (start, len): (usize, usize) = match raw.get(6..8) {
        Some([1, 0]) if raw.len() >= 10 => (10, u16::from_le_bytes(raw[8..10].try_into().expect("two bytes")) as usize),
        Some([2, 0]) | Some([3, 0]) if raw.len() >= 12 => {
            (12, u32::from_le_bytes(raw[8..12].try_into().expect("four bytes")) as usize)
        }
        _ => return Err(bad()),
    };
    let end = start.checked_add(len).ok_or_else(bad)?;
    let header = raw.get(start..end).and_then(|b| std::str::from_utf8(b).ok()).ok_or_else(bad)?;
    let token =
        regex::Regex::new(r#"\s*(?:'([^'\\]*)'|"([^"\\]*)"|([A-Za-z0-9<>|=]+)|([{}():,]))"#).expect("fixed NPY tokens");
    let mut tokens = vec![];
    let mut consumed = 0;
    for cap in token.captures_iter(header) {
        let all = cap.get(0).expect("matched token");
        if all.start() != consumed {
            return Err(bad());
        }
        consumed = all.end();
        let (quoted, s) = if let Some(s) = cap.get(1).or_else(|| cap.get(2)) {
            (true, s.as_str())
        } else {
            (false, cap.get(3).or_else(|| cap.get(4)).expect("token").as_str())
        };
        tokens.push((quoted, s));
    }
    if !header[consumed..].trim().is_empty() {
        return Err(bad());
    }
    let mut cursor = 0;
    let take = |cursor: &mut usize| -> Result<(bool, &str)> {
        let v = tokens.get(*cursor).copied().ok_or_else(bad)?;
        *cursor += 1;
        Ok(v)
    };
    if take(&mut cursor)? != (false, "{") {
        return Err(bad());
    }
    let (mut dtype, mut layout, mut shape) = (None, None, None);
    let mut keys = HashSet::new();
    while tokens.get(cursor) != Some(&(false, "}")) {
        let (quoted, key) = take(&mut cursor)?;
        if !quoted || !keys.insert(key) || take(&mut cursor)? != (false, ":") {
            return Err(bad());
        }
        match key {
            "descr" => {
                let (q, v) = take(&mut cursor)?;
                if !q {
                    return Err(bad());
                }
                dtype = Some(v);
            }
            "fortran_order" => {
                layout = Some(match take(&mut cursor)? {
                    (false, "False") => false,
                    (false, "True") => true,
                    _ => return Err(bad()),
                });
            }
            "shape" => {
                if take(&mut cursor)? != (false, "(") {
                    return Err(bad());
                }
                let (q, n) = take(&mut cursor)?;
                let n = n.parse::<usize>().map_err(|_| bad())?;
                if q || take(&mut cursor)? != (false, ",") {
                    return Err(bad());
                }
                let (q, d) = take(&mut cursor)?;
                let d = d.parse::<usize>().map_err(|_| bad())?;
                if q {
                    return Err(bad());
                }
                if tokens.get(cursor) == Some(&(false, ",")) {
                    cursor += 1;
                }
                if take(&mut cursor)? != (false, ")") {
                    return Err(bad());
                }
                shape = Some((n, d));
            }
            _ => return Err(bad()),
        }
        if tokens.get(cursor) == Some(&(false, ",")) {
            cursor += 1;
        } else if tokens.get(cursor) != Some(&(false, "}")) {
            return Err(bad());
        }
    }
    cursor += 1;
    if cursor != tokens.len() || keys.len() != 3 {
        return Err(bad());
    }
    let (count, dim) = shape.ok_or_else(bad)?;
    let (big, width) = match dtype.ok_or_else(bad)? {
        "<f4" => (false, 4),
        "=f4" => (cfg!(target_endian = "big"), 4),
        ">f4" => (true, 4),
        "<f8" => (false, 8),
        "=f8" => (cfg!(target_endian = "big"), 8),
        ">f8" => (true, 8),
        _ => return Err(Error::invalid("CCIP NPY dtype must be real float32 or float64")),
    };
    if dim == 0 || count.checked_mul(dim).and_then(|n| n.checked_mul(width)) != Some(raw.len() - end) {
        return Err(bad());
    }
    let fortran_order = layout.ok_or_else(bad)?;
    let offset = |i: usize, j: usize| end + (if fortran_order { j * count + i } else { i * dim + j }) * width;
    let values = if width == 4 {
        let mut rows = vec![vec![0.0; dim]; count];
        for (i, row) in rows.iter_mut().enumerate() {
            for (j, value) in row.iter_mut().enumerate() {
                let at = offset(i, j);
                let bytes = raw[at..at + 4].try_into().expect("four bytes");
                *value = if big { f32::from_be_bytes(bytes) } else { f32::from_le_bytes(bytes) };
            }
        }
        NpyValues::Float32(rows)
    } else {
        let mut rows = vec![vec![0.0; dim]; count];
        for (i, row) in rows.iter_mut().enumerate() {
            for (j, value) in row.iter_mut().enumerate() {
                let at = offset(i, j);
                let bytes = raw[at..at + 8].try_into().expect("eight bytes");
                *value = if big { f64::from_be_bytes(bytes) } else { f64::from_le_bytes(bytes) };
            }
        }
        NpyValues::Float64(rows)
    };
    Ok(NpyMatrix { dimension: dim, fortran_order, values })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn npy_precision_layout_endian_and_versions_match_fixed_numpy_reference() {
        // 实际的 NumPy 1.26.4 和 2.5.3 参考：两行 [1, 7*2^-12, 760*0]。
        // F 顺序的 float32 范数是 1；C 顺序的范数是 1.0000001。哈希针对归一化后的字节。
        let root = std::env::temp_dir().join(format!("illustash-npy-test-{}", crate::store::new_key().unwrap()));
        fs::create_dir(&root).unwrap();
        let path = root.join("features.npy");
        for major in [1u8, 2, 3] {
            for dtype in ["<f4", ">f4", "<f8", ">f8"] {
                for fortran in [false, true] {
                    let header = format!(
                        "{{'descr': '{dtype}', 'fortran_order': {}, 'shape': (2, 768), }}\n",
                        if fortran { "True" } else { "False" }
                    );
                    let mut file = b"\x93NUMPY".to_vec();
                    file.extend([major, 0]);
                    if major == 1 {
                        file.extend((header.len() as u16).to_le_bytes());
                    } else {
                        file.extend((header.len() as u32).to_le_bytes());
                    }
                    file.extend(header.bytes());
                    for k in 0..1536 {
                        let column = if fortran { k / 2 } else { k % 768 };
                        let value: f64 = if column == 0 {
                            1.0
                        } else if column < 8 {
                            2f64.powi(-12)
                        } else {
                            0.0
                        };
                        let big = dtype.starts_with('>');
                        if dtype.ends_with('4') {
                            file.extend(if big { (value as f32).to_be_bytes() } else { (value as f32).to_le_bytes() });
                        } else {
                            file.extend(if big { value.to_be_bytes() } else { value.to_le_bytes() });
                        }
                    }
                    fs::write(&path, file).unwrap();
                    let matrix = read_npy(&path).unwrap().into_matrix(vec!["A".into(), "B".into()]).unwrap();
                    let expected = if dtype.ends_with('8') {
                        "e92aee22f3419e2fb1be9f8e37e24ac0bba393f246aa495d2944a09816bdef2e"
                    } else if fortran {
                        "6ea16ed82a6eb94a465a9a6c6aabc9fe0e74d8697677396b7289d0279367deef"
                    } else {
                        "def732b18efcd370ca1bfac71f1c8141ea68432c81680025f10add4ce8f40a4a"
                    };
                    assert_eq!(
                        format!("{:x}", Sha256::digest(bytes(&matrix.rows[0]))),
                        expected,
                        "v{major} {dtype} F={fortran}"
                    );
                    assert_eq!(matrix.versions["A"], matrix.versions["B"]);
                    assert_eq!(matrix.versions["A"], Sha256::digest(bytes(&matrix.rows[0])).to_vec());
                    let (aliases, selected) =
                        read_npy(&path).unwrap().into_anchor_matrices(vec!["A".into(), "B".into()]).unwrap();
                    assert_eq!(
                        format!("{:x}", Sha256::digest(bytes(&aliases.rows[0]))),
                        "def732b18efcd370ca1bfac71f1c8141ea68432c81680025f10add4ce8f40a4a"
                    );
                    let expected_selected = if dtype.ends_with('8') {
                        "e92aee22f3419e2fb1be9f8e37e24ac0bba393f246aa495d2944a09816bdef2e"
                    } else {
                        "def732b18efcd370ca1bfac71f1c8141ea68432c81680025f10add4ce8f40a4a"
                    };
                    assert_eq!(format!("{:x}", Sha256::digest(bytes(&selected.rows[0]))), expected_selected);
                }
            }
        }
        // 超出 f32 范围的有限 f64 向量必须先归一化再转换类型。
        let raw = NpyMatrix { dimension: 2, fortran_order: false, values: NpyValues::Float64(vec![vec![1e40, 0.0]]) };
        assert_eq!(raw.into_matrix(vec!["A".into()]).unwrap().rows, vec![vec![1.0, 0.0]]);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn injected_validation_and_versions() {
        assert!(Matrix::new(vec!["A".into(), "A".into()], vec![vec![1.0, 0.0]; 2], 2).is_err());
        for v in [vec![0.0, 0.0], vec![f32::NAN, 1.0]] {
            assert!(Matrix::new(vec!["A".into()], vec![v], 2).is_err());
        }
        let a = Matrix::new(vec!["A".into()], vec![vec![2.0, 0.0]], 2).unwrap();
        let b = Matrix::new(vec!["A".into()], vec![vec![0.0, 1.0]], 2).unwrap();
        assert_ne!(a.versions, b.versions);
        assert_eq!(a.matrix(&["missing".into(), "A".into()]).0, vec!["A"]);
    }
    #[test]
    fn base_overlay_failure_and_deletion() {
        let root = std::env::temp_dir().join(format!("illustash-feature-test-{}", crate::store::new_key().unwrap()));
        fs::create_dir(&root).unwrap();
        let source = root.join("source.sqlite");
        let c = crate::sqlite::open(&source).unwrap();
        // 没有基础文件也没有覆盖层：一个生产宽度的空矩阵。
        let empty = load(&source, &root).unwrap();
        assert_eq!((empty.dimension, empty.rows.len()), (768, 0));
        c.execute_batch("CREATE TABLE embeddings(uuid,model,vec,PRIMARY KEY(uuid,model));").unwrap();
        fs::write(root.join("full-manifest.csv"), "uuid\nA\n").unwrap();
        assert!(load(&source, &root).is_err(), "manifest without matrix");
        let path = root.join("full-features.npy");
        let write_base = |row: &[f32]| {
            let header = "{'descr': '<f4', 'fortran_order': False, 'shape': (1, 2), }\n";
            let mut file = b"\x93NUMPY\x01\x00".to_vec();
            file.extend((header.len() as u16).to_le_bytes());
            file.extend(header.bytes());
            file.extend(bytes(row));
            fs::write(&path, file).unwrap();
        };
        write_base(&[0.0, -1.0]);
        let base = load(&source, &root).unwrap();
        assert_eq!(base.rows, vec![vec![0.0, -1.0]]);
        let version = base.versions["A"].clone();
        write_base(&[1.0, 0.0]);
        let replaced = load(&source, &root).unwrap();
        assert_eq!(replaced.rows, vec![vec![1.0, 0.0]]);
        assert_ne!(version, replaced.versions["A"]);
        // 无效的覆盖层让整个加载失败，包括范数溢出的有限行（#45）。
        for bad in [bytes(&[f32::NAN, 1.0]), bytes(&[1e30, 1e30]), vec![0, 0, 0], bytes(&[1.0, 0.0, 0.0])] {
            c.execute("INSERT OR REPLACE INTO embeddings VALUES ('B','ccip',?)", [bad]).unwrap();
            assert!(load(&source, &root).is_err());
        }
        // 覆盖层按 UUID 替换基础行，追加新的 UUID，删除后就消失。
        c.execute("UPDATE embeddings SET uuid='A', vec=?", [bytes(&[0.0, 3.0])]).unwrap();
        c.execute("INSERT INTO embeddings VALUES ('B','ccip',?)", [bytes(&[4.0, 0.0])]).unwrap();
        c.execute("INSERT INTO embeddings VALUES ('C','other',?)", [bytes(&[f32::NAN])]).unwrap();
        let merged = load(&source, &root).unwrap();
        assert_eq!(merged.uuids, vec!["A", "B"]);
        assert_eq!(merged.rows, vec![vec![0.0, 1.0], vec![1.0, 0.0]]);
        assert_eq!(merged.versions["B"], Sha256::digest(bytes(&[1.0, 0.0])).to_vec());
        c.execute("DELETE FROM embeddings WHERE model='ccip'", []).unwrap();
        assert_eq!(load(&source, &root).unwrap().rows, vec![vec![1.0, 0.0]]);
        drop(c);
        fs::remove_dir_all(root).unwrap();
    }
}
