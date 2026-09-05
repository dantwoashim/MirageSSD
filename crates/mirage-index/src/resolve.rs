use mirage_types::{MirageError, PageOrdinal};
use smallvec::SmallVec;

use crate::view::{ExtentView, FileView};

const MAX_RESOLVED_SPANS: usize = 1_048_576;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedSpan {
    pub page_ordinal: PageOrdinal,
    pub page_offset: u32,
    pub len: u32,
    pub dst_offset: u32,
}

pub fn resolve_range(
    file: FileView<'_>,
    offset: u64,
    len: usize,
) -> Result<SmallVec<[ResolvedSpan; 8]>, MirageError> {
    let mut output = SmallVec::new();
    if len == 0 {
        return Ok(output);
    }
    let requested_length =
        u64::try_from(len).map_err(|_| MirageError::invalid_argument("read length exceeds u64"))?;
    let requested_end = offset
        .checked_add(requested_length)
        .ok_or_else(|| MirageError::invalid_argument("read range overflows"))?;
    if offset >= file.logical_size() {
        return Ok(output);
    }
    let clipped_end = requested_end.min(file.logical_size());
    let clipped_length = clipped_end - offset;
    if clipped_length > u64::from(u32::MAX) {
        return Err(MirageError::invalid_argument(
            "one resolved read cannot exceed u32::MAX bytes",
        ));
    }
    let mut low = 0_u32;
    let mut high = file.extent_count();
    while low < high {
        let middle = low + (high - low) / 2;
        let extent = file.extent(middle)?;
        let end = extent
            .logical_offset()
            .checked_add(extent.logical_length())
            .ok_or_else(|| MirageError::manifest_invalid("extent end overflows"))?;
        if end <= offset {
            low = middle + 1;
        } else {
            high = middle;
        }
    }
    let span_count = count_spans(file, low, offset, clipped_end)?;
    if span_count > MAX_RESOLVED_SPANS {
        return Err(MirageError::invalid_argument(
            "read would exceed the resolved-span safety bound",
        ));
    }
    output
        .try_reserve(span_count.saturating_sub(output.inline_size()))
        .map_err(|_| MirageError::internal_invariant("resolved-span allocation failed"))?;

    for extent_index in low..file.extent_count() {
        let extent = file.extent(extent_index)?;
        if extent.logical_offset() >= clipped_end {
            break;
        }
        let first_page = first_page_for_offset(file, extent, offset)?;
        let mut page_logical_offset = extent
            .logical_offset()
            .checked_add(
                u64::from(first_page)
                    .checked_mul(file.page_size())
                    .ok_or_else(|| MirageError::manifest_invalid("page seek offset overflows"))?,
            )
            .ok_or_else(|| MirageError::manifest_invalid("page seek address overflows"))?;
        for page_index in first_page..extent.page_count() {
            let page = extent.page(page_index)?;
            let page_end = page_logical_offset
                .checked_add(u64::from(page.logical_length()))
                .ok_or_else(|| MirageError::manifest_invalid("page end overflows"))?;
            let intersection_start = page_logical_offset.max(offset);
            let intersection_end = page_end.min(clipped_end);
            if intersection_start < intersection_end {
                output.push(ResolvedSpan {
                    page_ordinal: PageOrdinal::from_u32(page.ordinal()),
                    page_offset: u32::try_from(intersection_start - page_logical_offset)
                        .map_err(|_| MirageError::manifest_invalid("page offset exceeds u32"))?,
                    len: u32::try_from(intersection_end - intersection_start)
                        .map_err(|_| MirageError::manifest_invalid("span length exceeds u32"))?,
                    dst_offset: u32::try_from(intersection_start - offset).map_err(|_| {
                        MirageError::manifest_invalid("destination offset exceeds u32")
                    })?,
                });
            }
            page_logical_offset = page_end;
            if page_logical_offset >= clipped_end {
                break;
            }
        }
    }
    let resolved = output
        .iter()
        .try_fold(0_u64, |total, span| total.checked_add(u64::from(span.len)));
    if resolved != Some(clipped_length) {
        return Err(MirageError::manifest_invalid(
            "resolved spans do not cover the clipped read exactly",
        ));
    }
    Ok(output)
}

fn count_spans(
    file: FileView<'_>,
    first_extent: u32,
    offset: u64,
    clipped_end: u64,
) -> Result<usize, MirageError> {
    let mut count = 0_usize;
    for extent_index in first_extent..file.extent_count() {
        let extent = file.extent(extent_index)?;
        if extent.logical_offset() >= clipped_end {
            break;
        }
        let first_page = first_page_for_offset(file, extent, offset)?;
        let mut page_logical_offset = extent
            .logical_offset()
            .checked_add(
                u64::from(first_page)
                    .checked_mul(file.page_size())
                    .ok_or_else(|| MirageError::manifest_invalid("page seek offset overflows"))?,
            )
            .ok_or_else(|| MirageError::manifest_invalid("page seek address overflows"))?;
        for page_index in first_page..extent.page_count() {
            let page = extent.page(page_index)?;
            let page_end = page_logical_offset
                .checked_add(u64::from(page.logical_length()))
                .ok_or_else(|| MirageError::manifest_invalid("page end overflows"))?;
            if page_logical_offset.max(offset) < page_end.min(clipped_end) {
                count = count.checked_add(1).ok_or_else(|| {
                    MirageError::invalid_argument("resolved span count overflows")
                })?;
                if count > MAX_RESOLVED_SPANS {
                    return Ok(count);
                }
            }
            page_logical_offset = page_end;
            if page_logical_offset >= clipped_end {
                break;
            }
        }
    }
    Ok(count)
}

fn first_page_for_offset(
    file: FileView<'_>,
    extent: ExtentView<'_>,
    offset: u64,
) -> Result<u32, MirageError> {
    if offset <= extent.logical_offset() {
        return Ok(0);
    }
    let relative = offset - extent.logical_offset();
    let page = relative / file.page_size();
    let page = u32::try_from(page)
        .map_err(|_| MirageError::manifest_invalid("page seek ordinal exceeds u32"))?;
    if page >= extent.page_count() {
        return Err(MirageError::manifest_invalid(
            "page seek ordinal exceeds extent",
        ));
    }
    Ok(page)
}
