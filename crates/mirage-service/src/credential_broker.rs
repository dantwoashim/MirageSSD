use std::time::SystemTime;
use zeroize::Zeroizing;

/// Short-lived capability supplied by a per-user broker. Refresh tokens never cross this boundary.
pub struct AccessCapability {
    token: Zeroizing<String>,
    pub expires_at: SystemTime,
}
impl AccessCapability {
    pub fn new(token: Zeroizing<String>, expires_at: SystemTime) -> Self {
        Self { token, expires_at }
    }
    pub fn bearer(&self) -> &str {
        &self.token
    }
}
impl std::fmt::Debug for AccessCapability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccessCapability")
            .field("token", &"[REDACTED]")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

pub trait CredentialBroker: Send + Sync {
    fn access_capability(&self, account_id: &str) -> Result<AccessCapability, String>;
}
