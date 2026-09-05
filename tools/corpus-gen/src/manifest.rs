use serde::{Deserialize, Serialize};

use crate::tree::{CorpusPlan, CorpusProfile};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CorpusDescriptor {
    pub schema_version: u32,
    pub profile: CorpusProfile,
    pub seed: u64,
    pub total_logical_bytes: u64,
    pub file_count: u64,
    pub oracle_spec_hash: String,
    pub plan: CorpusPlan,
}

#[must_use]
pub fn describe(plan: &CorpusPlan) -> CorpusDescriptor {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"MirageSSD deterministic corpus v1\0");
    hasher.update(&plan.seed.to_le_bytes());
    hasher.update(format!("{:?}", plan.profile).as_bytes());
    for file in &plan.files {
        hasher.update(file.path.as_bytes());
        hasher.update(&file.logical_length.to_le_bytes());
        hasher.update(&file.seed.to_le_bytes());
        hasher.update(format!("{:?}", file.pattern).as_bytes());
    }
    CorpusDescriptor {
        schema_version: 1,
        profile: plan.profile,
        seed: plan.seed,
        total_logical_bytes: plan.total_logical_bytes,
        file_count: plan.files.len() as u64,
        oracle_spec_hash: hasher.finalize().to_hex().to_string(),
        plan: plan.clone(),
    }
}
