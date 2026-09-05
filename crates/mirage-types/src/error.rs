//! Error taxonomy, machine codes, and public error envelopes for MirageSSD.

use std::error::Error as StdError;
use std::fmt;

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use crate::retry::RetryDisposition;

/// Stable category for Mirage errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum MirageErrorKind {
    /// Invalid argument, bad parameter, or malformed input provided by caller.
    InvalidArgument,
    /// Unsupported archive/layout version, compression, or format structure.
    UnsupportedLayout,
    /// Manifest data is corrupted, malformed, or missing mandatory headers.
    ManifestInvalid,
    /// BLAKE3 checksum, cryptographic hash, or byte length mismatch detected.
    IntegrityMismatch,
    /// Sparse disk cache arena is full and cannot admit or evict requested pages.
    CacheFull,
    /// Backend service authentication failed or token/credentials are invalid/expired.
    BackendUnauthenticated,
    /// Backend credentials are valid but lack permission for the requested immutable object.
    BackendPermissionDenied,
    /// Backend service returned rate limiting / 429 quota exhaustion.
    BackendRateLimited,
    /// Backend service is temporarily unavailable, timed out, or connection failed.
    BackendUnavailable,
    /// Requested remote pack, object, manifest, or commit does not exist on the remote.
    RemoteObjectMissing,
    /// Concurrent writer conflict or diverged repository branch detected.
    RepositoryConflict,
    /// An active update transaction or lock prevents the requested operation.
    UpdateActive,
    /// The requested cloud provider or local storage provider is unavailable/unsupported.
    ProviderUnavailable,
    /// Underlying host filesystem or operating system I/O error occurred.
    Io,
    /// The operation was explicitly cancelled before completion.
    Cancelled,
    /// The operation could not complete before its caller-supplied deadline.
    DeadlineExceeded,
    /// The requested command or capability is part of the stable surface but is not implemented.
    NotImplemented,
    /// Internal invariant or assertion failure in the engine/cache subsystem.
    InternalInvariant,
}

impl MirageErrorKind {
    /// Returns the canonical machine-readable error code string for this error kind.
    #[must_use]
    pub const fn default_code(&self) -> &'static str {
        match self {
            Self::InvalidArgument => "MIRAGE_INVALID_ARGUMENT",
            Self::UnsupportedLayout => "MIRAGE_UNSUPPORTED_LAYOUT",
            Self::ManifestInvalid => "MIRAGE_MANIFEST_INVALID",
            Self::IntegrityMismatch => "MIRAGE_INTEGRITY_MISMATCH",
            Self::CacheFull => "MIRAGE_CACHE_FULL",
            Self::BackendUnauthenticated => "MIRAGE_BACKEND_UNAUTHENTICATED",
            Self::BackendPermissionDenied => "MIRAGE_BACKEND_PERMISSION_DENIED",
            Self::BackendRateLimited => "MIRAGE_BACKEND_RATE_LIMITED",
            Self::BackendUnavailable => "MIRAGE_BACKEND_UNAVAILABLE",
            Self::RemoteObjectMissing => "MIRAGE_REMOTE_OBJECT_MISSING",
            Self::RepositoryConflict => "MIRAGE_REPOSITORY_CONFLICT",
            Self::UpdateActive => "MIRAGE_UPDATE_ACTIVE",
            Self::ProviderUnavailable => "MIRAGE_PROVIDER_UNAVAILABLE",
            Self::Io => "MIRAGE_IO_ERROR",
            Self::Cancelled => "MIRAGE_CANCELLED",
            Self::DeadlineExceeded => "MIRAGE_DEADLINE_EXCEEDED",
            Self::NotImplemented => "MIRAGE_NOT_IMPLEMENTED",
            Self::InternalInvariant => "MIRAGE_INTERNAL_INVARIANT",
        }
    }

    /// Returns the default retry disposition associated with this error kind.
    #[must_use]
    pub const fn default_retry(&self) -> RetryDisposition {
        match self {
            Self::InvalidArgument => RetryDisposition::Never,
            Self::UnsupportedLayout => RetryDisposition::Never,
            Self::ManifestInvalid => RetryDisposition::Never,
            Self::IntegrityMismatch => RetryDisposition::Never,
            Self::CacheFull => RetryDisposition::UserAction,
            Self::BackendUnauthenticated => RetryDisposition::UserAction,
            Self::BackendPermissionDenied => RetryDisposition::UserAction,
            Self::BackendRateLimited => RetryDisposition::Backoff,
            Self::BackendUnavailable => RetryDisposition::Backoff,
            Self::RemoteObjectMissing => RetryDisposition::Never,
            Self::RepositoryConflict => RetryDisposition::UserAction,
            Self::UpdateActive => RetryDisposition::Backoff,
            Self::ProviderUnavailable => RetryDisposition::UserAction,
            Self::Io => RetryDisposition::Never,
            Self::Cancelled => RetryDisposition::Never,
            Self::DeadlineExceeded => RetryDisposition::Never,
            Self::NotImplemented => RetryDisposition::Never,
            Self::InternalInvariant => RetryDisposition::Never,
        }
    }

    /// Returns the string identifier of the error kind.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::InvalidArgument => "InvalidArgument",
            Self::UnsupportedLayout => "UnsupportedLayout",
            Self::ManifestInvalid => "ManifestInvalid",
            Self::IntegrityMismatch => "IntegrityMismatch",
            Self::CacheFull => "CacheFull",
            Self::BackendUnauthenticated => "BackendUnauthenticated",
            Self::BackendPermissionDenied => "BackendPermissionDenied",
            Self::BackendRateLimited => "BackendRateLimited",
            Self::BackendUnavailable => "BackendUnavailable",
            Self::RemoteObjectMissing => "RemoteObjectMissing",
            Self::RepositoryConflict => "RepositoryConflict",
            Self::UpdateActive => "UpdateActive",
            Self::ProviderUnavailable => "ProviderUnavailable",
            Self::Io => "Io",
            Self::Cancelled => "Cancelled",
            Self::DeadlineExceeded => "DeadlineExceeded",
            Self::NotImplemented => "NotImplemented",
            Self::InternalInvariant => "InternalInvariant",
        }
    }
}

