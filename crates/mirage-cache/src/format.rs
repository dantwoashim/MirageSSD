use mirage_types::{ByteCount, MirageError, PageHash};

pub const ARENA_MAGIC: [u8; 8] = *b"MIRARN1\0";
pub const ARENA_VERSION: u32 = 1;
pub const ARENA_HEADER_BYTES: usize = 4096;
pub const SLOT_METADATA_BYTES: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheLayout {
    pub page_size: ByteCount,
    pub slot_count: u32,
    pub db_journal_allowance: ByteCount,
    pub filesystem_reserve: ByteCount,
}

impl CacheLayout {
    pub fn validate(self) -> Result<(), MirageError> {
        let page = self.page_size.as_u64();
        if !(64 * 1024..=16 * 1024 * 1024).contains(&page)
            || !page.is_power_of_two()
            || self.slot_count == 0
        {
            return Err(MirageError::invalid_argument(
                "cache page size or slot count is outside bounds",
            ));
        }
        self.logical_shard_bytes()?;
        self.declared_physical_budget()?;
        Ok(())
    }
    pub fn logical_shard_bytes(self) -> Result<u64, MirageError> {
        u64::from(self.slot_count)
            .checked_mul(self.page_size.as_u64())
            .and_then(|bytes| bytes.checked_add(ARENA_HEADER_BYTES as u64))
            .ok_or_else(|| MirageError::invalid_argument("cache shard logical length overflows"))
    }
    pub fn metadata_bytes(self) -> Result<u64, MirageError> {
        u64::from(self.slot_count)
            .checked_mul(SLOT_METADATA_BYTES as u64)
            .and_then(|bytes| bytes.checked_add(ARENA_HEADER_BYTES as u64))
            .ok_or_else(|| MirageError::invalid_argument("cache metadata length overflows"))
    }
    pub fn declared_physical_budget(self) -> Result<u64, MirageError> {
        self.logical_shard_bytes()?
            .checked_add(self.metadata_bytes()?)
            .and_then(|bytes| bytes.checked_add(self.db_journal_allowance.as_u64()))
            .and_then(|bytes| bytes.checked_add(self.filesystem_reserve.as_u64()))
            .ok_or_else(|| MirageError::invalid_argument("cache physical budget overflows"))
    }
    pub fn slot_offset(self, slot: u32) -> Result<u64, MirageError> {
        if slot >= self.slot_count {
            return Err(MirageError::invalid_argument(
                "cache slot is outside the shard",
            ));
        }
        u64::from(slot)
            .checked_mul(self.page_size.as_u64())
            .and_then(|offset| offset.checked_add(ARENA_HEADER_BYTES as u64))
            .ok_or_else(|| MirageError::invalid_argument("cache slot offset overflows"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArenaHeader {
    pub page_size: ByteCount,
    pub slot_count: u32,
}

impl ArenaHeader {
    pub fn encode(self) -> Result<[u8; ARENA_HEADER_BYTES], MirageError> {
        CacheLayout {
            page_size: self.page_size,
            slot_count: self.slot_count,
            db_journal_allowance: ByteCount::ZERO,
            filesystem_reserve: ByteCount::ZERO,
        }
        .validate()?;
        let mut output = [0_u8; ARENA_HEADER_BYTES];
        output[..8].copy_from_slice(&ARENA_MAGIC);
        output[8..12].copy_from_slice(&ARENA_VERSION.to_le_bytes());
        output[12..20].copy_from_slice(&self.page_size.as_u64().to_le_bytes());
        output[20..24].copy_from_slice(&self.slot_count.to_le_bytes());
        let checksum = crc32fast::hash(&output[..24]);
        output[24..28].copy_from_slice(&checksum.to_le_bytes());
        Ok(output)
    }
    pub fn decode(input: &[u8]) -> Result<Self, MirageError> {
        if input.len() != ARENA_HEADER_BYTES
            || input[..8] != ARENA_MAGIC
            || u32::from_le_bytes(input[8..12].try_into().expect("slice")) != ARENA_VERSION
            || crc32fast::hash(&input[..24])
                != u32::from_le_bytes(input[24..28].try_into().expect("slice"))
            || input[28..].iter().any(|byte| *byte != 0)
        {
            return Err(MirageError::integrity_mismatch(
                "cache arena header is invalid",
            ));
        }
        let header = Self {
            page_size: ByteCount::from_u64(u64::from_le_bytes(
                input[12..20].try_into().expect("slice"),
            )),
            slot_count: u32::from_le_bytes(input[20..24].try_into().expect("slice")),
        };
        CacheLayout {
            page_size: header.page_size,
            slot_count: header.slot_count,
            db_journal_allowance: ByteCount::ZERO,
            filesystem_reserve: ByteCount::ZERO,
        }
        .validate()?;
        Ok(header)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SlotState {
    Free = 0,
    Reserved = 1,
    Resident = 2,
    Evicting = 3,
    RetryDeallocate = 4,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlotMetadata {
    pub generation: u64,
    pub state: SlotState,
    pub page_hash: PageHash,
    pub logical_length: u32,
}

impl SlotMetadata {
    pub fn encode(self) -> [u8; SLOT_METADATA_BYTES] {
        let mut output = [0_u8; SLOT_METADATA_BYTES];
        output[..8].copy_from_slice(&self.generation.to_le_bytes());
        output[8] = self.state as u8;
        output[12..44].copy_from_slice(self.page_hash.as_bytes());
        output[44..48].copy_from_slice(&self.logical_length.to_le_bytes());
        let checksum = crc32fast::hash(&output[..48]);
        output[48..52].copy_from_slice(&checksum.to_le_bytes());
        output
    }
    pub fn decode(input: &[u8]) -> Result<Self, MirageError> {
        if input.len() != SLOT_METADATA_BYTES
            || input[9..12].iter().any(|byte| *byte != 0)
            || input[52..].iter().any(|byte| *byte != 0)
            || crc32fast::hash(&input[..48])
                != u32::from_le_bytes(input[48..52].try_into().expect("slice"))
        {
            return Err(MirageError::integrity_mismatch(
                "cache slot metadata checksum is invalid",
            ));
        }
        let state = match input[8] {
            0 => SlotState::Free,
            1 => SlotState::Reserved,
            2 => SlotState::Resident,
            3 => SlotState::Evicting,
            4 => SlotState::RetryDeallocate,
            _ => return Err(MirageError::unsupported_layout("unknown cache slot state")),
        };
        let logical_length = u32::from_le_bytes(input[44..48].try_into().expect("slice"));
        let mut hash = [0_u8; 32];
        hash.copy_from_slice(&input[12..44]);
        Ok(Self {
            generation: u64::from_le_bytes(input[..8].try_into().expect("slice")),
            state,
            page_hash: PageHash::from_bytes(hash),
            logical_length,
        })
    }
}
