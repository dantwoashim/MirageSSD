use mirage_types::MirageError;

pub(crate) fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, MirageError> {
    Ok(u16::from_le_bytes(take(bytes, offset)?))
}

pub(crate) fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, MirageError> {
    Ok(u32::from_le_bytes(take(bytes, offset)?))
}

pub(crate) fn read_u64(bytes: &[u8], offset: usize) -> Result<u64, MirageError> {
    Ok(u64::from_le_bytes(take(bytes, offset)?))
}

pub(crate) fn take<const N: usize>(bytes: &[u8], offset: usize) -> Result<[u8; N], MirageError> {
    let end = offset
        .checked_add(N)
        .ok_or_else(|| MirageError::manifest_invalid("pack field offset overflowed"))?;
    bytes
        .get(offset..end)
        .ok_or_else(|| MirageError::manifest_invalid("pack field is truncated"))?
        .try_into()
        .map_err(|_| MirageError::manifest_invalid("pack field has the wrong length"))
}

pub(crate) fn crc32(bytes: &[u8]) -> u32 {
    crc32fast::hash(bytes)
}
