//! Statistical-discipline helpers for the proposal's §4 evidence rules.
//!
//! These functions give the evaluation harness honest answers to "how much
//! evidence is enough": a Clopper-Pearson upper bound for zero-failure runs,
//! the session count a claim needs, and percentile helpers that keep the
//! per-run median and pooled views distinct.

use mirage_types::MirageError;

const MILLIONTHS: f64 = 1_000_000.0;

fn check_alpha(alpha_millionths: u32) -> Result<f64, MirageError> {
    if alpha_millionths == 0 || alpha_millionths >= 1_000_000 {
        return Err(MirageError::invalid_argument(
            "alpha must be in the open interval (0, 1_000_000) millionths",
        ));
    }
    Ok(f64::from(alpha_millionths) / MILLIONTHS)
}

/// One-sided (1-alpha) Clopper-Pearson upper bound on the per-session failure
/// probability after `sessions` independent sessions with zero failures:
/// `1 - alpha^(1/sessions)`, in millionths.
pub fn zero_failure_upper_bound_millionths(
    sessions: u64,
    alpha_millionths: u32,
) -> Result<u32, MirageError> {
    if sessions == 0 {
        return Err(MirageError::invalid_argument(
            "zero sessions provide no evidence",
        ));
    }
    let alpha = check_alpha(alpha_millionths)?;
    let upper = 1.0 - alpha.powf(1.0 / sessions as f64);
    Ok((upper * MILLIONTHS).round() as u32)
}

/// Smallest session count with zero observed failures whose upper bound is at
/// or below `target_millionths`: `ceil(ln(alpha) / ln(1 - target))`.
pub fn sessions_required_for_zero_failure_bound(
    target_millionths: u32,
    alpha_millionths: u32,
) -> Result<u64, MirageError> {
    if target_millionths == 0 || target_millionths >= 1_000_000 {
        return Err(MirageError::invalid_argument(
            "target must be in the open interval (0, 1_000_000) millionths",
        ));
    }
    let alpha = check_alpha(alpha_millionths)?;
    let target = f64::from(target_millionths) / MILLIONTHS;
    let sessions = alpha.ln() / (1.0 - target).ln();
    if !sessions.is_finite() || sessions > u64::MAX as f64 {
        return Err(MirageError::invalid_argument(
            "required session count overflows u64",
        ));
    }
    Ok(sessions.ceil() as u64)
}

/// Nearest-rank percentile of a non-empty sorted slice; `pct` in `0..=100`.
/// `pct == 0` returns the minimum, `pct == 100` the maximum.
pub fn percentile_nearest_rank(sorted: &[u64], pct: u8) -> Result<u64, MirageError> {
    if sorted.is_empty() {
        return Err(MirageError::invalid_argument(
            "percentile of an empty sample is undefined",
        ));
    }
    if pct > 100 {
        return Err(MirageError::invalid_argument("percentile exceeds 100"));
    }
    let rank = usize::from(pct)
        .checked_mul(sorted.len())
        .ok_or_else(|| MirageError::invalid_argument("percentile rank overflows"))?
        .div_ceil(100);
    Ok(sorted[rank.saturating_sub(1).min(sorted.len() - 1)])
}

/// Median of each run's own `pct` percentile; every run is a sample slice,
/// sorted internally. Pooling a fat-tailed run into the others would hide it,
/// so each run contributes its own percentile and the median is taken over
/// those.
pub fn median_of_per_run_percentiles(runs: &[Vec<u64>], pct: u8) -> Result<u64, MirageError> {
    if runs.is_empty() || runs.iter().any(Vec::is_empty) {
        return Err(MirageError::invalid_argument(
            "per-run median needs at least one non-empty run",
        ));
    }
    let mut per_run = Vec::with_capacity(runs.len());
    for run in runs {
        let mut sorted = run.clone();
        sorted.sort_unstable();
        per_run.push(percentile_nearest_rank(&sorted, pct)?);
    }
    per_run.sort_unstable();
    percentile_nearest_rank(&per_run, 50)
}

/// `pct` percentile over all samples pooled together. This weights large runs
/// more heavily than [`median_of_per_run_percentiles`] and can hide a
/// fat-tailed run; report both.
pub fn pooled_percentile(runs: &[Vec<u64>], pct: u8) -> Result<u64, MirageError> {
    let mut pooled: Vec<u64> = runs.iter().flatten().copied().collect();
    if pooled.is_empty() {
        return Err(MirageError::invalid_argument(
            "pooled percentile needs at least one sample",
        ));
    }
    pooled.sort_unstable();
    percentile_nearest_rank(&pooled, pct)
}
