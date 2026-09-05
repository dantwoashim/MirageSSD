use mirage_types::{MirageError, MirageErrorKind};

pub(crate) fn sqlite(error: rusqlite::Error, context: &'static str) -> MirageError {
    MirageError::new(
        MirageErrorKind::Io,
        MirageErrorKind::Io.default_code(),
        context,
    )
    .with_source(error)
}

pub(crate) fn writer_unavailable() -> MirageError {
    MirageError::new(
        MirageErrorKind::Io,
        MirageErrorKind::Io.default_code(),
        "database writer is unavailable",
    )
}

pub(crate) fn conflict(message: &'static str) -> MirageError {
    MirageError::repository_conflict(message)
}

pub(crate) fn transition(error: impl std::error::Error + Send + Sync + 'static) -> MirageError {
    MirageError::repository_conflict("durable state transition was rejected").with_source(error)
}
