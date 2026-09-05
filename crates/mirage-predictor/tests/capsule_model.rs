use mirage_predictor::capsule::*;
use mirage_types::{GenerationId, RepositoryId};
use roaring::RoaringBitmap;
fn bitmap(values: &[u32]) -> RoaringBitmap {
    values.iter().copied().collect()
}
#[test]
fn id_subset_dedupe_risk_and_roundtrip_are_deterministic() {
    let risk = RiskEstimate {
        held_out_violation_millionths: 10,
        unseen_branch_mass_millionths: 20,
        data_quality_millionths: 30,
        version_transfer_confidence_millionths: 900_000,
    };
    let args = (
        RepositoryId::from_bytes([1; 16]),
        GenerationId(2),
        ProfileKey("1080p/en-US".into()),
        bitmap(&[1, 2, 2, 3]),
        bitmap(&[1]),
        bitmap(&[3]),
        1_048_576,
        risk,
        vec![ClusterReason {
            cluster_id: 7,
            kind: ReasonKind::HardSet,
            pages: bitmap(&[1]),
            evidence_millionths: 1_000_000,
        }],
    );
    let a = CapsulePlan::new(CapsuleDraft {
        repository_id: args.0,
        generation: args.1,
        profile_key: args.2.clone(),
        page_set: args.3.clone(),
        mandatory_set: args.4.clone(),
        frontier_set: args.5.clone(),
        page_size: args.6,
        risk: args.7,
        reasons: args.8.clone(),
    })
    .unwrap();
    let b = CapsulePlan::new(CapsuleDraft {
        repository_id: args.0,
        generation: args.1,
        profile_key: args.2,
        page_set: args.3,
        mandatory_set: args.4,
        frontier_set: args.5,
        page_size: args.6,
        risk: args.7,
        reasons: args.8,
    })
    .unwrap();
    assert_eq!(a, b);
    assert_eq!(a.total_bytes, 3 * 1_048_576);
    let bytes = serde_json::to_vec(&a).unwrap();
    assert_eq!(serde_json::from_slice::<CapsulePlan>(&bytes).unwrap(), a);
}
#[test]
fn mandatory_outside_plan_and_unbounded_risk_fail() {
    let risk = RiskEstimate {
        held_out_violation_millionths: 1_000_001,
        unseen_branch_mass_millionths: 0,
        data_quality_millionths: 0,
        version_transfer_confidence_millionths: 0,
    };
    assert!(
        CapsulePlan::new(CapsuleDraft {
            repository_id: RepositoryId::from_bytes([1; 16]),
            generation: GenerationId(1),
            profile_key: ProfileKey("p".into()),
            page_set: bitmap(&[1]),
            mandatory_set: bitmap(&[2]),
            frontier_set: bitmap(&[]),
            page_size: 65536,
            risk,
            reasons: vec![],
        })
        .is_err()
    );
}
