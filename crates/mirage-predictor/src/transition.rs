use mirage_types::MirageError;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};

#[derive(Debug, Clone, Copy)]
pub struct TransitionPolicy {
    pub maximum_order: usize,
    pub top_k: usize,
    pub minimum_support_millionths: u32,
    pub session_decay_millionths: u32,
    pub smoothing_millionths: u32,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Candidate {
    pub cluster: u64,
    pub support_millionths: u64,
    pub probability_millionths: u32,
    pub context_order: usize,
}
#[derive(Debug, Clone, Default)]
pub struct TransitionGraph {
    edges: BTreeMap<Vec<u64>, BTreeMap<u64, u64>>,
    policy: Option<TransitionPolicy>,
}

impl TransitionGraph {
    pub fn train(sessions: &[Vec<u64>], policy: TransitionPolicy) -> Result<Self, MirageError> {
        if policy.maximum_order == 0
            || policy.maximum_order > 16
            || policy.top_k == 0
            || policy.top_k > 1024
            || policy.minimum_support_millionths > 1_000_000
            || policy.session_decay_millionths > 1_000_000
            || policy.smoothing_millionths > 1_000_000
        {
            return Err(MirageError::invalid_argument("invalid transition policy"));
        }
        let mut graph = Self {
            edges: BTreeMap::new(),
            policy: Some(policy),
        };
        let mut weight = 1_000_000_u64;
        for session in sessions.iter().rev() {
            for index in 1..session.len() {
                for order in 1..=policy.maximum_order.min(index) {
                    let context = session[index - order..index].to_vec();
                    *graph
                        .edges
                        .entry(context)
                        .or_default()
                        .entry(session[index])
                        .or_default() += weight;
                }
            }
            weight = weight.saturating_mul(policy.session_decay_millionths as u64) / 1_000_000;
        }
        for successors in graph.edges.values_mut() {
            let mut ranked: Vec<_> = successors.iter().map(|(&k, &v)| (k, v)).collect();
            ranked.sort_by_key(|&(k, v)| (std::cmp::Reverse(v), k));
            let keep: BTreeMap<_, _> = ranked.into_iter().take(policy.top_k).collect();
            *successors = keep;
        }
        Ok(graph)
    }
    pub fn predict(&self, recent: &[u64]) -> Vec<Candidate> {
        let Some(policy) = self.policy else {
            return Vec::new();
        };
        for order in (1..=policy.maximum_order.min(recent.len())).rev() {
            let context = &recent[recent.len() - order..];
            let Some(edges) = self.edges.get(context) else {
                continue;
            };
            let total: u64 = edges.values().sum();
            if total < policy.minimum_support_millionths as u64 {
                continue;
            }
            let denominator =
                total.saturating_add(policy.smoothing_millionths as u64 * edges.len() as u64);
            let mut result: Vec<_> = edges
                .iter()
                .map(|(&cluster, &support)| Candidate {
                    cluster,
                    support_millionths: support,
                    probability_millionths: ((support + policy.smoothing_millionths as u64)
                        * 1_000_000
                        / denominator.max(1))
                    .min(1_000_000) as u32,
                    context_order: order,
                })
                .collect();
            result.sort_by_key(|c| (std::cmp::Reverse(c.probability_millionths), c.cluster));
            return result;
        }
        Vec::new()
    }
}

pub fn bounded_context(sequence: &[u64], maximum: usize) -> VecDeque<u64> {
    sequence.iter().rev().take(maximum).rev().copied().collect()
}
