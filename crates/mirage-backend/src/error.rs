use std::error::Error;
use std::fmt;
use std::time::Duration;

use mirage_types::{MirageError, MirageErrorKind, RetryDisposition};
use serde::{Deserialize, Serialize};

/// Provider-independent backend failure classes used by retry and escalation policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendErrorClass {
    Authentication,
    Permission,
    RateLimit,
    Missing,
    TransientTransport,
    Integrity,
    Unsupported,
    Permanent,
}

/// Backend failure with a safe public summary and optional diagnostic cause.
#[derive(Debug)]
pub struct BackendError {
    pub class: BackendErrorClass,
    pub safe_message: String,
    pub retry_after: Option<Duration>,
    source: Option<Box<dyn Error + Send + Sync + 'static>>,
}

impl BackendError {
    #[must_use]
    pub fn new(class: BackendErrorClass, safe_message: impl Into<String>) -> Self {
        Self {
            class,
            safe_message: safe_message.into(),
            retry_after: None,
            source: None,
        }
    }

    #[must_use]
    pub fn with_retry_after(mut self, duration: Duration) -> Self {
        self.retry_after = Some(duration);
        self
    }

    #[must_use]
    pub fn with_source(
        mut self,
        source: impl Into<Box<dyn Error + Send + Sync + 'static>>,
    ) -> Self {
        self.source = Some(source.into());
        self
    }

    #[must_use]
    pub fn integrity(safe_message: impl Into<String>) -> Self {
        Self::new(BackendErrorClass::Integrity, safe_message)
    }

    #[must_use]
    pub fn missing(safe_message: impl Into<String>) -> Self {
        Self::new(BackendErrorClass::Missing, safe_message)
    }

    #[must_use]
    pub fn permanent(safe_message: impl Into<String>) -> Self {
        Self::new(BackendErrorClass::Permanent, safe_message)
    }
}

impl fmt::Display for BackendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}: {}", self.class, self.safe_message)
    }
}

impl Error for BackendError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source.as_deref().map(|source| source as &dyn Error)
    }
}

impl From<BackendError> for MirageError {
    fn from(error: BackendError) -> Self {
        let message = error.safe_message.clone();
        let mapped = match error.class {
            BackendErrorClass::Authentication => MirageError::backend_unauthenticated(message),
            BackendErrorClass::Permission => MirageError::backend_permission_denied(message),
            BackendErrorClass::RateLimit => {
                MirageError::backend_rate_limited(message, error.retry_after)
            }
            BackendErrorClass::Missing => MirageError::remote_object_missing(message),
            BackendErrorClass::TransientTransport => MirageError::backend_unavailable(message),
            BackendErrorClass::Integrity => MirageError::integrity_mismatch(message),
            BackendErrorClass::Unsupported => MirageError::provider_unavailable(message),
            BackendErrorClass::Permanent => MirageError::new(
                MirageErrorKind::BackendUnavailable,
                MirageErrorKind::BackendUnavailable.default_code(),
                message,
            )
            .with_retry(RetryDisposition::Never),
        };
        mapped.with_source(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_class_maps_to_the_expected_public_kind() {
        let cases = [
            (
                BackendErrorClass::Authentication,
                MirageErrorKind::BackendUnauthenticated,
            ),
            (
                BackendErrorClass::Permission,
                MirageErrorKind::BackendPermissionDenied,
            ),
            (
                BackendErrorClass::RateLimit,
                MirageErrorKind::BackendRateLimited,
            ),
            (
                BackendErrorClass::Missing,
                MirageErrorKind::RemoteObjectMissing,
            ),
            (
                BackendErrorClass::TransientTransport,
                MirageErrorKind::BackendUnavailable,
            ),
            (
                BackendErrorClass::Integrity,
                MirageErrorKind::IntegrityMismatch,
            ),
            (
                BackendErrorClass::Unsupported,
                MirageErrorKind::ProviderUnavailable,
            ),
            (
                BackendErrorClass::Permanent,
                MirageErrorKind::BackendUnavailable,
            ),
        ];
        for (class, expected) in cases {
            let error: MirageError = BackendError::new(class, "safe backend failure").into();
            assert_eq!(error.kind, expected);
        }
    }
}
