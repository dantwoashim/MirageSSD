use async_trait::async_trait;
use bytes::Bytes;
use mirage_backend::{BackendId, ObjectBackend, RemoteObjectRef};
use mirage_backend_drive::RefreshableDriveBackend;
use mirage_backend_drive::http::{HttpRequest, HttpResponse, HttpTransport};
use mirage_types::{ByteCount, CheckedRange, ContentHash, RepositoryId};
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
    async fn execute(
        &self,
        request: HttpRequest,
    ) -> Result<HttpResponse, mirage_backend::BackendError> {
        self.requests.lock().unwrap().push(request);
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| mirage_backend::BackendError::permanent("mock exhausted"))
    }
}

fn object() -> RemoteObjectRef {
    RemoteObjectRef {
        backend_id: BackendId::new("drive").unwrap(),
        provider_object_id: mirage_backend::ProviderObjectId::new("file-id").unwrap(),
        immutable_revision: None,
        byte_length: ByteCount::from_u64(4),
        content_hash: ContentHash::from_bytes([7; 32]),
        kind: mirage_backend::ObjectKind::Pack,
    }
}

fn range_response() -> HttpResponse {
    HttpResponse {
        status: 206,
        headers: [("content-range".to_owned(), "bytes 0-3/4".to_owned())]
            .into_iter()
            .collect(),
        body: Bytes::from_static(b"DATA"),
    }
}

#[test]
fn replace_token_switches_the_bearer_for_subsequent_reads() {
    let mock = Arc::new(Mock {
        responses: Mutex::new(VecDeque::new()),
        requests: Mutex::new(Vec::new()),
    });
    let factory_mock = Arc::clone(&mock);
    let backend = RefreshableDriveBackend::with_transport_factory(
        Zeroizing::new("old-token".to_owned()),
        RepositoryId::from_bytes([1; 16]),
        Arc::new(move || Ok(Arc::clone(&factory_mock) as Arc<dyn HttpTransport>)),
        5,
    )
    .expect("refreshable backend");
    let object = object();
    let range = CheckedRange::new(0, 4).unwrap();
    let cancel = CancellationToken::new();

    mock.responses.lock().unwrap().push_back(range_response());
    let read = futures_executor::block_on(backend.read_range(
        &object,
        range,
        mirage_backend::FetchClass::MandatoryAdmission,
        cancel.clone(),
    ))
    .expect("first read");
    let body = futures_executor::block_on(read.collect_bounded(4)).expect("body");
    assert_eq!(body.as_ref(), b"DATA");
    let auth = mock.requests.lock().unwrap()[0]
        .headers
        .get("authorization")
        .cloned()
        .unwrap();
    assert_eq!(auth, "Bearer old-token");

    backend
        .replace_token(Zeroizing::new("new-token".to_owned()))
        .expect("replace token");
    mock.responses.lock().unwrap().push_back(range_response());
    futures_executor::block_on(backend.read_range(
        &object,
        range,
        mirage_backend::FetchClass::MandatoryAdmission,
        cancel,
    ))
    .expect("second read");
    let auth = mock.requests.lock().unwrap()[1]
        .headers
        .get("authorization")
        .cloned()
        .unwrap();
    assert_eq!(auth, "Bearer new-token");
}
