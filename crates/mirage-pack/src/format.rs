use mirage_types::MirageError;

use crate::encode::{crc32, read_u32, read_u64};

pub const PACK_VERSION: u32 = 1;
pub const PACK_HEADER_LEN: usize = 64;
pub const FRAME_HEADER_LEN: usize = 64;
pub const INDEX_ENTRY_LEN: usize = 64;
pub const FOOTER_LEN: usize = 128;
pub const MAX_ENCODED_FRAME_LEN: u32 = 16 * 1024 * 1024;

const PACK_MAGIC: &[u8; 8] = b"MRGPACK1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PackHeader {
    pub page_size: u32,
    pub index_offset: u64,
    pub index_length: u64,
    pub entry_count: u64,
    pub encrypted: bool,
    pub pack_id: [u8; 16],
}

impl PackHeader {
    #[must_use]
    pub fn encode(self) -> [u8; PACK_HEADER_LEN] {
        let mut bytes = [0_u8; PACK_HEADER_LEN];
        bytes[0..8].copy_from_slice(PACK_MAGIC);
        bytes[8..12].copy_from_slice(&PACK_VERSION.to_le_bytes());
        bytes[12..16].copy_from_slice(&u32::from(self.encrypted).to_le_bytes());
        bytes[16..20].copy_from_slice(&self.page_size.to_le_bytes());
        bytes[20..24].copy_from_slice(&self.pack_id[..4]);
        bytes[24..32].copy_from_slice(&self.index_offset.to_le_bytes());
        bytes[32..40].copy_from_slice(&self.index_length.to_le_bytes());
        bytes[40..48].copy_from_slice(&self.entry_count.to_le_bytes());
        let checksum = crc32(&bytes[..48]);
        bytes[48..52].copy_from_slice(&checksum.to_le_bytes());
        bytes[52..64].copy_from_slice(&self.pack_id[4..]);
        bytes
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, MirageError> {
        if bytes.len() != PACK_HEADER_LEN || &bytes[..8] != PACK_MAGIC {
            return Err(MirageError::manifest_invalid(
                "pack header magic or length is invalid",
            ));
        }
        if read_u32(bytes, 8)? != PACK_VERSION {
            return Err(MirageError::unsupported_layout(
                "pack version is unsupported",
            ));
        }
        if read_u32(bytes, 48)? != crc32(&bytes[..48]) {
            return Err(MirageError::integrity_mismatch(
                "pack header checksum or reserved bytes are invalid",
            ));
        }
        let page_size = read_u32(bytes, 16)?;
        let flags = read_u32(bytes, 12)?;
        if flags & !1 != 0 {
            return Err(MirageError::unsupported_layout(
                "pack encryption flags are unsupported",
            ));
        }
        let mut pack_id = [0_u8; 16];
        pack_id[..4].copy_from_slice(&bytes[20..24]);
        pack_id[4..].copy_from_slice(&bytes[52..64]);
        if (flags & 1 != 0 && pack_id == [0; 16]) || (flags & 1 == 0 && pack_id != [0; 16]) {
            return Err(MirageError::integrity_mismatch(
                "pack identity and encryption policy disagree",
            ));
        }
        if !(64 * 1024..=16 * 1024 * 1024).contains(&page_size) || !page_size.is_power_of_two() {
            return Err(MirageError::manifest_invalid("pack page size is invalid"));
        }
        Ok(Self {
            page_size,
            index_offset: read_u64(bytes, 24)?,
            index_length: read_u64(bytes, 32)?,
            entry_count: read_u64(bytes, 40)?,
            encrypted: flags & 1 != 0,
            pack_id,
        })
    }
}
