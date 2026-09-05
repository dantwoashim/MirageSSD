use crate::http::{HttpRequest, HttpTransport, Method};
use bytes::Bytes;
use mirage_backend::{
    BackendByteStream, BackendError, BackendErrorClass, BackendRead, BackendResponseMetadata,
    RemoteObjectRef,
};
use mirage_types::{ByteCount, CheckedRange};
use std::collections::BTreeMap;
use tokio_util::sync::CancellationToken;

pub async fn read_exact(
    transport: &dyn HttpTransport,
    access_token: &str,
    object: &RemoteObjectRef,
    range: CheckedRange,
    cancel: &CancellationToken,
) -> Result<BackendRead, BackendError> {
    if cancel.is_cancelled() {
        return Err(BackendError::new(
            BackendErrorClass::TransientTransport,
            "Drive range request was cancelled",
        ));
    }
    let end = range
        .end_exclusive()
        .checked_sub(1)
        .ok_or_else(|| BackendError::permanent("empty ranges are forbidden"))?;
    let mut headers = BTreeMap::new();
    headers.insert("authorization".into(), format!("Bearer {access_token}"));
    headers.insert("range".into(), format!("bytes={}-{}", range.start(), end));
    let url = format!(
        "https://www.googleapis.com/drive/v3/files/{}?alt=media",
        object.provider_object_id.as_str()
    );
    let response = transport
        .execute(HttpRequest {
            method: Method::Get,
            url,
            headers,
            body: Bytes::new(),
        })
        .await?;
    if cancel.is_cancelled() {
        return Err(BackendError::new(
            BackendErrorClass::TransientTransport,
            "Drive range request was cancelled",
        ));
    }
    if response.status != 206 {
        return Err(if response.status == 200 {
            BackendError::integrity("Drive ignored the bounded range request")
        } else {
            crate::error::classify_response(&response)
        });
    }
    let content_range = response
        .header("content-range")
        .ok_or_else(|| BackendError::integrity("Drive omitted Content-Range"))?;
    crate::response::validate_content_range(content_range, range.start(), end)?;
    if response.body.len() as u64 != range.len() {
        return Err(BackendError::integrity(
            "Drive range body length was incorrect",
        ));
    }
    let length = ByteCount::from_u64(range.len());
    BackendRead::new(
        range,
        length,
        BackendResponseMetadata {
            provider_request_id: response.header("x-guploader-uploadid").map(str::to_owned),
            observed_revision: object.immutable_revision.clone(),
            transport_status: Some(206),
        },
        BackendByteStream::from_bytes(response.body),
    )
}
