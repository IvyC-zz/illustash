//! One authoritative resource/model path policy for production and import.
use crate::{Error, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};
pub struct ModelFiles {
    pub root: PathBuf,
    tags: PathBuf,
}
impl ModelFiles {
    pub fn resolve(resources: &Path) -> Result<Self> {
        Self::configured(resources, std::env::var_os("ILLUSTASH_MODELS").as_deref().map(Path::new))
    }
    pub(crate) fn configured(resources: &Path, configured: Option<&Path>) -> Result<Self> {
        let root = match configured {
            Some(path) if path.is_absolute() => path.to_owned(),
            Some(path) => resources.join(path),
            None => resources.join("models"),
        };
        Self::at(resources, &root)
    }
    pub fn at(resources: &Path, root: &Path) -> Result<Self> {
        Ok(Self {
            root: crate::app_paths::canonical(root)?,
            tags: crate::app_paths::canonical(&resources.join("tags/tags_v3.csv"))?,
        })
    }
    pub fn specs(&self, check: &dyn Fn() -> Result<()>) -> Result<Value> {
        Ok(
            json!({"wd":{"version":"swinv2_448","dimension":1024,"sha256":hash(&self.root.join("wd_swinv2_v3_emb.onnx"),check)?,"tags_sha256":hash(&self.tags,check)?},"ccip":{"version":"ccip-model_feat","dimension":768,"sha256":hash(&self.root.join("ccip/model_feat.onnx"),check)?}}),
        )
    }
}
pub(crate) fn hash(path: &Path, check: &dyn Fn() -> Result<()>) -> Result<String> {
    let mut file =
        fs::File::open(path).map_err(|e| Error::Invalid(format!("模型/锚点 {path}: {e}", path = path.display())))?;
    let mut digest = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        check()?;
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        digest.update(&buffer[..n]);
    }
    Ok(format!("{:x}", digest.finalize()))
}
