use async_trait::async_trait;
use bytes::Bytes;
use mirage_backend::{BackendError, BackendErrorClass};
use mirage_backend_drive::discover::enumerate_commits;
use mirage_backend_drive::http::{HttpRequest, HttpResponse, HttpTransport};
use mirage_types::RepositoryId;
use std::collections::VecDeque;
use std::sync::Mutex;

struct Mock {
    responses: Mutex<VecDeque<HttpResponse>>,
    urls: Mutex<Vec<String>>,
}
#[async_trait]
impl HttpTransport for Mock {
    async fn execute(&self, request: HttpRequest) -> Result<HttpResponse, BackendError> {
        self.urls.lock().unwrap().push(request.url);
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| BackendError::permanent("mock exhausted"))
    }
}
fn ok(body: &'static [u8]) -> HttpResponse {
    HttpResponse {
        status: 200,
        headers: Default::default(),
        body: Bytes::from_static(body),
    }
}
#[test]
fn pagination_is_bounded_filtered_and_deterministic() {
    let repo = RepositoryId::from_bytes([3; 16]);
    let hash = "07".repeat(32);
    let first = format!(
        r#"{{"files":[{{"id":"z-file","size":"64","headRevisionId":"r2","appProperties":{{"mirage_repository":"{repo}","mirage_kind":"commit","mirage_hash":"{hash}"}}}}],"nextPageToken":"next"}}"#
    );
    let second = format!(
        r#"{{"files":[{{"id":"a-file","size":"32","appProperties":{{"mirage_repository":"{repo}","mirage_kind":"commit","mirage_hash":"{hash}"}}}}]}}"#
    );
    let mock = Mock {
        responses: Mutex::new(
            [
                ok(Box::leak(first.into_bytes().into_boxed_slice())),
                ok(Box::leak(second.into_bytes().into_boxed_slice())),
            ]
            .into(),
        ),
        urls: Mutex::new(Vec::new()),
    };
    let objects = futures_executor::block_on(enumerate_commits(&mock, "secret", repo)).unwrap();
    assert_eq!(objects.len(), 2);
    assert_eq!(objects[0].provider_object_id.as_str(), "a-file");
    assert!(mock.urls.lock().unwrap()[1].contains("pageToken=next"));
}
#[test]
fn contradictory_metadata_is_rejected() {
    let repo = RepositoryId::from_bytes([4; 16]);
    let body = br#"{"files":[{"id":"x","size":"1","appProperties":{"mirage_repository":"wrong","mirage_kind":"commit","mirage_hash":"0707070707070707070707070707070707070707070707070707070707070707"}}]}"#;
    let mock = Mock {
        responses: Mutex::new([ok(body)].into()),
        urls: Mutex::new(Vec::new()),
    };
    assert_eq!(
        futures_executor::block_on(enumerate_commits(&mock, "secret", repo))
            .unwrap_err()
            .class,
        BackendErrorClass::Integrity
    );
}
