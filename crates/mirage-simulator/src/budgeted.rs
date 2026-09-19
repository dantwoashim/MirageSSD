//! Equal-budget prediction baselines with honest abstention.
//!
//! These baselines answer one question: given the same disk budget and only
//! simple rules (no learned model), how many held-out pages could a policy have
//! covered? They are baselines to beat at equal budget, not predictions of a
//! real title's access stream.

use std::collections::{BTreeMap, BTreeSet};

use mirage_predictor::GameProfile;
use mirage_predictor::hard_set::PageKey;
use mirage_types::MirageError;

/// One second in the profile's microsecond touch deltas, used by
/// [`BudgetPolicy::CoAccessCluster`].
const COACCESS_WINDOW_US: u64 = 1_000_000;

/// Simple deterministic policies; none are learned models.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum BudgetPolicy {
    /// Rank by earliest `first_touch_delta_us` across training sessions.
    AccessOrder,
    /// Rank by number of training sessions containing the page, then earliest touch.
    Frequency,
    /// Pages of the most recent training session first, then the previous, and
    /// so on. "Most recent" is the highest profile index: the service loads
    /// profiles in path-sorted timestamp order.
    Recency,
    /// Intersection of all training sessions, then pages co-occurring within a
    /// one-second window of an intersection page, by co-occurrence count.
    CoAccessCluster,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BudgetedFoldMetrics {
    pub held_out_index: usize,
    pub policy: BudgetPolicy,
    pub budget_pages: u64,
    /// True when training evidence is below `min_training_sessions`: the honest
    /// "not enough evidence" state rather than a fake confident miss.
    pub abstained: bool,
    /// Never exceeds `budget_pages`.
    pub predicted_pages: u64,
    pub needed_pages: u64,
    pub hit_pages: u64,
    /// Synthetic ordering model, not a network model: predicted pages arrive in
    /// ranked order; a needed page counts as a deadline miss when its rank
    /// exceeds `lead_pages + index of its first touch` in the held-out session.
    pub deadline_misses: u64,
    /// Needed pages not predicted, regardless of timing.
    pub violations: u64,
    pub precision_millionths: u32,
    pub recall_millionths: u32,
    /// Mean training-session support of the selected pages; 0 when abstained.
    pub confidence_millionths: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BudgetedConfig {
    pub budget_pages: u64,
    /// Default 2: below this many training sessions the fold abstains.
    pub min_training_sessions: usize,
    /// Predicted pages assumed resident before the session's first touch;
    /// default 0.
    pub lead_pages: u64,
}

impl BudgetedConfig {
    #[must_use]
    pub const fn new(budget_pages: u64) -> Self {
        Self {
            budget_pages,
            min_training_sessions: 2,
            lead_pages: 0,
        }
    }
}

/// Unique pages of a session in first-touch order, plus their touch deltas.
struct SessionView {
    ordered: Vec<PageKey>,
    touch: BTreeMap<PageKey, u64>,
}

impl SessionView {
    fn new(profile: &GameProfile) -> Self {
        let mut ordered = Vec::new();
        let mut touch = BTreeMap::new();
        for observation in &profile.page_observations {
            let page = PageKey {
                file_index: observation.file_index,
                page_ordinal: observation.page_ordinal,
            };
            if touch
                .insert(page, observation.first_touch_delta_us)
                .is_none()
            {
                ordered.push(page);
            }
        }
        Self { ordered, touch }
    }
}

fn earliest_touch(training: &[usize], sessions: &[SessionView], page: PageKey) -> u64 {
    training
        .iter()
        .filter_map(|session| sessions[*session].touch.get(&page).copied())
        .min()
        .unwrap_or(u64::MAX)
}

fn session_support(training: &[usize], sessions: &[SessionView], page: PageKey) -> u64 {
    training
        .iter()
        .filter(|session| sessions[**session].touch.contains_key(&page))
        .count() as u64
}

fn rank(policy: BudgetPolicy, training: &[usize], sessions: &[SessionView]) -> Vec<PageKey> {
    let union: BTreeSet<PageKey> = training
        .iter()
        .flat_map(|session| sessions[*session].ordered.iter().copied())
        .collect();
    match policy {
        BudgetPolicy::AccessOrder => {
            let mut ranked: Vec<_> = union.into_iter().collect();
            ranked.sort_by_key(|page| (earliest_touch(training, sessions, *page), *page));
            ranked
        }
        BudgetPolicy::Frequency => {
            let mut ranked: Vec<_> = union.into_iter().collect();
            ranked.sort_by_key(|page| {
                (
                    std::cmp::Reverse(session_support(training, sessions, *page)),
                    earliest_touch(training, sessions, *page),
                    *page,
                )
            });
            ranked
        }
        BudgetPolicy::Recency => {
            let mut ranked = Vec::new();
            let mut seen = BTreeSet::new();
            for session in training.iter().rev() {
                for page in &sessions[*session].ordered {
                    if seen.insert(*page) {
                        ranked.push(*page);
                    }
                }
            }
            ranked
        }
        BudgetPolicy::CoAccessCluster => {
            let intersection: BTreeSet<PageKey> = training.iter().skip(1).fold(
                sessions[training[0]].ordered.iter().copied().collect(),
                |acc: BTreeSet<PageKey>, session| {
                    acc.intersection(&sessions[*session].ordered.iter().copied().collect())
                        .copied()
                        .collect()
                },
            );
            let mut ranked: Vec<PageKey> = intersection.iter().copied().collect();
            ranked.sort_by_key(|page| (earliest_touch(training, sessions, *page), *page));
            let mut cooccurrence: BTreeMap<PageKey, u64> = BTreeMap::new();
            for session in training {
                let view = &sessions[*session];
                for page in &view.ordered {
                    if intersection.contains(page) {
                        continue;
                    }
                    let touch = view.touch[page];
                    let count = intersection
                        .iter()
                        .filter(|seed| {
                            view.touch.get(*seed).is_some_and(|seed_touch| {
                                seed_touch.abs_diff(touch) <= COACCESS_WINDOW_US
                            })
                        })
                        .count() as u64;
                    *cooccurrence.entry(*page).or_default() += count;
                }
            }
            let mut tail: Vec<_> = cooccurrence
                .into_iter()
                .filter(|(_, count)| *count > 0)
                .collect();
            tail.sort_by_key(|(page, count)| {
                (
                    std::cmp::Reverse(*count),
                    earliest_touch(training, sessions, *page),
                    *page,
                )
            });
            ranked.extend(tail.into_iter().map(|(page, _)| page));
            ranked
        }
    }
}

/// Evaluates every policy on every held-out fold at the same page budget.
pub fn evaluate_budgeted(
    profiles: &[GameProfile],
    config: &BudgetedConfig,
) -> Result<Vec<BudgetedFoldMetrics>, MirageError> {
    if config.budget_pages == 0 {
        return Err(MirageError::invalid_argument(
            "budgeted evaluation requires a non-zero page budget",
        ));
    }
    if profiles.is_empty() {
        return Err(MirageError::invalid_argument(
            "budgeted evaluation requires at least one session",
        ));
    }
    for profile in profiles {
        profile.validate()?;
    }
    let sessions: Vec<SessionView> = profiles.iter().map(SessionView::new).collect();
    let mut output = Vec::new();
    for held in 0..sessions.len() {
        let training: Vec<usize> = (0..sessions.len()).filter(|index| *index != held).collect();
        let needed: BTreeSet<PageKey> = sessions[held].ordered.iter().copied().collect();
        let needed_count = needed.len() as u64;
        let touch_index: BTreeMap<PageKey, u64> = sessions[held]
            .ordered
            .iter()
            .enumerate()
            .map(|(index, page)| (*page, index as u64))
            .collect();
        for policy in [
            BudgetPolicy::AccessOrder,
            BudgetPolicy::Frequency,
            BudgetPolicy::Recency,
            BudgetPolicy::CoAccessCluster,
        ] {
            if training.len() < config.min_training_sessions {
                output.push(BudgetedFoldMetrics {
                    held_out_index: held,
                    policy,
                    budget_pages: config.budget_pages,
                    abstained: true,
                    predicted_pages: 0,
                    needed_pages: needed_count,
                    hit_pages: 0,
                    deadline_misses: 0,
                    violations: needed_count,
                    precision_millionths: 0,
                    recall_millionths: 0,
                    confidence_millionths: 0,
                });
                continue;
            }
            let predicted = rank(policy, &training, &sessions);
            let predicted: Vec<PageKey> = predicted
                .into_iter()
                .take(config.budget_pages as usize)
                .collect();
            let selected: BTreeSet<PageKey> = predicted.iter().copied().collect();
            let hit = selected.intersection(&needed).count() as u64;
            let deadline_misses = predicted
                .iter()
                .enumerate()
                .filter(|(rank, page)| {
                    needed.contains(page) && (*rank as u64) > config.lead_pages + touch_index[page]
                })
                .count() as u64;
            let confidence = if predicted.is_empty() {
                0
            } else {
                let total: u64 = predicted
                    .iter()
                    .map(|page| {
                        session_support(&training, &sessions, *page) * 1_000_000
                            / training.len() as u64
                    })
                    .sum();
                (total / predicted.len() as u64) as u32
            };
            output.push(BudgetedFoldMetrics {
                held_out_index: held,
                policy,
                budget_pages: config.budget_pages,
                abstained: false,
                predicted_pages: predicted.len() as u64,
                needed_pages: needed_count,
                hit_pages: hit,
                deadline_misses,
                violations: needed_count - hit,
                precision_millionths: if predicted.is_empty() {
                    0
                } else {
                    (hit * 1_000_000 / predicted.len() as u64) as u32
                },
                recall_millionths: if needed_count == 0 {
                    1_000_000
                } else {
                    (hit * 1_000_000 / needed_count) as u32
                },
                confidence_millionths: confidence,
            });
        }
    }
    output.sort_by_key(|metrics| (metrics.held_out_index, metrics.policy));
    Ok(output)
}
