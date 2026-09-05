use mirage_types::{ContentHash, MirageError};

use crate::encode::{crc32, read_u32, read_u64, take};
use crate::format::{FOOTER_LEN, PACK_VERSION};

const FOOTER_MAGIC: &[u8; 8] = b"MRGPFTR1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PackFooter {
    pub index_hash: ContentHash,
    pub content_hash: ContentHash,
    pub pack_length: u64,
}

impl PackFooter {
    #[must_use]
    pub fn encode(self) -> [u8; FOOTER_LEN] {
        let mut bytes = [0_u8; FOOTER_LEN];
        bytes[..8].copy_from_slice(FOOTER_MAGIC);
        bytes[8..12].copy_from_slice(&PACK_VERSION.to_le_bytes());
        bytes[16..48].copy_from_slice(self.index_hash.as_bytes());
        bytes[48..80].copy_from_slice(self.content_hash.as_bytes());
        bytes[80..88].copy_from_slice(&self.pack_length.to_le_bytes());
        let checksum = crc32(&bytes[..88]);
        bytes[88..92].copy_from_slice(&checksum.to_le_bytes());
        bytes
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, MirageError> {
        if bytes.len() != FOOTER_LEN || &bytes[..8] != FOOTER_MAGIC {
            return Err(MirageError::manifest_invalid(
                "pack footer magic or length is invalid",
            ));
        }
        if read_u32(bytes, 8)? != PACK_VERSION {
            return Err(MirageError::unsupported_layout(
                "pack footer version is unsupported",
            ));
        }
        if take::<4>(bytes, 12)? != [0; 4]
            || take::<36>(bytes, 92)? != [0; 36]
            || read_u32(bytes, 88)? != crc32(&bytes[..88])
        {
            return Err(MirageError::integrity_mismatch(
                "pack footer checksum or reserved bytes are invalid",
            ));
        }
        Ok(Self {
            index_hash: ContentHash::from_bytes(take(bytes, 16)?),
            content_hash: ContentHash::from_bytes(take(bytes, 48)?),
            pack_length: read_u64(bytes, 80)?,
        })
    }
}
