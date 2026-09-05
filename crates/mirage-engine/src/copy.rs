use mirage_cache::ResidentPageGuard;
use mirage_types::MirageError;

pub fn copy_span(
    guard: &ResidentPageGuard,
    page_offset: u32,
    destination: &mut [u8],
) -> Result<(), MirageError> {
    let end = u64::from(page_offset)
        .checked_add(destination.len() as u64)
        .ok_or_else(|| MirageError::invalid_argument("page copy range overflows"))?;
    if end > u64::from(guard.logical_length()) {
        return Err(MirageError::integrity_mismatch(
            "resolved span exceeds resident page",
        ));
    }
    guard.read_exact(page_offset, destination)
}
