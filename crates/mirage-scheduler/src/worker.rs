use std::sync::Arc;

use mirage_backend::{FetchClass, ObjectBackend};
use mirage_cache::{ArenaShard, InsertOutcome, ResidentIndex, insert_page};
use mirage_db::Database;
use mirage_types::{MirageError, PageHash};
use tokio_util::sync::CancellationToken;

use mirage_pack::PackReadEncryption;

use crate::decode::decode_expected_with_encryption;
use crate::validate::exact_body;
use crate::{FetchPriority, FetchWindow};

#[derive(Debug)]
pub struct FrameResult {
    pub page_hash: PageHash,
    pub result: Result<(), MirageError>,
}

pub async fn fetch_window<B: ObjectBackend>(
    backend: &B,
    window: FetchWindow,
    db: &Database,
    shard: Arc<ArenaShard>,
    index: &ResidentIndex,
    cancel: CancellationToken,
    maximum_window: u64,
) -> Result<Vec<FrameResult>, MirageError> {
    fetch_window_with_encryption(
        backend,
        window,
        db,
        shard,
        index,
        cancel,
        maximum_window,
        None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn fetch_window_with_encryption<B: ObjectBackend>(
    backend: &B,
    window: FetchWindow,
    db: &Database,
    shard: Arc<ArenaShard>,
    index: &ResidentIndex,
    cancel: CancellationToken,
    maximum_window: u64,
    encryption: Option<&PackReadEncryption>,
) -> Result<Vec<FrameResult>, MirageError> {
    let response = backend
        .read_range(
            &window.object,
            window.range,
            backend_class(window.priority),
            cancel.clone(),
        )
        .await
        .map_err(MirageError::from)?;
    let returned = response.requested_range;
    let body = response
        .collect_bounded(maximum_window)
        .await
        .map_err(MirageError::from)?;
    let body = exact_body(window.range, returned, body, maximum_window)?;
    let mut results = Vec::with_capacity(window.frames.len());
    for mapping in window.frames {
        let result = (|| {
            let start = usize::try_from(mapping.window_offset)
                .map_err(|_| MirageError::invalid_argument("frame offset exceeds address space"))?;
            let length = usize::try_from(mapping.encoded_length)
                .map_err(|_| MirageError::invalid_argument("frame length exceeds address space"))?;
            let end = start
                .checked_add(length)
                .filter(|end| *end <= body.len())
                .ok_or_else(|| {
                    MirageError::integrity_mismatch("frame mapping escapes response body")
                })?;
            let frame_offset = window
                .range
                .start()
                .checked_add(mapping.window_offset)
                .ok_or_else(|| MirageError::invalid_argument("frame offset overflows"))?;
            let decoded = decode_expected_with_encryption(
                &body[start..end],
                mapping.page_hash,
                frame_offset,
                encryption,
            )?;
            if cancel.is_cancelled() {
                return Err(MirageError::cancelled(
                    "fetch cancelled before cache insertion",
                ));
            }
            let record = match insert_page(
                db,
                Arc::clone(&shard),
                mapping.page_hash,
                &decoded.page.bytes,
                &(),
            )? {
                InsertOutcome::Inserted(record) | InsertOutcome::Existing(record) => record,
            };
            index.install(record, Arc::clone(&shard))
        })();
        results.push(FrameResult {
            page_hash: mapping.page_hash,
            result,
        });
    }
    Ok(results)
}
const fn backend_class(priority: FetchPriority) -> FetchClass {
    match priority {
        FetchPriority::P0Blocking => FetchClass::BlockingRead,
        FetchPriority::P1Mandatory => FetchClass::MandatoryAdmission,
        FetchPriority::P2Capsule => FetchClass::CapsuleAdmission,
        FetchPriority::P3Frontier => FetchClass::LiveFrontier,
        FetchPriority::P4ReadAhead => FetchClass::ReadAhead,
        FetchPriority::P5IdleWarm => FetchClass::IdleWarm,
        FetchPriority::P6Maintenance => FetchClass::Maintenance,
    }
}
