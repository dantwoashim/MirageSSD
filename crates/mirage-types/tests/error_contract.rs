//! Contract and synchronization tests for MirageSSD error taxonomy and public envelopes.

use std::fs;
use std::path::Path;
use std::time::Duration;

use mirage_types::error::{MirageError, MirageErrorKind, PublicErrorEnvelope};
use mirage_types::retry::RetryDisposition;

const ALL_KINDS: &[MirageErrorKind] = &[
    MirageErrorKind::InvalidArgument,
    MirageErrorKind::UnsupportedLayout,
    MirageErrorKind::ManifestInvalid,
    MirageErrorKind::IntegrityMismatch,
    MirageErrorKind::CacheFull,
    MirageErrorKind::BackendUnauthenticated,
    MirageErrorKind::BackendPermissionDenied,
    MirageErrorKind::BackendRateLimited,
    MirageErrorKind::BackendUnavailable,
    MirageErrorKind::RemoteObjectMissing,
    MirageErrorKind::RepositoryConflict,
    MirageErrorKind::UpdateActive,
    MirageErrorKind::ProviderUnavailable,
    MirageErrorKind::Io,
    MirageErrorKind::Cancelled,
    MirageErrorKind::DeadlineExceeded,
    MirageErrorKind::NotImplemented,
    MirageErrorKind::InternalInvariant,
];

#[test]
fn test_all_18_error_kinds_present() {
    assert_eq!(ALL_KINDS.len(), 18);
}

#[test]
fn test_error_catalog_markdown_synchronization() {
    // Find docs/specs/error-catalog.md relative to repository workspace or manifest dir
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
    let path_in_crate = Path::new(&manifest_dir).join("../../docs/specs/error-catalog.md");
    let path_direct = Path::new("docs/specs/error-catalog.md");

    let catalog_path = if path_in_crate.exists() {
        path_in_crate
    } else if path_direct.exists() {
        path_direct.to_path_buf()
    } else {
        panic!("could not locate docs/specs/error-catalog.md");
    };

    let content = fs::read_to_string(catalog_path).expect("read error-catalog.md");

    for &kind in ALL_KINDS {
        let kind_str = kind.as_str();
        let code_str = kind.default_code();

        assert!(
            content.contains(kind_str),
            "error-catalog.md is missing error kind: {}",
            kind_str
        );
        assert!(
            content.contains(code_str),
            "error-catalog.md is missing machine error code: {}",
            code_str
        );
    }
}

#[test]
fn test_public_envelope_serialization_excludes_source_and_secrets() {
    let internal_io = std::io::Error::new(
        std::io::ErrorKind::PermissionDenied,
        "failed to open C:/Users/secret/path/token.dat: access denied",
    );
    let error = MirageError::new(
        MirageErrorKind::BackendUnauthenticated,
        "MIRAGE_BACKEND_UNAUTHENTICATED",
        "Authentication token expired or invalid",
    )
    .with_source(internal_io);

    let envelope = error.to_public_envelope();
    let json = serde_json::to_string_pretty(&envelope).expect("serialize envelope");

    // Must contain standard fields
    assert!(json.contains("MIRAGE_BACKEND_UNAUTHENTICATED"));
    assert!(json.contains("BackendUnauthenticated"));
    assert!(json.contains("Authentication token expired or invalid"));
    assert!(json.contains("user_action"));

    // Must strictly NOT leak the source chain or secret path
    assert!(!json.contains("secret"));
    assert!(!json.contains("token.dat"));
    assert!(!json.contains("source"));

    // Deserialization round-trip
    let deserialized: PublicErrorEnvelope =
        serde_json::from_str(&json).expect("deserialize envelope");
    assert_eq!(deserialized, envelope);
}

#[test]
fn test_retry_disposition_serde_variants() {
    let variants = [
        (RetryDisposition::Never, "\"never\""),
        (RetryDisposition::Immediate, "\"immediate\""),
        (RetryDisposition::Backoff, "\"backoff\""),
        (RetryDisposition::UserAction, "\"user_action\""),
    ];

    for (disp, expected_json) in variants {
        let serialized = serde_json::to_string(&disp).unwrap();
        assert_eq!(serialized, expected_json);
        let parsed: RetryDisposition = serde_json::from_str(&serialized).unwrap();
        assert_eq!(parsed, disp);
    }

    // After with duration
    let after = RetryDisposition::After(Duration::from_millis(1500));
    let serialized_after = serde_json::to_string(&after).unwrap();
    assert_eq!(
        serialized_after,
        r#"{"after":{"seconds":1,"nanoseconds":500000000}}"#
    );
    let parsed_after: RetryDisposition = serde_json::from_str(&serialized_after).unwrap();
    assert_eq!(parsed_after, after);
}

#[test]
fn test_io_conversion_no_blanket_retry() {
    // Non-retryable
    let not_found = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
    assert_eq!(MirageError::from(not_found).retry, RetryDisposition::Never);

    let invalid_input = std::io::Error::new(std::io::ErrorKind::InvalidInput, "bad param");
    assert_eq!(
        MirageError::from(invalid_input).retry,
        RetryDisposition::Never
    );

    // Transient
    let timed_out = std::io::Error::new(std::io::ErrorKind::TimedOut, "socket timeout");
    assert_eq!(
        MirageError::from(timed_out).retry,
        RetryDisposition::Backoff
    );

    let interrupted = std::io::Error::new(std::io::ErrorKind::Interrupted, "signal interrupted");
    assert_eq!(
        MirageError::from(interrupted).retry,
        RetryDisposition::Immediate
    );
}
