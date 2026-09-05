use crate::http::HttpTransport;
use crate::metadata::CompletedUpload;
use crate::resumable::{self, CHUNK_ALIGNMENT};
use bytes::Bytes;
use mirage_backend::BackendError;
use std::collections::BTreeMap;

pub async fn upload_bytes(
    transport: &dyn HttpTransport,
    access_token: &str,
    name: &str,
    properties: &BTreeMap<String, String>,
    source: Bytes,
) -> Result<CompletedUpload, BackendError> {
    let total = source.len() as u64;
    let mut session = resumable::start(transport, access_token, name, properties, total).await?;
    while session.committed < total {
        let remaining = total - session.committed;
        let length = remaining.min(CHUNK_ALIGNMENT * 32) as usize;
        let start = session.committed as usize;
        let chunk = source.slice(start..start + length);
        if let Some(completed) = resumable::upload_chunk(transport, &mut session, chunk).await? {
            return Ok(completed);
        }
    }
    Err(BackendError::integrity(
        "Drive upload ended without completion metadata",
    ))
}