impl fmt::Display for MirageErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Core domain error for MirageSSD operations.
///
/// Contains a structured error kind, a stable machine-readable code, a safe user-facing
/// error message, explicit retry semantics, and an optional diagnostic source chain.
#[derive(Debug)]
pub struct MirageError {
    /// Classification category of the error.
    pub kind: MirageErrorKind,
    /// Stable machine code (e.g. `MIRAGE_BACKEND_429`, `MIRAGE_INTEGRITY_MISMATCH`).
    pub code: &'static str,
    /// Sanitized user-facing message.
    pub message: String,
    /// Retry disposition indicating whether/how caller may retry.
    pub retry: RetryDisposition,
    /// Optional underlying diagnostic error cause (excluded from public wire envelopes).
    pub source: Option<Box<dyn StdError + Send + Sync + 'static>>,
}

impl MirageError {
    /// Creates a new `MirageError` with the given kind, machine code, and message.
    /// The default retry disposition for the kind is applied.
    pub fn new(kind: MirageErrorKind, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            kind,
            code,
            message: message.into(),
            retry: kind.default_retry(),
            source: None,
        }
    }

    /// Overrides the retry disposition of this error.
    #[must_use]
    pub fn with_retry(mut self, retry: RetryDisposition) -> Self {
        self.retry = retry;
        self
    }

    /// Attaches an underlying diagnostic source error.
    #[must_use]
    pub fn with_source(
        mut self,
        source: impl Into<Box<dyn StdError + Send + Sync + 'static>>,
    ) -> Self {
        self.source = Some(source.into());
        self
    }

    /// Constructs an `InvalidArgument` error.
    pub fn invalid_argument(message: impl Into<String>) -> Self {
        Self::new(
            MirageErrorKind::InvalidArgument,
            MirageErrorKind::InvalidArgument.default_code(),
            message,
        )
    }

    /// Constructs an `UnsupportedLayout` error.
    pub fn unsupported_layout(message: impl Into<String>) -> Self {
        Self::new(
            MirageErrorKind::UnsupportedLayout,
            MirageErrorKind::UnsupportedLayout.default_code(),
            message,
        )
    }

    /// Constructs a `ManifestInvalid` error.
    pub fn manifest_invalid(message: impl Into<String>) -> Self {
        Self::new(
            MirageErrorKind::ManifestInvalid,
            MirageErrorKind::ManifestInvalid.default_code(),
            message,
        )
    }

    /// Constructs an `IntegrityMismatch` error.
    pub fn integrity_mismatch(message: impl Into<String>) -> Self {
        Self::new(
            MirageErrorKind::IntegrityMismatch,
            MirageErrorKind::IntegrityMismatch.default_code(),
            message,
        )
    }

    /// Constructs a `CacheFull` error.
    pub fn cache_full(message: impl Into<String>) -> Self {
        Self::new(
            MirageErrorKind::CacheFull,
            MirageErrorKind::CacheFull.default_code(),
            message,
        )
    }

    /// Constructs a `BackendUnauthenticated` error.
    pub fn backend_unauthenticated(message: impl Into<String>) -> Self {
        Self::new(
            MirageErrorKind::BackendUnauthenticated,
            MirageErrorKind::BackendUnauthenticated.default_code(),
            message,
        )
    }

    /// Constructs a `BackendPermissionDenied` error.
    pub fn backend_permission_denied(message: impl Into<String>) -> Self {
        Self::new(
            MirageErrorKind::BackendPermissionDenied,
            MirageErrorKind::BackendPermissionDenied.default_code(),
            message,
        )
    }

    /// Constructs a `BackendRateLimited` error with optional cooldown.
    pub fn backend_rate_limited(
        message: impl Into<String>,
        cooldown: Option<core::time::Duration>,
    ) -> Self {
        let retry = match cooldown {
            Some(dur) => RetryDisposition::After(dur),
            None => RetryDisposition::Backoff,
        };
        Self::new(
            MirageErrorKind::BackendRateLimited,
            MirageErrorKind::BackendRateLimited.default_code(),
            message,
        )
        .with_retry(retry)
    }

    /// Constructs a `BackendUnavailable` error.
    pub fn backend_unavailable(message: impl Into<String>) -> Self {
        Self::new(
            MirageErrorKind::BackendUnavailable,
            MirageErrorKind::BackendUnavailable.default_code(),
            message,
        )
    }

    /// Constructs a `RemoteObjectMissing` error.
    pub fn remote_object_missing(message: impl Into<String>) -> Self {
        Self::new(
            MirageErrorKind::RemoteObjectMissing,
            MirageErrorKind::RemoteObjectMissing.default_code(),
            message,
        )
    }

    /// Constructs a `RepositoryConflict` error.
    pub fn repository_conflict(message: impl Into<String>) -> Self {
        Self::new(
            MirageErrorKind::RepositoryConflict,
            MirageErrorKind::RepositoryConflict.default_code(),
            message,
        )
    }

    /// Constructs an `UpdateActive` error.
    pub fn update_active(message: impl Into<String>) -> Self {
        Self::new(
            MirageErrorKind::UpdateActive,
            MirageErrorKind::UpdateActive.default_code(),
            message,
        )
    }

    /// Constructs a `ProviderUnavailable` error.
    pub fn provider_unavailable(message: impl Into<String>) -> Self {
        Self::new(
            MirageErrorKind::ProviderUnavailable,
            MirageErrorKind::ProviderUnavailable.default_code(),
            message,
        )
    }

    /// Constructs an `InternalInvariant` error.
    pub fn internal_invariant(message: impl Into<String>) -> Self {
        Self::new(
            MirageErrorKind::InternalInvariant,
            MirageErrorKind::InternalInvariant.default_code(),
            message,
        )
    }

    /// Constructs a cancellation error. A fresh caller request may be issued independently.
    pub fn cancelled(message: impl Into<String>) -> Self {
        Self::new(
            MirageErrorKind::Cancelled,
            MirageErrorKind::Cancelled.default_code(),
            message,
        )
    }

    /// Constructs a deadline-exceeded error without retrying inside the expired request.
    pub fn deadline_exceeded(message: impl Into<String>) -> Self {
        Self::new(
            MirageErrorKind::DeadlineExceeded,
            MirageErrorKind::DeadlineExceeded.default_code(),
            message,
        )
    }

    /// Constructs a stable not-implemented error for command-surface placeholders.
    pub fn not_implemented(message: impl Into<String>) -> Self {
        Self::new(
            MirageErrorKind::NotImplemented,
            MirageErrorKind::NotImplemented.default_code(),
            message,
        )
    }

    /// Constructs a safe public error envelope, stripping internal diagnostic sources and traces.
    #[must_use]
    pub fn to_public_envelope(&self) -> PublicErrorEnvelope {
        PublicErrorEnvelope {
            code: self.code.to_string(),
            kind: self.kind.to_string(),
            message: self.message.clone(),
            retry: self.retry,
        }
    }
}

