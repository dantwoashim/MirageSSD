use mirage_predictor::{GameProfile, ObservationClass, PageObservation};
use mirage_simulator::budgeted::{BudgetPolicy, BudgetedConfig, evaluate_budgeted};
use mirage_types::{ManifestHash, RepositoryId};

fn profile(label: &str, touches: &[(u32, u64)]) -> GameProfile {
    let mut ordered = touches.to_vec();
    ordered.sort_by_key(|(_, delta)| *delta);
    GameProfile {
        format_version: 1,
        repository_id: RepositoryId::from_bytes([1; 16]),
        manifest_hash: ManifestHash::from_bytes([2; 32]),
        label: label.into(),
        page_observations: ordered
            .into_iter()
            .map(|(page_ordinal, first_touch_delta_us)| PageObservation {
                file_index: 0,
                page_ordinal,
                first_touch_delta_us,
                class: ObservationClass::Demand,
            })
            .collect(),
        processes: vec![],
        dropped_event_count: 0,
    }
}

fn shared_training() -> [GameProfile; 3] {
    [
        profile(
            "s1",
            &[
                (1, 0),
                (2, 1),
                (3, 2),
                (4, 3),
                (5, 4),
                (6, 5),
                (7, 6),
                (8, 7),
                (9, 8),
                (10, 9),
                (101, 10),
            ],
        ),
        profile(
            "s2",
            &[
                (1, 0),
                (2, 1),
                (3, 2),
                (4, 3),
                (5, 4),
                (6, 5),
                (7, 6),
                (8, 7),
                (9, 8),
                (10, 9),
                (102, 10),
            ],
        ),
        profile(
            "s3",
            &[
                (1, 0),
                (2, 1),
                (3, 2),
                (4, 3),
                (5, 4),
                (6, 5),
                (7, 6),
                (8, 7),
                (9, 8),
                (10, 9),
                (103, 10),
            ],
        ),
    ]
}

fn held_session() -> GameProfile {
    profile(
        "held",
        &[
            (1, 0),
            (2, 1),
            (3, 2),
            (4, 3),
            (5, 4),
            (6, 5),
            (7, 6),
            (8, 7),
            (9, 8),
            (10, 9),
            (201, 10),
            (202, 11),
        ],
    )
}

fn fold(
    metrics: &[mirage_simulator::BudgetedFoldMetrics],
    held: usize,
    policy: BudgetPolicy,
) -> &mirage_simulator::BudgetedFoldMetrics {
    metrics
        .iter()
        .find(|m| m.held_out_index == held && m.policy == policy)
        .expect("fold metrics")
}

#[test]
fn frequency_covers_the_shared_core_at_equal_budget() {
    let mut profiles = vec![held_session()];
    profiles.extend(shared_training());
    let metrics = evaluate_budgeted(
        &profiles,
        &BudgetedConfig {
            budget_pages: 10,
            min_training_sessions: 2,
            lead_pages: 0,
        },
    )
    .expect("evaluate");
    for m in metrics.iter().filter(|m| m.held_out_index == 0) {
        assert!(!m.abstained);
        assert!(m.predicted_pages <= 10);
    }
    let frequency = fold(&metrics, 0, BudgetPolicy::Frequency);
    assert_eq!(frequency.predicted_pages, 10);
    assert_eq!(frequency.hit_pages, 10);
    assert_eq!(frequency.needed_pages, 12);
    assert_eq!(frequency.violations, 2);
    assert_eq!(frequency.recall_millionths, 833_333);
    assert_eq!(frequency.precision_millionths, 1_000_000);
}

#[test]
fn insufficient_training_sessions_abstain_instead_of_guessing() {
    let profiles = [held_session(), profile("only", &[(1, 0), (2, 1)])];
    let metrics = evaluate_budgeted(
        &profiles,
        &BudgetedConfig {
            budget_pages: 10,
            min_training_sessions: 2,
            lead_pages: 0,
        },
    )
    .expect("evaluate");
    assert_eq!(metrics.len(), 8);
    for m in &metrics {
        assert!(m.abstained);
        assert_eq!(m.predicted_pages, 0);
        assert_eq!(m.hit_pages, 0);
        assert_eq!(m.confidence_millionths, 0);
        assert_eq!(m.recall_millionths, 0);
        assert_eq!(m.violations, m.needed_pages);
    }
}

