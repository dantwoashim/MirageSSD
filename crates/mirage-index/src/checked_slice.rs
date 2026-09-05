use mirage_types::MirageError;

pub fn bytes_at(bytes: &[u8], offset: u64, length: u64) -> Result<&[u8], MirageError> {
    let start = usize::try_from(offset)
        .map_err(|_| MirageError::manifest_invalid("index offset does not fit address space"))?;
    let length = usize::try_from(length)
        .map_err(|_| MirageError::manifest_invalid("index length does not fit address space"))?;
    let end = start
        .checked_add(length)
        .ok_or_else(|| MirageError::manifest_invalid("index slice overflows address space"))?;
    bytes
        .get(start..end)
        .ok_or_else(|| MirageError::manifest_invalid("index slice lies outside file"))
}

pub fn array_at<const N: usize>(bytes: &[u8], offset: usize) -> Result<[u8; N], MirageError> {
    let end = offset
        .checked_add(N)
        .ok_or_else(|| MirageError::manifest_invalid("index field offset overflows"))?;
    bytes
        .get(offset..end)
        .ok_or_else(|| MirageError::manifest_invalid("index field is truncated"))?
        .try_into()
        .map_err(|_| MirageError::manifest_invalid("index field has invalid width"))
}

pub fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, MirageError> {
    Ok(u32::from_le_bytes(array_at(bytes, offset)?))
}

pub fn read_u64(bytes: &[u8], offset: usize) -> Result<u64, MirageError> {
    Ok(u64::from_le_bytes(array_at(bytes, offset)?))
}

pub fn write_u32(bytes: &mut [u8], offset: usize, value: u32) -> Result<(), MirageError> {
    write_bytes(bytes, offset, &value.to_le_bytes())
}

pub fn write_u64(bytes: &mut [u8], offset: usize, value: u64) -> Result<(), MirageError> {
    write_bytes(bytes, offset, &value.to_le_bytes())
}

pub fn write_bytes(bytes: &mut [u8], offset: usize, value: &[u8]) -> Result<(), MirageError> {
    let end = offset
        .checked_add(value.len())
        .ok_or_else(|| MirageError::internal_invariant("index write offset overflows"))?;
    bytes
        .get_mut(offset..end)
        .ok_or_else(|| MirageError::internal_invariant("index write exceeds buffer"))?
        .copy_from_slice(value);
    Ok(())
}
