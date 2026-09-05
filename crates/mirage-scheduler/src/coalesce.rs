use mirage_types::{CheckedRange, MirageError};

use crate::{FetchPriority, FetchWindow, FrameMapping, WindowFrame};

pub fn coalesce(
    mut frames: Vec<WindowFrame>,
    max_encoded_bytes: u64,
    max_gap_bytes: u64,
) -> Result<Vec<FetchWindow>, MirageError> {
    if max_encoded_bytes == 0 {
        return Err(MirageError::invalid_argument(
            "fetch window maximum is zero",
        ));
    }
    frames.sort_by(|a, b| {
        a.object
            .provider_object_id
            .cmp(&b.object.provider_object_id)
            .then_with(|| a.range.start().cmp(&b.range.start()))
            .then_with(|| a.page_hash.cmp(&b.page_hash))
    });
    let mut windows = Vec::new();
    for frame in frames {
        if frame.range.is_empty()
            || frame.range.end_exclusive() > frame.object.byte_length.as_u64()
            || frame.range.len() > max_encoded_bytes
        {
            return Err(MirageError::invalid_argument(
                "frame range is outside coalescer bounds",
            ));
        }
        let can_join = windows.last().is_some_and(|window: &FetchWindow| {
            let gap = frame
                .range
                .start()
                .saturating_sub(window.range.end_exclusive());
            window.object == frame.object
                && frame.range.start() >= window.range.end_exclusive()
                && gap <= max_gap_bytes
                && frame
                    .range
                    .end_exclusive()
                    .saturating_sub(window.range.start())
                    <= max_encoded_bytes
                && compatible(window.priority, frame.priority)
        });
        if can_join {
            let window = windows.last_mut().expect("window exists");
            let gap = frame.range.start() - window.range.end_exclusive();
            window.gap_bytes += gap;
            window.frames.push(FrameMapping {
                page_hash: frame.page_hash,
                window_offset: frame.range.start() - window.range.start(),
                encoded_length: frame.range.len(),
            });
            window.range = CheckedRange::from_start_and_end(
                window.range.start(),
                frame.range.end_exclusive(),
            )?;
            window.priority = window.priority.min(frame.priority);
        } else {
            windows.push(FetchWindow {
                object: frame.object,
                range: frame.range,
                priority: frame.priority,
                frames: vec![FrameMapping {
                    page_hash: frame.page_hash,
                    window_offset: 0,
                    encoded_length: frame.range.len(),
                }],
                gap_bytes: 0,
            });
        }
    }
    Ok(windows)
}
const fn compatible(a: FetchPriority, b: FetchPriority) -> bool {
    (a as u8 <= FetchPriority::P2Capsule as u8) == (b as u8 <= FetchPriority::P2Capsule as u8)
}
