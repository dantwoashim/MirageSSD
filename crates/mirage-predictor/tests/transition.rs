use mirage_predictor::transition::{TransitionGraph, TransitionPolicy};
#[test]
fn variable_context_decay_top_k_and_backoff_are_deterministic() {
    let policy = TransitionPolicy {
        maximum_order: 2,
        top_k: 2,
        minimum_support_millionths: 1,
        smoothing_millionths: 1,
        session_decay_millionths: 500_000,
    };
    let graph =
        TransitionGraph::train(&[vec![1, 2, 3], vec![1, 2, 4], vec![1, 2, 4]], policy).unwrap();
    let exact = graph.predict(&[1, 2]);
    assert_eq!(exact[0].cluster, 4);
    assert_eq!(exact[0].context_order, 2);
    let backoff = graph.predict(&[9, 2]);
    assert_eq!(backoff[0].context_order, 1);
    assert!(
        exact
            .iter()
            .map(|c| c.probability_millionths as u64)
            .sum::<u64>()
            <= 1_000_000
    );
}
