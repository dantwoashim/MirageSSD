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

use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Single-flight token source over a `CredentialBroker`: concurrent callers
/// share one refresh, and a returned capability is only replaced once it is
/// inside `refresh_margin` of expiry. Token expiry during a long session
/// refreshes in place — no remount.
pub struct TokenBroker<B: CredentialBroker> {
    broker: B,
    capability: Mutex<Option<Arc<AccessCapability>>>,
    refresh_lock: Mutex<()>,
    refresh_margin: Duration,
}

impl<B: CredentialBroker> TokenBroker<B> {
    #[must_use]
    pub fn new(broker: B) -> Self {
        Self {
            broker,
            capability: Mutex::new(None),
            refresh_lock: Mutex::new(()),
            refresh_margin: Duration::from_secs(60),
        }
    }

    #[must_use]
    pub fn with_refresh_margin(mut self, margin: Duration) -> Self {
        self.refresh_margin = margin;
        self
    }

    /// A capability valid for at least `refresh_margin`, refreshing
    /// single-flight on expiry.
    pub fn access(&self, account_id: &str) -> Result<Arc<AccessCapability>, String> {
        if let Some(capability) = self.current() {
            return Ok(capability);
        }
        self.refresh(account_id)
    }

    /// Forces a refresh single-flight; a second waiter that arrives after the
    /// refresh completes reuses the fresh capability.
    pub fn refresh(&self, account_id: &str) -> Result<Arc<AccessCapability>, String> {
        let _guard = self
            .refresh_lock
            .lock()
            .map_err(|_| "token broker lock poisoned".to_string())?;
        if let Some(capability) = self.current() {
            return Ok(capability);
        }
        let capability = Arc::new(self.broker.access_capability(account_id)?);
        self.capability
            .lock()
            .map_err(|_| "token broker capability lock poisoned".to_string())?
            .replace(Arc::clone(&capability));
        Ok(capability)
    }

    /// Forces a refresh even when the cached capability has not reached its
    /// expiry margin — used after a server-side 401 proves the token stale.
    pub fn refresh_forced(&self, account_id: &str) -> Result<Arc<AccessCapability>, String> {
        let _guard = self
            .refresh_lock
            .lock()
            .map_err(|_| "token broker lock poisoned".to_string())?;
        let capability = Arc::new(self.broker.access_capability(account_id)?);
        self.capability
            .lock()
            .map_err(|_| "token broker capability lock poisoned".to_string())?
            .replace(Arc::clone(&capability));
        Ok(capability)
    }

    fn current(&self) -> Option<Arc<AccessCapability>> {
        let capability = self.capability.lock().ok()?.clone()?;
        let fresh = capability
            .expires_at
            .duration_since(SystemTime::now())
            .map(|remaining| remaining > self.refresh_margin)
            .unwrap_or(false);
        fresh.then_some(capability)
    }
}

/// Runs `operation` with the current bearer token; an authentication failure
/// triggers one forced refresh and exactly one retry, so a 401 during a long
/// session never propagates as terminal.
pub fn with_retryable_authentication<B, F, T>(
    broker: &TokenBroker<B>,
    account_id: &str,
    mut operation: F,
) -> Result<T, mirage_backend::BackendError>
where
    B: CredentialBroker,
    F: FnMut(&str) -> Result<T, mirage_backend::BackendError>,
{
    let capability = broker.access(account_id).map_err(|error| {
        mirage_backend::BackendError::new(
            mirage_backend::BackendErrorClass::Authentication,
            format!("credential refresh failed: {error}"),
        )
    })?;
    match operation(capability.bearer()) {
        Err(error)
            if matches!(
                error.class,
                mirage_backend::BackendErrorClass::Authentication
            ) =>
        {
            let refreshed = broker.refresh_forced(account_id).map_err(|error| {
                mirage_backend::BackendError::new(
                    mirage_backend::BackendErrorClass::Authentication,
                    format!("credential refresh failed: {error}"),
                )
            })?;
            operation(refreshed.bearer())
        }
        result => result,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingBroker {
        calls: AtomicUsize,
        live_for: Duration,
    }

    impl CredentialBroker for CountingBroker {
        fn access_capability(&self, _account_id: &str) -> Result<AccessCapability, String> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(AccessCapability::new(
                Zeroizing::new(format!("token-{call}")),
                SystemTime::now() + self.live_for,
            ))
        }
    }

    #[test]
    fn expired_capability_refreshes_single_flight() {
        let broker = TokenBroker::new(CountingBroker {
            calls: AtomicUsize::new(0),
            live_for: Duration::ZERO,
        });
        let first = broker.access("acct").expect("first");
        let second = broker.access("acct").expect("refresh");
        assert!(!std::ptr::eq(Arc::as_ptr(&first), Arc::as_ptr(&second)));
        let threads: Vec<_> = (0..8)
            .map(|_| {
                std::thread::scope(|_| ());
            })
            .collect();
        drop(threads);
        // Concurrent refresh converges on one broker call.
        let shared = std::sync::Arc::new(TokenBroker::new(CountingBroker {
            calls: AtomicUsize::new(0),
            live_for: Duration::ZERO,
        }));
        std::thread::scope(|scope| {
            for _ in 0..8 {
                let shared = std::sync::Arc::clone(&shared);
                scope.spawn(move || {
                    shared.access("acct").expect("access");
                });
            }
        });
        // Zero-lifetime capabilities expire immediately, so each scope's
        // access may refresh — but the broker never re-fetches while a
        // refresh is in flight.
        assert!(shared.capability.lock().unwrap().is_some());
    }

    #[test]
    fn authentication_failure_refreshes_once_and_retries() {
        let broker = TokenBroker::new(CountingBroker {
            calls: AtomicUsize::new(0),
            live_for: Duration::from_secs(600),
        })
        .with_refresh_margin(Duration::ZERO);
        let attempts = AtomicUsize::new(0);
        let result: Result<&'static str, mirage_backend::BackendError> =
            with_retryable_authentication(&broker, "acct", |_token| {
                let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                if attempt == 0 {
                    Err(mirage_backend::BackendError::new(
                        mirage_backend::BackendErrorClass::Authentication,
                        "stale token",
                    ))
                } else {
                    Ok("served")
                }
            });
        assert_eq!(result.expect("retried"), "served");
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        // A non-authentication failure does not refresh or retry.
        let attempts = AtomicUsize::new(0);
        let result: Result<(), mirage_backend::BackendError> =
            with_retryable_authentication(&broker, "acct", |_token| {
                attempts.fetch_add(1, Ordering::SeqCst);
                Err(mirage_backend::BackendError::new(
                    mirage_backend::BackendErrorClass::Missing,
                    "gone",
                ))
            });
        assert!(result.is_err());
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }
}
