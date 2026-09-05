use mirage_backend::{BackendError, BackendErrorClass};

pub fn validate_content_range(value: &str, start: u64, end: u64) -> Result<(), BackendError> {
    let value = value
        .strip_prefix("bytes ")
        .ok_or_else(|| BackendError::integrity("Drive returned malformed Content-Range"))?;
    let (span, total) = value
        .split_once('/')
        .ok_or_else(|| BackendError::integrity("Drive returned malformed Content-Range"))?;
    let (actual_start, actual_end) = span
        .split_once('-')
        .ok_or_else(|| BackendError::integrity("Drive returned malformed Content-Range"))?;
    let actual_start = actual_start
        .parse::<u64>()
        .map_err(|_| BackendError::integrity("Drive returned malformed Content-Range"))?;
    let actual_end = actual_end
        .parse::<u64>()
        .map_err(|_| BackendError::integrity("Drive returned malformed Content-Range"))?;
    if total != "*" && total.parse::<u64>().is_err() {
        return Err(BackendError::integrity(
            "Drive returned malformed Content-Range",
        ));
    }
    if actual_start != start || actual_end != end {
        return Err(BackendError::new(
            BackendErrorClass::Integrity,
            "Drive returned the wrong byte range",
        ));
    }
    Ok(())
}
