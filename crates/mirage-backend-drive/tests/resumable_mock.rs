use async_trait::async_trait;
use bytes::Bytes;
use mirage_backend::BackendError;
use mirage_backend_drive::http::{HttpRequest, HttpResponse, HttpTransport};
use mirage_backend_drive::resumable::{self, CHUNK_ALIGNMENT, ResumableSession};
use std::collections::{BTreeMap, VecDeque};
use std::sync::Mutex;

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
fn response(status: u16, headers: &[(&str, &str)], body: &'static [u8]) -> HttpResponse {
    HttpResponse {
        status,
        headers: headers
            .iter()
            .map(|(k, v)| ((*k).into(), (*v).into()))
            .collect(),
        body: Bytes::from_static(body),
    }
}

#[test]
fn interrupted_upload_queries_and_resumes_exact_confirmed_prefix() {
    let total = CHUNK_ALIGNMENT * 2;
    let mock = Mock { responses: Mutex::new([response(200, &[("Location", "https://upload.example/session")], b""), response(308, &[("Range", "bytes=0-262143")], b""), response(308, &[("Range", "bytes=0-262143")], b""), response(200, &[], br#"{"id":"file-1","size":"524288","md5Checksum":"abc","headRevisionId":"rev-1"}"#)].into()), requests: Mutex::new(Vec::new()) };
    let mut session = futures_executor::block_on(resumable::start(
        &mock,
        "token",
        "pack",
        &BTreeMap::new(),
        total,
    ))
    .unwrap();
    assert!(
        futures_executor::block_on(resumable::upload_chunk(
            &mock,
            &mut session,
            Bytes::from(vec![1; CHUNK_ALIGNMENT as usize])
        ))
        .unwrap()
        .is_none()
    );
    let persisted = session.clone();
    assert_eq!(
        futures_executor::block_on(resumable::query(&mock, &persisted)).unwrap(),
        CHUNK_ALIGNMENT
    );
    let completed = futures_executor::block_on(resumable::upload_chunk(
        &mock,
        &mut session,
        Bytes::from(vec![2; CHUNK_ALIGNMENT as usize]),
    ))
    .unwrap()
    .unwrap();
    assert_eq!(completed.file_id.as_str(), "file-1");
    assert_eq!(session.committed, total);
    let requests = mock.requests.lock().unwrap();
    assert_eq!(
        requests[1].headers["content-range"],
        "bytes 0-262143/524288"
    );
    assert_eq!(
        requests[3].headers["content-range"],
        "bytes 262144-524287/524288"
    );
    assert!(!format!("{:?}", requests[0]).contains("token"));
}

#[test]
fn offset_disagreement_and_expired_session_fail_closed() {
    let mock = Mock {
        responses: Mutex::new([response(308, &[("Range", "bytes=0-10")], b"")].into()),
        requests: Mutex::new(Vec::new()),
    };
    let mut session = ResumableSession {
        uri: "https://upload.example/session".into(),
        committed: 0,
        total: CHUNK_ALIGNMENT * 2,
    };
    assert!(
        futures_executor::block_on(resumable::upload_chunk(
            &mock,
            &mut session,
            Bytes::from(vec![0; CHUNK_ALIGNMENT as usize])
        ))
        .is_err()
    );
    let expired = Mock {
        responses: Mutex::new([response(410, &[], b"")].into()),
        requests: Mutex::new(Vec::new()),
    };
    assert!(futures_executor::block_on(resumable::query(&expired, &session)).is_err());
}
