use mirage_simulator::statistics::{
    median_of_per_run_percentiles, percentile_nearest_rank, pooled_percentile,
    sessions_required_for_zero_failure_bound, zero_failure_upper_bound_millionths,
};

#[test]
fn three_hundred_zero_failure_sessions_bound_the_rate_near_one_percent() {
    assert_eq!(
        zero_failure_upper_bound_millionths(300, 50_000).unwrap(),
        9_936
    );
}

#[test]
fn zero_failure_claims_need_the_session_count_they_advertise() {
    assert_eq!(
        sessions_required_for_zero_failure_bound(100, 50_000).unwrap(),
        29_956
    );
    assert!(zero_failure_upper_bound_millionths(29_956, 50_000).unwrap() <= 100);
    // The returned bound is rounded to the nearest millionth; the exact bound
    // one session earlier already exceeds the 0.01% target.
    let unrounded = 1.0 - 0.05_f64.powf(1.0 / 29_955.0);
    assert!(unrounded > 0.0001);
}

#[test]
fn nearest_rank_percentile_covers_the_edges() {
    let sorted: Vec<u64> = (1..=10).collect();
    assert_eq!(percentile_nearest_rank(&sorted, 0).unwrap(), 1);
    assert_eq!(percentile_nearest_rank(&sorted, 100).unwrap(), 10);
    assert_eq!(percentile_nearest_rank(&sorted, 50).unwrap(), 5);
    assert_eq!(percentile_nearest_rank(&[42], 99).unwrap(), 42);
    assert!(percentile_nearest_rank(&[], 50).is_err());
    assert!(percentile_nearest_rank(&sorted, 101).is_err());
}

#[test]
fn per_run_median_and_pooled_percentile_can_differ_on_fat_tails() {
    // Nine clean runs and one fat-tailed run: pooling hides the tail run,
    // the per-run median does not.
    let mut runs = vec![vec![1_u64; 100]; 9];
    runs.push(vec![1000_u64; 100]);
    let per_run = median_of_per_run_percentiles(&runs, 99).unwrap();
    let pooled = pooled_percentile(&runs, 99).unwrap();
    assert_eq!(per_run, 1);
    assert_eq!(pooled, 1000);
    assert_ne!(per_run, pooled);
}

#[test]
fn invalid_inputs_fail_closed() {
    assert!(zero_failure_upper_bound_millionths(0, 50_000).is_err());
    assert!(zero_failure_upper_bound_millionths(10, 0).is_err());
    assert!(zero_failure_upper_bound_millionths(10, 1_000_000).is_err());
    assert!(sessions_required_for_zero_failure_bound(0, 50_000).is_err());
    assert!(sessions_required_for_zero_failure_bound(1_000_000, 50_000).is_err());
    assert!(median_of_per_run_percentiles(&[], 99).is_err());
    assert!(median_of_per_run_percentiles(&[vec![]], 99).is_err());
    assert!(pooled_percentile(&[], 99).is_err());
    assert!(pooled_percentile(&[vec![]], 99).is_err());
}
