use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use bytes::Bytes;
use mirage_backend::{
    BackendError, BackendErrorClass, BackendId, ImmutableRevision, ObjectKind, ProviderObjectId,
    RemoteObjectRef,
};
use mirage_backend_drive::{DriveClient, HttpRequest, HttpResponse, HttpTransport};
use mirage_types::{ByteCount, CheckedRange, ContentHash};
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

#[derive(Default)]
struct Mock {
    responses: Mutex<VecDeque<HttpResponse>>,
    requests: Mutex<Vec<HttpRequest>>,
}
#[async_trait]
impl HttpTransport for Mock {
    async fn execute(&self, request: HttpRequest) -> Result<HttpResponse, BackendError> {
        self.requests.lock().unwrap().push(request);
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| BackendError::permanent("mock exhausted"))
    }
}
fn object() -> RemoteObjectRef {
    RemoteObjectRef {
        backend_id: BackendId::new("drive").unwrap(),
        provider_object_id: ProviderObjectId::new("file-id-1").unwrap(),
        immutable_revision: Some(ImmutableRevision::new("rev-1").unwrap()),
        byte_length: ByteCount::from_u64(1024),
        content_hash: ContentHash::from_bytes([7; 32]),
        kind: ObjectKind::Pack,
    }
}
fn response(status: u16, range: Option<&str>, body: &'static [u8]) -> HttpResponse {
    let mut headers = BTreeMap::new();
    if let Some(value) = range {
        headers.insert("Content-Range".into(), value.into());
    }
    HttpResponse {
        status,
        headers,
        body: Bytes::from_static(body),
    }
}

#[test]
fn exact_206_is_accepted_and_authorization_is_redacted() {
    let mock = Arc::new(Mock::default());
    mock.responses
        .lock()
        .unwrap()
        .push_back(response(206, Some("bytes 10-13/1024"), b"abcd"));
    let client = DriveClient::new(mock.clone(), Zeroizing::new("secret-token".into()));
    let read = futures_executor::block_on(client.read_range(
        &object(),
        CheckedRange::new(10, 4).unwrap(),
        &CancellationToken::new(),
    ))
    .unwrap();
    assert_eq!(
        futures_executor::block_on(read.collect_bounded(4)).unwrap(),
        b"abcd"[..]
    );
    let request = &mock.requests.lock().unwrap()[0];
    assert_eq!(request.headers["range"], "bytes=10-13");
    assert!(!format!("{request:?}").contains("secret-token"));
}

#[test]
fn whole_short_malformed_and_cancelled_responses_are_rejected() {
    for response in [
        response(200, None, b"abcd"),
        response(206, Some("bytes 11-14/1024"), b"abcd"),
        response(206, Some("bytes 10-13/1024"), b"abc"),
    ] {
        let mock = Arc::new(Mock::default());
        mock.responses.lock().unwrap().push_back(response);
        let client = DriveClient::new(mock, Zeroizing::new("token".into()));
        let error = futures_executor::block_on(client.read_range(
            &object(),
            CheckedRange::new(10, 4).unwrap(),
            &CancellationToken::new(),
        ))
        .unwrap_err();
        assert_eq!(error.class, BackendErrorClass::Integrity);
    }
    let cancel = CancellationToken::new();
    cancel.cancel();
    let error = futures_executor::block_on(
        DriveClient::new(Arc::new(Mock::default()), Zeroizing::new("token".into())).read_range(
            &object(),
            CheckedRange::new(10, 4).unwrap(),
            &cancel,
        ),
    )
    .unwrap_err();
    assert_eq!(error.class, BackendErrorClass::TransientTransport);
}
