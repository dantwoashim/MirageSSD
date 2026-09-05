use bytes::Bytes;
use mirage_types::{CheckedRange, MirageError};

pub fn exact_body(
    requested: CheckedRange,
    returned: CheckedRange,
    body: Bytes,
    maximum: u64,
) -> Result<Bytes, MirageError> {
    if requested != returned || body.len() as u64 != requested.len() || requested.len() > maximum {
        return Err(MirageError::integrity_mismatch(
            "backend response range or length is not exact",
        ));
    }
    Ok(body)
}
