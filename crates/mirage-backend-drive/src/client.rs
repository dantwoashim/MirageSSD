use crate::http::HttpTransport;
use std::sync::Arc;
use zeroize::Zeroizing;

use mirage_backend::{BackendError, BackendRead, RemoteObjectRef};
use mirage_types::CheckedRange;
use tokio_util::sync::CancellationToken;

pub struct DriveClient {
    pub(crate) transport: Arc<dyn HttpTransport>,
    access_token: Zeroizing<String>,
}
impl DriveClient {
    pub fn new(transport: Arc<dyn HttpTransport>, access_token: Zeroizing<String>) -> Self {
        Self {
            transport,
            access_token,
        }
    }
    pub(crate) fn access_token(&self) -> &str {
        &self.access_token
    }

    pub async fn read_range(
        &self,
        object: &RemoteObjectRef,
        range: CheckedRange,
        cancel: &CancellationToken,
    ) -> Result<BackendRead, BackendError> {
        crate::read::read_exact(
            self.transport.as_ref(),
            self.access_token(),
            object,
            range,
            cancel,
        )
        .await
    }
}
impl std::fmt::Debug for DriveClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DriveClient")
            .field("access_token", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}
