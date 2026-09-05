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
    let grant = refresh_access_token(transport.as_ref(), client_id, None, &refresh)
        .await
        .map_err(MirageError::from)?;
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
