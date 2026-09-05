use mirage_cache::{AdmissionContext, AdmissionDecision, AdmissionWeights};
use mirage_types::PageHash;

fn context(incoming_freq: u32, victim_freq: u32) -> AdmissionContext {
    AdmissionContext {
        incoming_freq,
        victim_freq,
        incoming_miss_cost: 1,
        victim_miss_cost: 1,
        incoming_capsule_probability: 0.0,
        prefetched: true,
    }
}

#[test]
fn hot_set_resists_scan_and_blocking_overrides() {
    let weights = AdmissionWeights::default();
    let incoming = PageHash::from_bytes([1; 32]);
    let hot = PageHash::from_bytes([2; 32]);
    assert_eq!(
        weights
            .decide(incoming, hot, context(1, 20), false)
            .expect("decision"),
        AdmissionDecision::RejectSpeculative
    );
    assert_eq!(
        weights
            .decide(incoming, hot, context(1, 20), true)
            .expect("blocking"),
        AdmissionDecision::Admit
    );
}

#[test]
fn score_and_tie_break_are_deterministic() {
    let weights = AdmissionWeights::default();
    let low = PageHash::from_bytes([1; 32]);
    let high = PageHash::from_bytes([2; 32]);
    assert_eq!(
        weights
            .decide(low, high, context(10, 1), false)
            .expect("hot incoming"),
        AdmissionDecision::Admit
    );
    let tie = AdmissionContext {
        prefetched: false,
        ..context(2, 2)
    };
    assert_eq!(
        weights.decide(low, high, tie, false).expect("tie"),
        AdmissionDecision::Admit
    );
    assert_eq!(
        weights.decide(high, low, tie, false).expect("tie reverse"),
        AdmissionDecision::RejectSpeculative
    );
}
