use crate::http::HttpTransport;
use crate::metadata::CompletedUpload;
use crate::resumable::{self, CHUNK_ALIGNMENT};
use bytes::Bytes;
use bytes::BytesMut;
use futures_util::StreamExt;
use mirage_backend::{BackendError, UploadSource};
use mirage_types::ContentHash;
use std::collections::BTreeMap;

const UPLOAD_CHUNK_BYTES: u64 = CHUNK_ALIGNMENT * 32;

/// Streams an upload body through a resumable session in aligned chunks so
/// peak memory stays proportional to one chunk, not the object. The content
/// hash is verified before the final chunk is sent: a mismatch abandons the
/// session without ever creating the remote object.
pub async fn upload_stream(
    transport: &dyn HttpTransport,
    access_token: &str,
    name: &str,
    properties: &BTreeMap<String, String>,
    source: UploadSource,
    expected_hash: ContentHash,
) -> Result<CompletedUpload, BackendError> {
    let total = source.length.as_u64();
    let mut session = resumable::start(transport, access_token, name, properties, total).await?;
    let mut stream = source.into_stream();
    let mut hasher = blake3::Hasher::new();
    let mut buffered = BytesMut::new();
    let mut completed = None;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        hasher.update(&chunk);
        buffered.extend_from_slice(&chunk);
        // Flush full aligned chunks eagerly, but never send the final chunk
        // before the content hash has been verified.
        while buffered.len() as u64 >= UPLOAD_CHUNK_BYTES
            && session.committed + UPLOAD_CHUNK_BYTES < session.total
        {
            let piece = buffered.split_to(UPLOAD_CHUNK_BYTES as usize).freeze();
            if let Some(done) = resumable::upload_chunk(transport, &mut session, piece).await? {
                completed = Some(done);
            }
        }
    }
    if hasher.finalize().as_bytes() != expected_hash.as_bytes() {
        return Err(BackendError::integrity(
            "Drive upload source hash mismatch; resumable session abandoned incomplete",
        ));
    }
    while session.committed < session.total {
        let remaining = session.total - session.committed;
        if buffered.is_empty() {
            return Err(BackendError::integrity(
                "Drive upload stream ended before its declared length",
            ));
        }
        let length = (buffered.len() as u64)
            .min(remaining)
            .min(UPLOAD_CHUNK_BYTES) as usize;
        let piece = buffered.split_to(length).freeze();
        if let Some(done) = resumable::upload_chunk(transport, &mut session, piece).await? {
            completed = Some(done);
        }
    }
    if !buffered.is_empty() {
        return Err(BackendError::integrity(
            "Drive upload stream produced more bytes than declared",
        ));
    }
    completed
        .ok_or_else(|| BackendError::integrity("Drive upload ended without completion metadata"))
}

/// Buffered convenience wrapper retained for small metadata bodies.
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
