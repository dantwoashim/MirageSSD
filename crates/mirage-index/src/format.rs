use mirage_types::MirageError;

pub const MAGIC: &[u8; 8] = b"MIRIDX02";
pub const FORMAT_VERSION: u32 = 2;
pub const HEADER_SIZE: usize = 320;
pub const HEADER_ALIGNMENT: u64 = 64;
pub const SECTION_DIRECTORY_OFFSET: usize = 128;
pub const SECTION_ENTRY_SIZE: usize = 32;
pub const SECTION_COUNT: usize = 6;
pub const INDEX_HASH_OFFSET: usize = 88;
pub const INDEX_HASH_LENGTH: usize = 32;

pub const DIRECTORY_RECORD_SIZE: u32 = 56;
pub const FILE_RECORD_SIZE: u32 = 64;
pub const EXTENT_RECORD_SIZE: u32 = 32;
pub const PAGE_RECORD_SIZE: u32 = 48;
pub const REMOTE_LOCATION_RECORD_SIZE: u32 = 104;

const FORMAT_DESCRIPTOR: &[u8] = b"MirageSSD/MIRIDX02/le/header320/sections:string1,dir56,file64,extent32,page48,remote104/hash-zeroed-v2";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u32)]
pub enum SectionKind {
    Strings = 1,
    Directories = 2,
    Files = 3,
    Extents = 4,
    Pages = 5,
    RemoteLocations = 6,
}

impl SectionKind {
    pub const ALL: [Self; SECTION_COUNT] = [
        Self::Strings,
        Self::Directories,
        Self::Files,
        Self::Extents,
        Self::Pages,
        Self::RemoteLocations,
    ];

    pub fn from_u32(value: u32) -> Result<Self, MirageError> {
        match value {
            1 => Ok(Self::Strings),
            2 => Ok(Self::Directories),
            3 => Ok(Self::Files),
            4 => Ok(Self::Extents),
            5 => Ok(Self::Pages),
            6 => Ok(Self::RemoteLocations),
            _ => Err(MirageError::manifest_invalid(
                "index contains an unknown section kind",
            )),
        }
    }

    #[must_use]
    pub const fn index(self) -> usize {
        self as usize - 1
    }

    #[must_use]
    pub const fn record_size(self) -> u32 {
        match self {
            Self::Strings => 1,
            Self::Directories => DIRECTORY_RECORD_SIZE,
            Self::Files => FILE_RECORD_SIZE,
            Self::Extents => EXTENT_RECORD_SIZE,
            Self::Pages => PAGE_RECORD_SIZE,
            Self::RemoteLocations => REMOTE_LOCATION_RECORD_SIZE,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Section {
    pub kind: SectionKind,
    pub offset: u64,
    pub length: u64,
    pub count: u64,
}

impl Section {
    pub fn end(self) -> Result<u64, MirageError> {
        self.offset
            .checked_add(self.length)
            .ok_or_else(|| MirageError::manifest_invalid("index section end overflows"))
    }
}

#[must_use]
pub fn format_hash() -> [u8; 32] {
    *blake3::hash(FORMAT_DESCRIPTOR).as_bytes()
}

pub fn align_up(value: u64, alignment: u64) -> Result<u64, MirageError> {
    if alignment == 0 || !alignment.is_power_of_two() {
        return Err(MirageError::internal_invariant(
            "index alignment is invalid",
        ));
    }
    value
        .checked_add(alignment - 1)
        .map(|adjusted| adjusted & !(alignment - 1))
        .ok_or_else(|| MirageError::manifest_invalid("aligned index offset overflows"))
}
