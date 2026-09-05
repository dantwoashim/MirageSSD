use mirage_predictor::GameProfile;
use mirage_predictor::hard_set::PageKey;
use mirage_types::MirageError;
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Baseline {
    NoPrefetch,
    Sequential,
    RecentUnion,
    Hybrid,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoldMetrics {
    pub held_out_index: usize,
    pub baseline: Baseline,
    pub predicted_pages: u64,
    pub needed_pages: u64,
    pub hit_pages: u64,
    pub violations: u64,
    pub precision_millionths: u32,
    pub recall_millionths: u32,
}
pub fn evaluate(profiles: &[GameProfile]) -> Result<Vec<FoldMetrics>, MirageError> {
    if profiles.len() < 2 {
        return Err(MirageError::invalid_argument(
            "held-out evaluation requires at least two sessions",
        ));
    }
    for p in profiles {
        p.validate()?;
    }
    let sessions: Vec<BTreeSet<PageKey>> = profiles
        .iter()
        .map(|p| {
            p.page_observations
                .iter()
                .map(|o| PageKey {
                    file_index: o.file_index,
                    page_ordinal: o.page_ordinal,
                })
                .collect()
        })
        .collect();
    let mut output = Vec::new();
    for held in 0..sessions.len() {
        let training: Vec<_> = sessions
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != held)
            .map(|(_, s)| s)
            .collect();
        let union: BTreeSet<_> = training.iter().flat_map(|s| s.iter().copied()).collect();
        let intersection: BTreeSet<_> =
            training.iter().skip(1).fold(training[0].clone(), |acc, s| {
                acc.intersection(s).copied().collect()
            });
        let sequential: BTreeSet<_> = intersection
            .iter()
            .flat_map(|p| {
                [
                    *p,
                    PageKey {
                        file_index: p.file_index,
                        page_ordinal: p.page_ordinal.saturating_add(1),
                    },
                ]
            })
            .collect();
        for (baseline, predicted) in [
            (Baseline::NoPrefetch, BTreeSet::new()),
            (Baseline::Sequential, sequential),
            (Baseline::RecentUnion, union.clone()),
            (
                Baseline::Hybrid,
                union.union(&intersection).copied().collect(),
            ),
        ] {
            let needed = &sessions[held];
            let hit = predicted.intersection(needed).count() as u64;
            let precision = if predicted.is_empty() {
                0
            } else {
                (hit * 1_000_000 / predicted.len() as u64) as u32
            };
            let recall = if needed.is_empty() {
                1_000_000
            } else {
                (hit * 1_000_000 / needed.len() as u64) as u32
            };
            output.push(FoldMetrics {
                held_out_index: held,
                baseline,
                predicted_pages: predicted.len() as u64,
                needed_pages: needed.len() as u64,
                hit_pages: hit,
                violations: needed.len() as u64 - hit,
                precision_millionths: precision,
                recall_millionths: recall,
            });
        }
    }
    output.sort_by_key(|m| (m.held_out_index, m.baseline));
    Ok(output)
}
