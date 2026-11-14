//! WD 的 Core ML 执行文件与原始模型分开；清单固定两者的关系，结果身份始终取原始模型。
use super::{
    backend::{Backend, Model},
    models::hash,
};
use crate::{app_models::WdCoreMl, Error, Result};
use std::path::{Path, PathBuf};

pub(super) fn execution_path(original: &Path, model: Model, backend: Backend) -> Result<PathBuf> {
    if model != Model::Wd || backend != Backend::CoreMl {
        return Ok(original.to_owned());
    }
    let variant = crate::app_models::manifest()
        .wd_coreml
        .as_ref()
        .ok_or_else(|| Error::invalid("此构建没有 WD Core ML 执行文件清单"))?;
    checked(original, variant)
}

fn checked(original: &Path, variant: &WdCoreMl) -> Result<PathBuf> {
    if variant.revision != 1 || hash(original, &|| Ok(()))? != variant.source_sha256 {
        return Err(Error::invalid("WD 原始模型与 Core ML 执行文件版本不匹配，继续使用原始模型 CPU"));
    }
    let file = original.parent().ok_or_else(|| Error::invalid("WD 模型路径缺少父目录"))?.join(&variant.file.path);
    let failed = || Error::invalid("WD 加速文件缺失或损坏；在设置的识别模型中重新下载或导入加速文件");
    if !std::fs::metadata(&file).is_ok_and(|m| m.is_file() && m.len() == variant.file.size)
        || hash(&file, &|| Ok(())).map_err(|_| failed())? != variant.file.sha256
    {
        return Err(failed());
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn variant_requires_matching_original_revision_and_complete_file() {
        let root = std::env::temp_dir().join(format!("illustash-wd-variant-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let original = root.join("original.onnx");
        let derived = root.join("derived.onnx");
        fs::write(&original, b"original").unwrap();
        fs::write(&derived, b"derived").unwrap();
        let mut variant: WdCoreMl = serde_json::from_value(serde_json::json!({
            "revision": 1, "source_sha256": hash(&original, &|| Ok(())).unwrap(),
            "file": {"path": "derived.onnx", "size": 7,
                "sha256": hash(&derived, &|| Ok(())).unwrap(), "url": "https://example.invalid/derived.onnx"}
        }))
        .unwrap();
        assert_eq!(checked(&original, &variant).unwrap(), derived);
        variant.revision = 2;
        assert!(checked(&original, &variant).is_err());
        variant.revision = 1;
        fs::write(&original, b"different").unwrap();
        assert!(checked(&original, &variant).is_err());
        fs::write(&original, b"original").unwrap();
        for invalid in [b"damaged".as_slice(), b"short"] {
            fs::write(&derived, invalid).unwrap();
            assert!(checked(&original, &variant).is_err());
        }
        fs::remove_file(&derived).unwrap();
        assert!(checked(&original, &variant).is_err());
        // CPU 与 DirectML 不依赖变体，原始文件无需符合内置模型哈希。
        for backend in [Backend::Cpu, Backend::DirectMl] {
            assert_eq!(execution_path(&original, Model::Wd, backend).unwrap(), original);
        }
        assert_eq!(execution_path(&original, Model::Ccip, Backend::CoreMl).unwrap(), original);
        fs::remove_dir_all(root).unwrap();
    }
}