impl fmt::Display for MirageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "[{}] {}: {} (retry: {})",
            self.code, self.kind, self.message, self.retry
        )
    }
}

impl StdError for MirageError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        self.source
            .as_deref()
            .map(|s| s as &(dyn StdError + 'static))
    }
}

/// Converts a `std::io::Error` into a `MirageError`.
///
/// NOTE: Non-transient I/O errors (e.g. `NotFound`, `PermissionDenied`, `InvalidData`) are marked
/// `RetryDisposition::Never`. Only specifically transient errors (e.g. `TimedOut`, `Interrupted`,
/// `WouldBlock`) are marked retryable to avoid blanket retry conversions.
impl From<std::io::Error> for MirageError {
    fn from(err: std::io::Error) -> Self {
        let retry = match err.kind() {
            std::io::ErrorKind::TimedOut => RetryDisposition::Backoff,
            std::io::ErrorKind::Interrupted => RetryDisposition::Immediate,
            std::io::ErrorKind::WouldBlock => RetryDisposition::Immediate,
            std::io::ErrorKind::PermissionDenied => RetryDisposition::UserAction,
            _ => RetryDisposition::Never,
        };
        let msg = err.to_string();
        Self {
            kind: MirageErrorKind::Io,
            code: MirageErrorKind::Io.default_code(),
            message: msg,
            retry,
            source: Some(Box::new(err)),
        }
    }
}