#[test]
fn evaluation_is_deterministic_and_tie_breaks_by_page_key() {
    let mut profiles = vec![held_session()];
    profiles.extend(shared_training());
    let config = BudgetedConfig {
        budget_pages: 10,
        min_training_sessions: 2,
        lead_pages: 0,
    };
    let first = evaluate_budgeted(&profiles, &config).expect("first");
    let second = evaluate_budgeted(&profiles, &config).expect("second");
    assert_eq!(first, second);
    // Equal-support pages resolve to the same ordering across policies.
    let access = fold(&first, 0, BudgetPolicy::AccessOrder);
    assert_eq!(access.predicted_pages, 10);
    assert_eq!(access.violations, 2);
}

#[test]
fn budget_truncates_and_precision_is_on_the_truncated_set() {
    let mut profiles = vec![held_session()];
    profiles.extend(shared_training());
    let metrics = evaluate_budgeted(
        &profiles,
        &BudgetedConfig {
            budget_pages: 3,
            min_training_sessions: 2,
            lead_pages: 0,
        },
    )
    .expect("evaluate");
    let frequency = fold(&metrics, 0, BudgetPolicy::Frequency);
    assert_eq!(frequency.predicted_pages, 3);
    assert_eq!(frequency.hit_pages, 3);
    assert_eq!(frequency.precision_millionths, 1_000_000);
    assert_eq!(frequency.violations, 9);
}

#[test]
fn a_predicted_page_arriving_after_its_first_touch_is_a_deadline_miss() {
    let training = [
        profile("t1", &[(2, 0), (3, 1), (1, 100)]),
        profile("t2", &[(2, 0), (3, 1), (1, 100)]),
    ];
    let held = profile("h", &[(1, 0), (2, 1), (3, 2)]);
    let profiles = [held, training[0].clone(), training[1].clone()];
    let metrics = evaluate_budgeted(
        &profiles,
        &BudgetedConfig {
            budget_pages: 3,
            min_training_sessions: 2,
            lead_pages: 0,
        },
    )
    .expect("evaluate");
    let access = fold(&metrics, 0, BudgetPolicy::AccessOrder);
    // Access order ranks page 1 last (earliest training touch 100us) but the
    // held-out session touches it first: it would not have arrived in time.
    assert_eq!(access.hit_pages, 3);
    assert_eq!(access.violations, 0);
    assert_eq!(access.deadline_misses, 1);
}

#[test]
fn confidence_is_mean_session_support_of_the_selection() {
    let profiles = [
        profile("h", &[(1, 0)]),
        profile("s1", &[(1, 0), (11, 1)]),
        profile("s2", &[(1, 0), (12, 1)]),
        profile("s3", &[(1, 0), (13, 1)]),
    ];
    let metrics = evaluate_budgeted(
        &profiles,
        &BudgetedConfig {
            budget_pages: 1,
            min_training_sessions: 2,
            lead_pages: 0,
        },
    )
    .expect("evaluate");
    let frequency = fold(&metrics, 0, BudgetPolicy::Frequency);
    assert_eq!(frequency.predicted_pages, 1);
    assert_eq!(frequency.confidence_millionths, 1_000_000);
    assert_eq!(frequency.hit_pages, 1);
}

#[test]
fn invalid_configurations_fail_closed() {
    let mut profiles = vec![held_session()];
    profiles.extend(shared_training());
    let error = evaluate_budgeted(
        &profiles,
        &BudgetedConfig {
            budget_pages: 0,
            min_training_sessions: 2,
            lead_pages: 0,
        },
    )
    .expect_err("zero budget");
    assert_eq!(error.kind, mirage_types::MirageErrorKind::InvalidArgument);
    assert!(evaluate_budgeted(&[], &BudgetedConfig::new(4)).is_err());
}
