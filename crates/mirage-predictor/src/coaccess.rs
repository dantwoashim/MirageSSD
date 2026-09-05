use mirage_types::MirageError;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimedPage {
    pub timestamp_us: u64,
    pub page: u64,
    pub scan: bool,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageCluster {
    pub id: u64,
    pub pages: BTreeSet<u64>,
    pub session_support: u32,
    pub edge_weight: u64,
}

pub fn cluster(
    sessions: &[Vec<TimedPage>],
    window_us: u64,
    minimum_sessions: u32,
    maximum_cluster_pages: usize,
) -> Result<Vec<PageCluster>, MirageError> {
    if window_us == 0 || minimum_sessions == 0 || maximum_cluster_pages == 0 {
        return Err(MirageError::invalid_argument("invalid co-access policy"));
    }
    let mut edges = BTreeMap::<(u64, u64), (u64, BTreeSet<usize>)>::new();
    for (session_id, events) in sessions.iter().enumerate() {
        for (i, left) in events.iter().enumerate() {
            if left.scan {
                continue;
            }
            for right in events
                .iter()
                .skip(i + 1)
                .take_while(|r| r.timestamp_us.saturating_sub(left.timestamp_us) <= window_us)
            {
                if right.scan || right.page == left.page {
                    continue;
                }
                let pair = if left.page < right.page {
                    (left.page, right.page)
                } else {
                    (right.page, left.page)
                };
                let entry = edges.entry(pair).or_default();
                entry.0 += 1;
                entry.1.insert(session_id);
            }
        }
    }
    let accepted: Vec<_> = edges
        .into_iter()
        .filter(|(_, (_, support))| support.len() >= minimum_sessions as usize)
        .collect();
    let mut adjacency = BTreeMap::<u64, BTreeSet<u64>>::new();
    for ((a, b), _) in &accepted {
        adjacency.entry(*a).or_default().insert(*b);
        adjacency.entry(*b).or_default().insert(*a);
    }
    let mut visited = BTreeSet::new();
    let mut output = Vec::new();
    for &start in adjacency.keys() {
        if !visited.insert(start) {
            continue;
        }
        let mut pending = vec![start];
        let mut component = BTreeSet::new();
        while let Some(page) = pending.pop() {
            component.insert(page);
            if let Some(next) = adjacency.get(&page) {
                for &other in next {
                    if visited.insert(other) {
                        pending.push(other)
                    }
                }
            }
        }
        let pages: Vec<_> = component.into_iter().collect();
        for chunk in pages.chunks(maximum_cluster_pages) {
            let set: BTreeSet<_> = chunk.iter().copied().collect();
            let mut support = BTreeSet::<usize>::new();
            let mut weight = 0;
            for ((a, b), (count, sessions)) in &accepted {
                if set.contains(a) && set.contains(b) {
                    weight += count;
                    support.extend(sessions.iter().copied())
                }
            }
            let id = u64::from_le_bytes(
                blake3::hash(
                    &chunk
                        .iter()
                        .flat_map(|p| p.to_le_bytes())
                        .collect::<Vec<_>>(),
                )
                .as_bytes()[..8]
                    .try_into()
                    .unwrap(),
            );
            output.push(PageCluster {
                id,
                pages: set,
                session_support: support.len() as u32,
                edge_weight: weight,
            });
        }
    }
    output.sort_by_key(|c| c.id);
    Ok(output)
}
