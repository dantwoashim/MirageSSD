use mirage_manifest::Codec;
use mirage_types::{MirageError, PageHash};

use crate::encode::{read_u32, read_u64, take};
use crate::format::INDEX_ENTRY_LEN;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PackEntry {
    pub page_hash: PageHash,
    pub frame_offset: u64,
    pub frame_length: u64,
    pub logical_length: u32,
    pub encoded_length: u32,
    pub codec: Codec,
    pub encrypted: bool,
}

impl PackEntry {
    #[must_use]
    pub fn encode(self) -> [u8; INDEX_ENTRY_LEN] {
        let mut bytes = [0_u8; INDEX_ENTRY_LEN];
        bytes[..32].copy_from_slice(self.page_hash.as_bytes());
        bytes[32..40].copy_from_slice(&self.frame_offset.to_le_bytes());
        bytes[40..48].copy_from_slice(&self.frame_length.to_le_bytes());
        bytes[48..52].copy_from_slice(&self.logical_length.to_le_bytes());
        bytes[52..56].copy_from_slice(&self.encoded_length.to_le_bytes());
        bytes[56] = self.codec.code();
        bytes[57] = u8::from(self.encrypted);
        bytes
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, MirageError> {
        if bytes.len() != INDEX_ENTRY_LEN || bytes[57] > 1 || take::<6>(bytes, 58)? != [0; 6] {
            return Err(MirageError::manifest_invalid(
                "pack index entry is malformed",
            ));
        }
        let codec = match bytes[56] {
            0 => Codec::None,
            _ => {
                return Err(MirageError::unsupported_layout(
                    "pack index codec is unsupported",
                ));
            }
        };
        let entry = Self {
            page_hash: PageHash::from_bytes(take(bytes, 0)?),
            frame_offset: read_u64(bytes, 32)?,
            frame_length: read_u64(bytes, 40)?,
            logical_length: read_u32(bytes, 48)?,
            encoded_length: read_u32(bytes, 52)?,
            codec,
            encrypted: bytes[57] != 0,
        };
        let overhead = if entry.encrypted { 68_u64 } else { 64_u64 };
        if entry.frame_length != overhead.saturating_add(u64::from(entry.encoded_length))
            || entry.logical_length == 0
            || entry.encoded_length == 0
        {
            return Err(MirageError::manifest_invalid(
                "pack index lengths are inconsistent",
            ));
        }
        Ok(entry)
    }
}

pub(crate) fn encode_index(entries: &[PackEntry]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(entries.len().saturating_mul(INDEX_ENTRY_LEN));
    for entry in entries {
        bytes.extend_from_slice(&entry.encode());
    }
    bytes
}

pub(crate) fn decode_index(bytes: &[u8], count: u64) -> Result<Vec<PackEntry>, MirageError> {
    let expected = usize::try_from(count)
        .ok()
        .and_then(|value| value.checked_mul(INDEX_ENTRY_LEN))
        .ok_or_else(|| MirageError::manifest_invalid("pack index count overflows"))?;
    if bytes.len() != expected {
        return Err(MirageError::manifest_invalid(
            "pack index length is inconsistent",
        ));
    }
    let mut entries = Vec::with_capacity(expected / INDEX_ENTRY_LEN);
    for chunk in bytes.chunks_exact(INDEX_ENTRY_LEN) {
        entries.push(PackEntry::decode(chunk)?);
    }
    if !entries
        .windows(2)
        .all(|pair| pair[0].page_hash < pair[1].page_hash)
    {
        return Err(MirageError::manifest_invalid(
            "pack index is not strictly hash-sorted",
        ));
    }
    Ok(entries)
}
