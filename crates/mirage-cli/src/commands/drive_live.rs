use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use mirage_backend_drive::{
    NativeHttpTransport, RetryingHttpTransport,
    oauth::{account_id, refresh_access_token},
    quota::{StorageQuota, storage_quota},
    scope::DRIVE_FILE,
    token_store::TokenStore,
};
use mirage_types::MirageError;
use serde::Deserialize;
use zeroize::{Zeroize, Zeroizing};

use crate::output;

const MAX_CLIENT_CREDENTIAL_BYTES: u64 = 64 * 1024;

#[derive(Deserialize)]
struct GoogleClientFile {
    installed: GoogleInstalledClient,
}

#[derive(Deserialize)]
struct GoogleInstalledClient {
    client_id: String,
    client_secret: String,
}

pub struct LiveDriveSession {
    pub transport: Arc<RetryingHttpTransport>,
    pub access_token: Zeroizing<String>,
    pub account_id: String,
    pub quota: StorageQuota,
}

pub fn connect(
    client_credentials: &Path,
    token_store: Option<&Path>,
) -> Result<LiveDriveSession, MirageError> {
    futures_executor::block_on(connect_async(client_credentials, token_store))
}

pub async fn connect_async(
    client_credentials: &Path,
    token_store: Option<&Path>,
) -> Result<LiveDriveSession, MirageError> {
    if !client_credentials.is_absolute() {
        return Err(MirageError::invalid_argument(
            "OAuth client credential path must be absolute",
        ));
    }
    let metadata = std::fs::metadata(client_credentials).map_err(MirageError::from)?;
    if metadata.len() == 0 || metadata.len() > MAX_CLIENT_CREDENTIAL_BYTES {
        return Err(MirageError::invalid_argument(
            "OAuth Desktop client JSON size is invalid",
        ));
    }
    let credentials: GoogleClientFile =
        serde_json::from_slice(&std::fs::read(client_credentials).map_err(MirageError::from)?)
            .map_err(|_| MirageError::invalid_argument("OAuth Desktop client JSON is invalid"))?;
    if !credentials
        .installed
        .client_id
        .ends_with(".apps.googleusercontent.com")
        || credentials.installed.client_secret.is_empty()
    {
        return Err(MirageError::invalid_argument(
            "OAuth Desktop client credentials are invalid",
        ));
    }
    let store = TokenStore::new(resolve_token_path(token_store)?);
    let (stored, mut refresh) = store.load()?;
    if stored.scopes != [DRIVE_FILE] {
        return Err(MirageError::backend_permission_denied(
            "stored Drive authorization does not have the exact drive.file scope",
        ));
    }
    let native = Arc::new(NativeHttpTransport::new().map_err(MirageError::from)?);
    let retrying = Arc::new(RetryingHttpTransport::new(native, 5).map_err(MirageError::from)?);
    let grant = refresh_access_token(
        retrying.as_ref(),
        &credentials.installed.client_id,
        Some(&credentials.installed.client_secret),
        &refresh,
    )
    .await
    .map_err(MirageError::from)?;
    refresh.zeroize();
    let account = account_id(retrying.as_ref(), &grant.access_token)
        .await
        .map_err(MirageError::from)?;
    if account != stored.account_id {
        return Err(MirageError::backend_unauthenticated(
            "refreshed Drive account differs from the protected token record",
        ));
    }
    store.bind_client_id(&credentials.installed.client_id)?;
    let quota = storage_quota(retrying.as_ref(), &grant.access_token)
        .await
        .map_err(MirageError::from)?;
    Ok(LiveDriveSession {
        transport: retrying,
        access_token: grant.access_token,
        account_id: account,
        quota,
    })
}

pub fn verify(
    client_credentials: &Path,
    token_store: Option<&Path>,
    json: bool,
) -> Result<(), MirageError> {
    let session = connect(client_credentials, token_store)?;
    let available = session
        .quota
        .limit
        .map(|limit| limit.saturating_sub(session.quota.usage));
    if json {
        output::emit_success(&serde_json::json!({
            "provider": "google-drive",
            "authenticated": true,
            "account_id": session.account_id,
            "scope": DRIVE_FILE,
            "quota_limit_bytes": session.quota.limit,
            "quota_usage_bytes": session.quota.usage,
            "quota_available_bytes": available,
            "token_refresh": "verified",
            "requests": session.transport.request_count(),
            "retries": session.transport.retry_count()
        }))
    } else {
        println!("Google Drive live authentication and token refresh verified.");
        println!("Account ID: {}", session.account_id);
        println!(
            "Available quota: {}",
            available.map_or_else(|| "unlimited".into(), |bytes| bytes.to_string())
        );
        Ok(())
    }
}

pub fn resolve_token_path(explicit: Option<&Path>) -> Result<PathBuf, MirageError> {
    if let Some(path) = explicit {
        if !path.is_absolute() {
            return Err(MirageError::invalid_argument(
                "token store path must be absolute",
            ));
        }
        return Ok(path.to_owned());
    }
    let local_app_data = std::env::var_os("LOCALAPPDATA")
        .ok_or_else(|| MirageError::invalid_argument("LOCALAPPDATA is unavailable"))?;
    Ok(PathBuf::from(local_app_data)
        .join("MirageSSD")
        .join("credentials")
        .join("drive-token.json"))
}
