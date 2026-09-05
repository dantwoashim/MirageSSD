use bytes::Bytes;
use mirage_backend::BackendErrorClass;
use mirage_backend_drive::HttpResponse;
use mirage_backend_drive::error::classify_response;
use std::collections::BTreeMap;

fn response(status: u16, body: &'static [u8], retry: Option<&str>) -> HttpResponse {
    let mut headers = BTreeMap::new();
    if let Some(value) = retry {
        headers.insert("Retry-After".into(), value.into());
    }
    HttpResponse {
        status,
        headers,
        body: Bytes::from_static(body),
    }
}
#[test]
fn classifications_are_scheduler_safe_and_body_is_not_exposed() {
    let cases = [
        (401, b"secret".as_slice(), BackendErrorClass::Authentication),
        (
            403,
            br#"{"reason":"insufficientPermissions"}"#,
            BackendErrorClass::Permission,
        ),
        (
            403,
            br#"{"reason":"userRateLimitExceeded"}"#,
            BackendErrorClass::RateLimit,
        ),
        (404, b"", BackendErrorClass::Missing),
        (429, b"", BackendErrorClass::RateLimit),
        (503, b"", BackendErrorClass::TransientTransport),
    ];
    for (status, body, class) in cases {
        let error = classify_response(&response(status, body, Some("17")));
        assert_eq!(error.class, class);
        assert!(!error.to_string().contains("secret"));
    }
    assert_eq!(
        classify_response(&response(429, b"", Some("17")))
            .retry_after
            .unwrap()
            .as_secs(),
        17
    );
    assert!(
        classify_response(&response(429, b"", Some("invalid")))
            .retry_after
            .is_none()
    );
}
