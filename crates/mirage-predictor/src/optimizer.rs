use mirage_types::MirageError;
use roaring::RoaringBitmap;
use std::collections::BTreeSet;

#[derive(Debug, Clone)]
pub struct CandidateCluster {
    pub id: u64,
    pub pages: RoaringBitmap,
    pub value_millionths: u64,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Optimization {
    pub pages: RoaringBitmap,
    pub selected_clusters: Vec<u64>,
    pub used_bytes: u64,
    pub budget_bytes: u64,
}

pub fn optimize(
    mandatory: &RoaringBitmap,
    candidates: &[CandidateCluster],
    page_size: u64,
    budget_bytes: u64,
    reserve_bytes: u64,
) -> Result<Optimization, MirageError> {
    if page_size == 0 || !page_size.is_power_of_two() {
        return Err(MirageError::invalid_argument("invalid capsule page size"));
    }
    let usable = budget_bytes
        .checked_sub(reserve_bytes)
        .ok_or_else(|| MirageError::cache_full("cache budget is smaller than required reserve"))?;
    let mandatory_bytes = mandatory
        .len()
        .checked_mul(page_size)
        .ok_or_else(|| MirageError::invalid_argument("mandatory byte count overflows"))?;
    if mandatory_bytes > usable {
        return Err(MirageError::cache_full(format!(
            "mandatory capsule requires {mandatory_bytes} bytes"
        )));
    }
    let mut pages = mandatory.clone();
    let mut remaining: BTreeSet<usize> = (0..candidates.len()).collect();
    let mut selected = Vec::new();
    loop {
        let mut best: Option<(usize, u64, u64, u64)> = None;
        for &index in &remaining {
            let unique = &candidates[index].pages - &pages;
            let cost = unique.len() * page_size;
            if cost == 0 {
                if candidates[index].value_millionths > 0 {
                    best = Some((index, u64::MAX, candidates[index].value_millionths, 0));
                    break;
                }
                continue;
            }
            if mandatory_bytes + (pages.len() - mandatory.len()) * page_size + cost > usable {
                continue;
            }
            let score = candidates[index].value_millionths.saturating_mul(1_000_000) / cost;
            let key = (
                score,
                candidates[index].value_millionths,
                u64::MAX - candidates[index].id,
            );
            if best.as_ref().is_none_or(|(_, s, v, c)| key > (*s, *v, *c)) {
                best = Some((index, key.0, key.1, key.2));
            }
        }
        let Some((index, _, _, _)) = best else { break };
        remaining.remove(&index);
        pages |= &candidates[index].pages;
        selected.push(candidates[index].id);
    }
    selected.sort_unstable();
    let used = pages.len() * page_size;
    Ok(Optimization {
        pages,
        selected_clusters: selected,
        used_bytes: used,
        budget_bytes,
    })
}
