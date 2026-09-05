use mirage_types::{CheckedRange, MirageError, PageHash};

use crate::index::PackEntry;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedRange {
    pub range: CheckedRange,
    pub pages: Vec<PageHash>,
}

pub fn plan_ranges(
    entries: &[PackEntry],
    pack_length: u64,
    max_gap: u64,
    max_window: u64,
) -> Result<Vec<PlannedRange>, MirageError> {
    if max_window == 0 {
        return Err(MirageError::invalid_argument(
            "range window must be positive",
        ));
    }
    let mut ordered = entries.to_vec();
    ordered.sort_by_key(|entry| entry.frame_offset);
    let mut result: Vec<PlannedRange> = Vec::new();
    for entry in ordered {
        let end = entry
            .frame_offset
            .checked_add(entry.frame_length)
            .ok_or_else(|| MirageError::manifest_invalid("pack frame range overflows"))?;
        if entry.frame_length == 0 || end > pack_length || entry.frame_length > max_window {
            return Err(MirageError::manifest_invalid(
                "pack frame range is outside its immutable object",
            ));
        }
        if let Some(last) = result.last_mut() {
            let last_end = last.range.end_exclusive();
            if entry.frame_offset < last_end {
                return Err(MirageError::manifest_invalid("pack frame ranges overlap"));
            }
            let gap = entry.frame_offset - last_end;
            let combined = end - last.range.start();
            if gap <= max_gap && combined <= max_window {
                last.range = CheckedRange::new(last.range.start(), combined)?;
                last.pages.push(entry.page_hash);
                continue;
            }
        }
        result.push(PlannedRange {
            range: CheckedRange::new(entry.frame_offset, entry.frame_length)?,
            pages: vec![entry.page_hash],
        });
    }
    Ok(result)
}
