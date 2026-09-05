use crate::http::{HttpRequest, HttpTransport, Method};
use crate::metadata::{CompletedUpload, CreateMetadata, DriveFile};
use bytes::Bytes;
use mirage_backend::{BackendError, BackendErrorClass};
use std::collections::BTreeMap;

pub const CHUNK_ALIGNMENT: u64 = 256 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumableSession {
    pub uri: String,
    pub committed: u64,
    pub total: u64,
}

pub async fn start(
    transport: &dyn HttpTransport,
    access_token: &str,
    name: &str,
    properties: &BTreeMap<String, String>,
    total: u64,
) -> Result<ResumableSession, BackendError> {
    if total == 0 {
        return Err(BackendError::permanent("empty Drive objects are forbidden"));
    }
    let body = serde_json::to_vec(&CreateMetadata {
        name,
        app_properties: properties,
    })
    .map_err(|_| BackendError::permanent("Drive metadata serialization failed"))?;
    let headers = [
        ("authorization".into(), format!("Bearer {access_token}")),
        (
            "content-type".into(),
            "application/json; charset=UTF-8".into(),
        ),
        (
            "x-upload-content-type".into(),
            "application/octet-stream".into(),
        ),
        ("x-upload-content-length".into(), total.to_string()),
    ]
    .into();
    let response = transport.execute(HttpRequest { method: Method::Post, url: "https://www.googleapis.com/upload/drive/v3/files?uploadType=resumable&fields=id,size,md5Checksum,headRevisionId".into(), headers, body: body.into() }).await?;
    if response.status != 200 {
        return Err(crate::error::classify_response(&response));
    }
    let uri = response
        .header("location")
        .filter(|value| value.starts_with("https://"))
        .ok_or_else(|| BackendError::integrity("Drive omitted a secure resumable session URI"))?
        .to_owned();
    Ok(ResumableSession {
        uri,
        committed: 0,
        total,
    })
}

pub async fn query(
    transport: &dyn HttpTransport,
    session: &ResumableSession,
) -> Result<u64, BackendError> {
    let response = transport
        .execute(HttpRequest {
            method: Method::Put,
            url: session.uri.clone(),
            headers: [
                ("content-length".into(), "0".into()),
                ("content-range".into(), format!("bytes */{}", session.total)),
            ]
            .into(),
            body: Bytes::new(),
        })
        .await?;
    match response.status {
        308 => parse_committed(response.header("range"), session.total),
        200 | 201 => Ok(session.total),
        404 | 410 => Err(BackendError::new(
            BackendErrorClass::Missing,
            "Drive resumable session expired",
        )),
        _ => Err(crate::error::classify_response(&response)),
    }
}

pub async fn upload_chunk(
    transport: &dyn HttpTransport,
    session: &mut ResumableSession,
    bytes: Bytes,
) -> Result<Option<CompletedUpload>, BackendError> {
    if bytes.is_empty() || session.committed >= session.total {
        return Err(BackendError::permanent("invalid resumable upload chunk"));
    }
    let length = bytes.len() as u64;
    let end_exclusive = session
        .committed
        .checked_add(length)
        .ok_or_else(|| BackendError::permanent("upload offset overflow"))?;
    if end_exclusive > session.total
        || (end_exclusive < session.total && !length.is_multiple_of(CHUNK_ALIGNMENT))
    {
        return Err(BackendError::permanent(
            "Drive upload chunk is misaligned or oversized",
        ));
    }
    let end = end_exclusive - 1;
    let headers = [
        ("content-length".into(), length.to_string()),
        (
            "content-range".into(),
            format!("bytes {}-{}/{}", session.committed, end, session.total),
        ),
    ]
    .into();
    let response = transport
        .execute(HttpRequest {
            method: Method::Put,
            url: session.uri.clone(),
            headers,
            body: bytes,
        })
        .await?;
    match response.status {
        308 => {
            let confirmed = parse_committed(response.header("range"), session.total)?;
            if confirmed != end_exclusive {
                return Err(BackendError::integrity(
                    "Drive confirmed an unexpected upload offset",
                ));
            }
            session.committed = confirmed;
            Ok(None)
        }
        200 | 201 if end_exclusive == session.total => {
            let file: DriveFile = serde_json::from_slice(&response.body).map_err(|_| {
                BackendError::integrity("Drive upload completion metadata was invalid")
            })?;
            let completed = CompletedUpload::try_from(file)?;
            if completed.size != session.total {
                return Err(BackendError::integrity(
                    "Drive completed upload with wrong size",
                ));
            }
            session.committed = session.total;
            Ok(Some(completed))
        }
        200 | 201 => Err(BackendError::integrity(
            "Drive completed upload before all bytes were sent",
        )),
        _ => Err(crate::error::classify_response(&response)),
    }
}

fn parse_committed(range: Option<&str>, total: u64) -> Result<u64, BackendError> {
    let Some(value) = range else { return Ok(0) };
    let end = value
        .strip_prefix("bytes=0-")
        .ok_or_else(|| BackendError::integrity("Drive returned invalid resumable Range"))?
        .parse::<u64>()
        .map_err(|_| BackendError::integrity("Drive returned invalid resumable Range"))?;
    let committed = end
        .checked_add(1)
        .ok_or_else(|| BackendError::integrity("Drive resumable offset overflow"))?;
    if committed > total {
        return Err(BackendError::integrity(
            "Drive resumable offset exceeds object",
        ));
    }
    Ok(committed)
}