/// Public, sanitized serialization envelope for wire and API responses.
///
/// Excludes diagnostic source chains, call stacks, and internal credential paths.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct PublicErrorEnvelope {
    /// Stable machine error code.
    pub code: String,
    /// Category kind name.
    pub kind: String,
    /// Safe user message.
    pub message: String,
    /// Retry disposition.
    pub retry: RetryDisposition,
}

impl From<&MirageError> for PublicErrorEnvelope {
    fn from(err: &MirageError) -> Self {
        err.to_public_envelope()
    }
}

impl From<MirageError> for PublicErrorEnvelope {
    fn from(err: MirageError) -> Self {
        err.to_public_envelope()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_construction_and_envelope() {
        let err = MirageError::integrity_mismatch("BLAKE3 page mismatch at ordinal 42");
        assert_eq!(err.kind, MirageErrorKind::IntegrityMismatch);
        assert_eq!(err.code, "MIRAGE_INTEGRITY_MISMATCH");
        assert_eq!(err.retry, RetryDisposition::Never);

        let envelope = err.to_public_envelope();
        assert_eq!(envelope.code, "MIRAGE_INTEGRITY_MISMATCH");
        assert_eq!(envelope.kind, "IntegrityMismatch");
        assert_eq!(envelope.message, "BLAKE3 page mismatch at ordinal 42");
        assert_eq!(envelope.retry, RetryDisposition::Never);
    }

    #[test]
    fn test_io_error_conversion_retry_semantics() {
        let not_found = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        let mirage_err = MirageError::from(not_found);
        assert_eq!(mirage_err.retry, RetryDisposition::Never);

        let timed_out = std::io::Error::new(std::io::ErrorKind::TimedOut, "socket timed out");
        let mirage_err_to = MirageError::from(timed_out);
        assert_eq!(mirage_err_to.retry, RetryDisposition::Backoff);
    }
}
