use mirage_types::{CapsuleId, GenerationId, MirageError, RepositoryId};
use roaring::RoaringBitmap;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ProfileKey(pub String);
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RiskEstimate {
    pub held_out_violation_millionths: u32,
    pub unseen_branch_mass_millionths: u32,
    pub data_quality_millionths: u32,
    pub version_transfer_confidence_millionths: u32,
}
impl RiskEstimate {
    pub fn validate(self) -> Result<(), MirageError> {
        if [
            self.held_out_violation_millionths,
            self.unseen_branch_mass_millionths,
            self.data_quality_millionths,
            self.version_transfer_confidence_millionths,
        ]
        .into_iter()
        .any(|v| v > 1_000_000)
        {
            Err(MirageError::invalid_argument(
                "capsule risk value exceeds one",
            ))
        } else {
            Ok(())
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasonKind {
    HardSet,
    RecentUnion,
    Transition,
    ScanMap,
    Manual,
    Safety,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClusterReason {
    pub cluster_id: u64,
    pub kind: ReasonKind,
    pub pages: RoaringBitmap,
    pub evidence_millionths: u32,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapsulePlan {
    pub capsule_id: CapsuleId,
    pub repository_id: RepositoryId,
    pub generation: GenerationId,
    pub profile_key: ProfileKey,
    pub page_set: RoaringBitmap,
    pub mandatory_set: RoaringBitmap,
    pub frontier_set: RoaringBitmap,
    pub page_size: u32,
    pub total_bytes: u64,
    pub risk: RiskEstimate,
    pub reasons: Vec<ClusterReason>,
}
pub struct CapsuleDraft {
    pub repository_id: RepositoryId,
    pub generation: GenerationId,
    pub profile_key: ProfileKey,
    pub page_set: RoaringBitmap,
    pub mandatory_set: RoaringBitmap,
    pub frontier_set: RoaringBitmap,
    pub page_size: u32,
    pub risk: RiskEstimate,
    pub reasons: Vec<ClusterReason>,
}

impl CapsulePlan {
    pub fn new(draft: CapsuleDraft) -> Result<Self, MirageError> {
        let CapsuleDraft {
            repository_id,
            generation,
            profile_key,
            page_set,
            mandatory_set,
            frontier_set,
            page_size,
            risk,
            mut reasons,
        } = draft;
        if profile_key.0.is_empty()
            || profile_key.0.len() > 1024
            || page_size == 0
            || !page_size.is_power_of_two()
            || !mandatory_set.is_subset(&page_set)
            || !frontier_set.is_subset(&page_set)
        {
            return Err(MirageError::invalid_argument(
                "invalid capsule sets, profile, or page size",
            ));
        }
        risk.validate()?;
        reasons.sort_by_key(|r| (r.cluster_id, r.kind));
        if reasons
            .iter()
            .any(|r| r.evidence_millionths > 1_000_000 || !r.pages.is_subset(&page_set))
        {
            return Err(MirageError::invalid_argument(
                "capsule provenance is invalid",
            ));
        }
        let total_bytes = page_set
            .len()
            .checked_mul(page_size as u64)
            .ok_or_else(|| MirageError::invalid_argument("capsule byte count overflows"))?;
        let mut hasher = blake3::Hasher::new();
        hasher.update(repository_id.as_bytes());
        hasher.update(&generation.0.to_le_bytes());
        hasher.update(profile_key.0.as_bytes());
        hasher.update(&page_size.to_le_bytes());
        for page in &page_set {
            hasher.update(&page.to_le_bytes());
        }
        for page in &mandatory_set {
            hasher.update(b"m");
            hasher.update(&page.to_le_bytes());
        }
        let digest = hasher.finalize();
        let mut id = [0; 16];
        id.copy_from_slice(&digest.as_bytes()[..16]);
        Ok(Self {
            capsule_id: CapsuleId::from_bytes(id),
            repository_id,
            generation,
            profile_key,
            page_set,
            mandatory_set,
            frontier_set,
            page_size,
            total_bytes,
            risk,
            reasons,
        })
    }
}
