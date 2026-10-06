//! Internal worker sidecar, separate from the public WD recognition/1 package.
use super::{
    anchor_snapshot::{Fingerprint, Snapshot},
    anchors::Claim,
};
use crate::{app_tasks::TaskIdentity, Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Claims {
    pub format: String,
    pub identity: TaskIdentity,
    pub assets: Vec<String>,
    pub models: Value,
    pub anchors: Fingerprint,
    pub claims: Vec<Claim>,
}
impl Claims {
    pub fn new(
        identity: TaskIdentity,
        assets: Vec<String>,
        models: Value,
        anchors: Fingerprint,
        claims: Vec<Claim>,
    ) -> Self {
        Self { format: "illustash-recognition-claims/1".into(), identity, assets, models, anchors, claims }
    }
    pub fn validate(
        &self,
        identity: &TaskIdentity,
        assets: &[String],
        models: &Value,
        snapshot: &Snapshot,
        ccip: &[u8],
        check: &dyn Fn() -> Result<()>,
    ) -> Result<()> {
        let invalid = || Error::Invalid("角色认领 sidecar 身份、范围、模型、锚点版本或计算不匹配".into());
        if self.format != "illustash-recognition-claims/1"
            || &self.identity != identity
            || self.assets != assets
            || &self.models != models
            || self.claims.len() > assets.len() * 9
        {
            return Err(invalid());
        }
        check()?;
        if self.anchors != snapshot.fingerprint || ccip.len() != assets.len() * 768 * 4 {
            return Err(invalid());
        }
        let mut unique = HashSet::new();
        if self.claims.iter().any(|c| {
            !assets.contains(&c.asset)
                || !unique.insert((&c.asset, &c.character))
                || !c.distance.is_finite()
                || !c.threshold.is_finite()
                || !c.confidence.is_finite()
        }) {
            return Err(invalid());
        }
        let mut expected = Vec::new();
        for (id, row) in assets.iter().zip(ccip.chunks_exact(768 * 4)) {
            check()?;
            let feature = row
                .chunks_exact(4)
                .map(|bytes| f32::from_le_bytes(bytes.try_into().expect("four-byte chunk")))
                .collect::<Vec<_>>();
            expected.extend(snapshot.claims_checked(id, &feature, check)?);
        }
        // Scores retain CCIP's dimension (1-distance), including f32 cosine rounding.
        // They are not WD probabilities and are never rounded/clamped to pretend to be one.
        if serde_json::to_value(&expected).map_err(|_| invalid())?
            != serde_json::to_value(&self.claims).map_err(|_| invalid())?
        {
            return Err(invalid());
        }
        Ok(())
    }
}
