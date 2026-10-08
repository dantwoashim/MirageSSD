use std::path::{Path, PathBuf};
use std::sync::Arc;

use mirage_types::MirageError;
use zeroize::{Zeroize, Zeroizing};

use crate::oauth::{account_id, refresh_access_token};
use crate::scope::DRIVE_FILE;
use crate::token_store::TokenStore;
use crate::{NativeHttpTransport, RetryingHttpTransport};

pub struct StoredDriveSession {
    pub transport: Arc<RetryingHttpTransport>,
    pub access_token: Zeroizing<String>,
    pub account_id: String,
}

/// Refreshes the current user's DPAPI-protected Drive grant without exposing a token to the UI.
pub async fn refresh_stored_session(
    token_path: Option<&Path>,
) -> Result<StoredDriveSession, MirageError> {
    let store = TokenStore::new(match token_path {
        Some(path) if path.is_absolute() => path.to_owned(),
        Some(_) => {
            return Err(MirageError::invalid_argument(
                "Drive token store path must be absolute",
            ));
        }
        None => default_token_path()?,
    });
    let (metadata, mut refresh) = store.load()?;
    if metadata.scopes != [DRIVE_FILE] {
        return Err(MirageError::backend_permission_denied(
            "stored Drive authorization does not have the exact drive.file scope",
        ));
    }
    let client_id = metadata.client_id.as_deref().ok_or_else(|| {
        MirageError::backend_unauthenticated(
            "legacy Drive login has no bound OAuth client ID; verify once with the CLI credentials file",
        )
    })?;
    let native = Arc::new(NativeHttpTransport::new().map_err(MirageError::from)?);
    let transport = Arc::new(RetryingHttpTransport::new(native, 5).map_err(MirageError::from)?);
    let grant = refresh_with_stored_secret(transport.as_ref(), &store, client_id, &refresh).await?;
    refresh.zeroize();
    let account = account_id(transport.as_ref(), &grant.access_token)
        .await
        .map_err(MirageError::from)?;
    if account != metadata.account_id {
        return Err(MirageError::backend_unauthenticated(
            "refreshed Drive account differs from the protected token record",
        ));
    }
    Ok(StoredDriveSession {
        transport,
        access_token: grant.access_token,
        account_id: account,
    })
}

pub fn default_token_path() -> Result<PathBuf, MirageError> {
    let local_app_data = std::env::var_os("LOCALAPPDATA")
        .ok_or_else(|| MirageError::invalid_argument("LOCALAPPDATA is unavailable"))?;
    Ok(PathBuf::from(local_app_data)
        .join("MirageSSD")
        .join("credentials")
        .join("drive-token.json"))
}

/// Refreshes with the stored client secret when the record carries one —
/// OAuth clients that bound a secret require it on every refresh.
async fn refresh_with_stored_secret(
    transport: &dyn crate::http::HttpTransport,
    store: &TokenStore,
    client_id: &str,
    refresh_token: &[u8],
) -> Result<crate::oauth::TokenGrant, MirageError> {
    let mut secret = store.load_client_secret_optional()?;
    let result = refresh_access_token(
        transport,
        client_id,
        secret
            .as_deref()
            .and_then(|bytes| std::str::from_utf8(bytes).ok()),
        refresh_token,
    )
    .await
    .map_err(MirageError::from);
    if let Some(secret) = &mut secret {
        secret.zeroize();
    }
    result
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use crate::http::{HttpRequest, HttpResponse};
    use mirage_backend::BackendError;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    struct RecordingTransport {
        requests: Mutex<VecDeque<HttpRequest>>,
    }

    #[async_trait::async_trait]
    impl crate::http::HttpTransport for RecordingTransport {
        async fn execute(&self, request: HttpRequest) -> Result<HttpResponse, BackendError> {
            self.requests.lock().unwrap().push_back(request);
            Ok(HttpResponse {
                status: 200,
                headers: Default::default(),
                body: bytes::Bytes::from_static(
                    br#"{"access_token":"at","expires_in":3600,"scope":"https://www.googleapis.com/auth/drive.file","token_type":"Bearer"}"#,
                ),
            })
        }
    }

    #[test]
    fn refresh_includes_bound_client_secret() {
        let directory = tempfile::tempdir().expect("tempdir");
        let store = TokenStore::new(directory.path().join("token.json"));
        let mut refresh = Zeroizing::new(b"refresh-secret".to_vec());
        store
            .save(
                "client.apps.googleusercontent.com",
                "acct",
                &["https://www.googleapis.com/auth/drive.file".to_owned()],
                1,
                &mut refresh,
            )
            .expect("save");
        let mut secret = Zeroizing::new(b"desktop-secret".to_vec());
        store
            .bind_client_secret("client.apps.googleusercontent.com", &mut secret)
            .expect("bind secret");
        let (_metadata, refresh) = store.load().expect("load");
        let transport = RecordingTransport {
            requests: Mutex::new(VecDeque::new()),
        };
        futures_executor::block_on(refresh_with_stored_secret(
            &transport,
            &store,
            "client.apps.googleusercontent.com",
            &refresh,
        ))
        .expect("refresh");
        let requests = transport.requests.lock().unwrap();
        let body = String::from_utf8(requests[0].body.to_vec()).unwrap();
        assert!(body.contains("client_secret=desktop-secret"), "{body}");
        assert!(body.contains("client_id=client.apps.googleusercontent.com"));
    }
}
