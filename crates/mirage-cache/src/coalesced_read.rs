//! Coalesced reads of adjacent resident slots.
//!
//! Slots are laid out contiguously in the arena file, so a run of
//! adjacent-slot pages can be served with one read while every contributing
//! lease is still held. Any violated precondition is an `InvalidArgument`
//! error; there is no silent fallback.

use mirage_types::MirageError;

use crate::{ArenaShard, ResidentPageGuard};

/// Whether `guards` pin a run of consecutive slot indices in order.
#[must_use]
pub fn slots_are_contiguous(guards: &[ResidentPageGuard]) -> bool {
    guards
        .windows(2)
        .all(|window| window[1].slot_index() == window[0].slot_index().wrapping_add(1))
}

/// Reads `output` starting at `offset` within the first guard's page, spanning
/// the guards in order.
///
/// Requires: `guards` non-empty; every guard's slot is exactly the previous
/// slot + 1; every guard except the last has `logical_length == page_size`;
/// the read stays inside the last guard's logical length. The guards are
/// borrowed for the whole call, so no contributing slot can be evicted or
/// reused meanwhile.
pub fn read_contiguous(
    shard: &ArenaShard,
    guards: &[ResidentPageGuard],
    offset: u32,
    output: &mut [u8],
) -> Result<(), MirageError> {
    if guards.is_empty() {
        return Err(MirageError::invalid_argument(
            "contiguous read needs at least one resident guard",
        ));
    }
    if guards
        .iter()
        .any(|guard| !std::ptr::eq(guard.page.shard.as_ref(), shard))
    {
        return Err(MirageError::invalid_argument(
            "contiguous read guards do not pin the requested arena",
        ));
    }
    if !slots_are_contiguous(guards) {
        return Err(MirageError::invalid_argument(
            "contiguous read guards are not adjacent slots",
        ));
    }
    let page_size = shard.layout().page_size.as_u64();
    for guard in &guards[..guards.len() - 1] {
        if u64::from(guard.logical_length()) != page_size {
            return Err(MirageError::invalid_argument(
                "contiguous read crossed a short middle page",
            ));
        }
    }
    let last = guards.last().expect("non-empty guards");
    let window_end = (guards.len() as u64 - 1)
        .checked_mul(page_size)
        .and_then(|bytes| bytes.checked_add(u64::from(last.logical_length())))
        .ok_or_else(|| MirageError::invalid_argument("contiguous read window overflows"))?;
    let read_end = u64::from(offset)
        .checked_add(output.len() as u64)
        .ok_or_else(|| MirageError::invalid_argument("contiguous read range overflows"))?;
    if read_end > window_end {
        return Err(MirageError::invalid_argument(
            "contiguous read exceeds the last page's logical length",
        ));
    }
    if output.is_empty() {
        return Ok(());
    }
    if u64::from(offset) >= page_size {
        return Err(MirageError::invalid_argument(
            "contiguous read offset is outside the first page",
        ));
    }
    let slot_count = u32::try_from(guards.len())
        .map_err(|_| MirageError::invalid_argument("contiguous read guard count overflows"))?;
    shard.read_slots_contiguous(guards[0].slot_index(), slot_count, offset, output)
}
