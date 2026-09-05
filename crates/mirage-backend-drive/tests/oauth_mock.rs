use async_trait::async_trait;
use bytes::Bytes;
use mirage_backend::{BackendError, BackendErrorClass};
use mirage_backend_drive::http::{HttpRequest, HttpResponse, HttpTransport};
use mirage_backend_drive::oauth::{OAuthAttempt, account_id, exchange_code, refresh_access_token};
use std::collections::{BTreeMap, VecDeque};
use std::sync::Mutex;

struct Mock(Mutex<VecDeque<HttpResponse>>);
#[async_trait]
impl HttpTransport for Mock {
    async fn execute(&self, _: HttpRequest) -> Result<HttpResponse, BackendError> {
        self.0
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| BackendError::permanent("mock exhausted"))
    }
}

#[test]
fn refresh_grant_is_secret_safe_and_accepts_no_rotated_refresh_token() {
    let success = HttpResponse {
        status: 200,
        headers: BTreeMap::new(),
        body: Bytes::from_static(
            br#"{"access_token":"new-access-secret","expires_in":3600,"scope":"https://www.googleapis.com/auth/drive.file","token_type":"Bearer"}"#,
        ),
    };
    let grant = futures_executor::block_on(refresh_access_token(
        &Mock(Mutex::new([success].into())),
        "client",
        Some("desktop-secret"),
        b"refresh-secret",
    ))
    .expect("refresh");
    assert_eq!(&*grant.access_token, "new-access-secret");
    assert!(grant.refresh_token.is_none());
    assert!(!format!("{grant:?}").contains("new-access-secret"));
}
#[test]
fn authorization_uses_state_pkce_and_narrow_scope() {
    let attempt = OAuthAttempt::new().unwrap();
    let url = attempt
        .authorization_url("client", "http://127.0.0.1:49152/callback")
        .unwrap();
    let query: BTreeMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(query["scope"], "https://www.googleapis.com/auth/drive.file");
    assert_eq!(query["code_challenge_method"], "S256");
    assert!(query["state"].len() >= 40 && query["code_challenge"].len() >= 40);
    assert_eq!(
        attempt
            .validate_callback("wrong", Some("code"), None)
            .unwrap_err()
            .class,
        BackendErrorClass::Authentication
    );
}
#[test]
fn token_success_and_errors_are_secret_safe() {
    let success = HttpResponse { status: 200, headers: BTreeMap::new(), body: Bytes::from_static(br#"{"access_token":"access-secret","expires_in":3600,"refresh_token":"refresh-secret","scope":"https://www.googleapis.com/auth/drive.file","token_type":"Bearer"}"#) };
    let grant = futures_executor::block_on(exchange_code(
        &Mock(Mutex::new([success].into())),
        "client",
        None,
        "code",
        "verifier",
        "http://127.0.0.1/callback",
    ))
    .unwrap();
    assert_eq!(&*grant.access_token, "access-secret");
    assert!(!format!("{grant:?}").contains("secret"));
    let denied = HttpResponse {
        status: 400,
        headers: BTreeMap::new(),
        body: Bytes::from_static(b"contains-secret"),
    };
    let error = futures_executor::block_on(exchange_code(
        &Mock(Mutex::new([denied].into())),
        "client",
        None,
        "code",
        "verifier",
        "http://127.0.0.1/callback",
    ))
    .unwrap_err();
    assert!(!error.to_string().contains("contains-secret"));
}

#[test]
fn drive_account_uses_stable_permission_id() {
    let response = HttpResponse {
        status: 200,
        headers: BTreeMap::new(),
        body: Bytes::from_static(br#"{"user":{"permissionId":"permission-123"}}"#),
    };
    let account = futures_executor::block_on(account_id(
        &Mock(Mutex::new([response].into())),
        "access-secret",
    ))
    .unwrap();
    assert_eq!(account, "permission-123");
}
