use async_trait::async_trait;
use bytes::Bytes;
use mirage_backend::{BackendError, ObjectBackend, ObjectKind, UploadSource};
use mirage_backend_drive::DriveObjectBackend;
use mirage_backend_drive::http::{HttpRequest, HttpResponse, HttpTransport};
use mirage_pack::format::{PACK_HEADER_LEN, PackHeader};
use mirage_types::{ContentHash, RepositoryId};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

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
fn response(status: u16, headers: &[(&str, &str)], body: Bytes) -> HttpResponse {
    HttpResponse {
        status,
        headers: headers
            .iter()
            .map(|(k, v)| ((*k).into(), (*v).into()))
            .collect(),
        body,
    }
}

#[test]
fn immutable_put_uploads_once_then_reuses_exact_provider_object() {
    let bytes = Bytes::from_static(b"immutable-drive-object");
    let hash = ContentHash::from_bytes(*blake3::hash(&bytes).as_bytes());
    let mock = Arc::new(Mock {
        responses: Mutex::new(
            [
                response(200, &[], Bytes::from_static(br#"{"files":[]}"#)),
                response(
                    200,
                    &[("Location", "https://upload.example/s")],
                    Bytes::new(),
                ),
                response(
                    200,
                    &[],
                    Bytes::from_static(
                        br#"{"id":"drive-file","size":"22","headRevisionId":"rev-1"}"#,
                    ),
                ),
                response(
                    200,
                    &[],
                    Bytes::from_static(
                        br#"{"files":[{"id":"drive-file","size":"22","headRevisionId":"rev-1"}]}"#,
                    ),
                ),
            ]
            .into(),
        ),
        requests: Mutex::new(Vec::new()),
    });
    let backend = DriveObjectBackend::new(
        mock.clone(),
        Zeroizing::new("secret-token".into()),
        RepositoryId::from_bytes([8; 16]),
    )
    .unwrap();
    let first = futures_executor::block_on(backend.put_immutable(
        ObjectKind::Manifest,
        UploadSource::from_bytes(bytes.clone()),
        hash,
        CancellationToken::new(),
    ))
    .unwrap();
    let second = futures_executor::block_on(backend.put_immutable(
        ObjectKind::Manifest,
        UploadSource::from_bytes(bytes),
        hash,
        CancellationToken::new(),
    ))
    .unwrap();
    assert_eq!(first, second);
    let requests = mock.requests.lock().unwrap();
    assert_eq!(requests.len(), 4);
    assert!(
        requests
            .iter()
            .all(|request| !format!("{request:?}").contains("secret-token"))
    );
}

#[test]
fn source_hash_mismatch_never_reaches_drive() {
    let mock = Arc::new(Mock {
        responses: Mutex::new(VecDeque::new()),
        requests: Mutex::new(Vec::new()),
    });
    let backend = DriveObjectBackend::new(
        mock.clone(),
        Zeroizing::new("token".into()),
        RepositoryId::from_bytes([9; 16]),
    )
    .unwrap();
    let result = futures_executor::block_on(backend.put_immutable(
        ObjectKind::Pack,
        UploadSource::from_bytes(Bytes::from_static(b"wrong")),
        ContentHash::from_bytes([1; 32]),
        CancellationToken::new(),
    ));
    assert!(result.is_err());
    assert!(mock.requests.lock().unwrap().is_empty());
}

#[test]
fn plaintext_pack_is_rejected_before_any_drive_request() {
    let mock = Arc::new(Mock {
        responses: Mutex::new(VecDeque::new()),
        requests: Mutex::new(Vec::new()),
    });
    let backend = DriveObjectBackend::new(
        mock.clone(),
        Zeroizing::new("token".into()),
        RepositoryId::from_bytes([10; 16]),
    )
    .unwrap();
    let header = PackHeader {
        page_size: 1024 * 1024,
        index_offset: PACK_HEADER_LEN as u64,
        index_length: 0,
        entry_count: 0,
        encrypted: false,
        pack_id: [0; 16],
    }
    .encode();
    let bytes = Bytes::copy_from_slice(&header);
    let hash = ContentHash::from_bytes(*blake3::hash(&bytes).as_bytes());
    let result = futures_executor::block_on(backend.put_immutable(
        ObjectKind::Pack,
        UploadSource::from_bytes(bytes),
        hash,
        CancellationToken::new(),
    ));
    assert!(result.is_err());
    assert!(mock.requests.lock().unwrap().is_empty());
}
