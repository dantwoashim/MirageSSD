use mirage_predictor::optimizer::*;
use roaring::RoaringBitmap;
fn bm(v: &[u32]) -> RoaringBitmap {
    v.iter().copied().collect()
}
#[test]
fn overlap_zero_cost_budget_and_ties_are_deterministic() {
    let candidates = [
        CandidateCluster {
            id: 2,
            pages: bm(&[1, 2]),
            value_millionths: 100,
        },
        CandidateCluster {
            id: 1,
            pages: bm(&[2, 3]),
            value_millionths: 100,
        },
        CandidateCluster {
            id: 3,
            pages: bm(&[1]),
            value_millionths: 1,
        },
    ];
    let result = optimize(&bm(&[1]), &candidates, 1024, 3072, 0).unwrap();
    assert!(result.used_bytes <= 3072);
    assert_eq!(result.pages, bm(&[1, 2, 3]));
    assert!(result.selected_clusters.contains(&3));
    assert_eq!(
        result,
        optimize(&bm(&[1]), &candidates, 1024, 3072, 0).unwrap()
    );
}
#[test]
fn mandatory_minimum_is_exact() {
    let error = optimize(&bm(&[1, 2, 3]), &[], 1024, 3000, 0).unwrap_err();
    assert!(error.message.contains("3072"));
}
