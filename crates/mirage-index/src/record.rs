use mirage_backend::ObjectKind;
use mirage_manifest::{Codec, FileClass};
use mirage_types::{MirageError, PageHash, StableFileId};

use crate::checked_slice::{array_at, read_u32, read_u64, write_bytes, write_u32, write_u64};
use crate::format::{
    DIRECTORY_RECORD_SIZE, EXTENT_RECORD_SIZE, FILE_RECORD_SIZE, PAGE_RECORD_SIZE,
    REMOTE_LOCATION_RECORD_SIZE,
};

pub const NO_PARENT: u32 = u32::MAX;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StringRef {
    pub offset: u64,
    pub length: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirectoryRecord {
    pub parent: u32,
    pub name: StringRef,
    pub key: StringRef,
    pub first_directory: u32,
    pub directory_count: u32,
    pub first_file: u32,
    pub file_count: u32,
    pub canonical_index: u32,
}

impl DirectoryRecord {
    pub fn encode(self, output: &mut [u8]) -> Result<(), MirageError> {
        exact_width(output, DIRECTORY_RECORD_SIZE)?;
        write_u32(output, 0, self.parent)?;
        write_u64(output, 8, self.name.offset)?;
        write_u32(output, 16, self.name.length)?;
        write_u32(output, 20, self.key.length)?;
        write_u64(output, 24, self.key.offset)?;
        write_u32(output, 32, self.first_directory)?;
        write_u32(output, 36, self.directory_count)?;
        write_u32(output, 40, self.first_file)?;
        write_u32(output, 44, self.file_count)?;
        write_u32(output, 48, self.canonical_index)
    }

    pub fn decode(input: &[u8]) -> Result<Self, MirageError> {
        exact_width(input, DIRECTORY_RECORD_SIZE)?;
        require_zero(&input[4..8])?;
        require_zero(&input[52..56])?;
        Ok(Self {
            parent: read_u32(input, 0)?,
            name: StringRef {
                offset: read_u64(input, 8)?,
                length: read_u32(input, 16)?,
            },
            key: StringRef {
                offset: read_u64(input, 24)?,
                length: read_u32(input, 20)?,
            },
            first_directory: read_u32(input, 32)?,
            directory_count: read_u32(input, 36)?,
            first_file: read_u32(input, 40)?,
            file_count: read_u32(input, 44)?,
            canonical_index: read_u32(input, 48)?,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileRecord {
    pub parent_directory: u32,
    pub class: FileClass,
    pub name: StringRef,
    pub key: StringRef,
    pub logical_size: u64,
    pub stable_id: StableFileId,
    pub extent_start: u32,
    pub extent_count: u32,
    pub canonical_index: u32,
}

impl FileRecord {
    pub fn encode(self, output: &mut [u8]) -> Result<(), MirageError> {
        exact_width(output, FILE_RECORD_SIZE)?;
        write_u32(output, 0, self.parent_directory)?;
        output[4] = self.class.code();
        write_u64(output, 8, self.name.offset)?;
        write_u32(output, 16, self.name.length)?;
        write_u32(output, 20, self.key.length)?;
        write_u64(output, 24, self.key.offset)?;
        write_u64(output, 32, self.logical_size)?;
        write_u64(output, 40, self.stable_id.as_u64())?;
        write_u32(output, 48, self.extent_start)?;
        write_u32(output, 52, self.extent_count)?;
        write_u32(output, 56, self.canonical_index)
    }

    pub fn decode(input: &[u8]) -> Result<Self, MirageError> {
        exact_width(input, FILE_RECORD_SIZE)?;
        require_zero(&input[5..8])?;
        require_zero(&input[60..64])?;
        Ok(Self {
            parent_directory: read_u32(input, 0)?,
            class: decode_file_class(input[4])?,
            name: StringRef {
                offset: read_u64(input, 8)?,
                length: read_u32(input, 16)?,
            },
            key: StringRef {
                offset: read_u64(input, 24)?,
                length: read_u32(input, 20)?,
            },
            logical_size: read_u64(input, 32)?,
            stable_id: StableFileId::from_u64(read_u64(input, 40)?),
            extent_start: read_u32(input, 48)?,
            extent_count: read_u32(input, 52)?,
            canonical_index: read_u32(input, 56)?,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExtentRecord {
    pub logical_offset: u64,
    pub logical_length: u64,
    pub page_start: u32,
    pub page_count: u32,
    pub tail_length: u32,
}

impl ExtentRecord {
    pub fn encode(self, output: &mut [u8]) -> Result<(), MirageError> {
        exact_width(output, EXTENT_RECORD_SIZE)?;
        write_u64(output, 0, self.logical_offset)?;
        write_u64(output, 8, self.logical_length)?;
        write_u32(output, 16, self.page_start)?;
        write_u32(output, 20, self.page_count)?;
        write_u32(output, 24, self.tail_length)
    }

    pub fn decode(input: &[u8]) -> Result<Self, MirageError> {
        exact_width(input, EXTENT_RECORD_SIZE)?;
        require_zero(&input[28..32])?;
        Ok(Self {
            logical_offset: read_u64(input, 0)?,
            logical_length: read_u64(input, 8)?,
            page_start: read_u32(input, 16)?,
            page_count: read_u32(input, 20)?,
            tail_length: read_u32(input, 24)?,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageRecord {
    pub plaintext_hash: PageHash,
    pub logical_length: u32,
    pub remote_location: u32,
}

impl PageRecord {
    pub fn encode(self, output: &mut [u8]) -> Result<(), MirageError> {
        exact_width(output, PAGE_RECORD_SIZE)?;
        write_bytes(output, 0, self.plaintext_hash.as_bytes())?;
        write_u32(output, 32, self.logical_length)?;
        write_u32(output, 36, self.remote_location)
    }

    pub fn decode(input: &[u8]) -> Result<Self, MirageError> {
        exact_width(input, PAGE_RECORD_SIZE)?;
        require_zero(&input[40..48])?;
        Ok(Self {
            plaintext_hash: PageHash::from_bytes(array_at(input, 0)?),
            logical_length: read_u32(input, 32)?,
            remote_location: read_u32(input, 36)?,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemoteLocationRecord {
    pub backend_id: StringRef,
    pub provider_object_id: StringRef,
    pub immutable_revision: Option<StringRef>,
    pub object_length: u64,
    pub object_hash: [u8; 32],
    pub object_kind: ObjectKind,
    pub codec: Codec,
    pub pack_offset: u64,
    pub encoded_length: u64,
}

impl RemoteLocationRecord {
    pub fn encode(self, output: &mut [u8]) -> Result<(), MirageError> {
        exact_width(output, REMOTE_LOCATION_RECORD_SIZE)?;
        write_u64(output, 0, self.backend_id.offset)?;
        write_u32(output, 8, self.backend_id.length)?;
        write_u32(output, 12, self.provider_object_id.length)?;
        write_u64(output, 16, self.provider_object_id.offset)?;
        if let Some(revision) = self.immutable_revision {
            write_u64(output, 24, revision.offset)?;
            write_u32(output, 32, revision.length)?;
            write_u32(output, 36, 1)?;
        }
        write_u64(output, 40, self.object_length)?;
        write_bytes(output, 48, &self.object_hash)?;
        output[80] = object_kind_code(self.object_kind);
        output[81] = self.codec.code();
        write_u64(output, 88, self.pack_offset)?;
        write_u64(output, 96, self.encoded_length)
    }

    pub fn decode(input: &[u8]) -> Result<Self, MirageError> {
        exact_width(input, REMOTE_LOCATION_RECORD_SIZE)?;
        require_zero(&input[82..88])?;
        let revision_present = read_u32(input, 36)?;
        let immutable_revision = match revision_present {
            0 => {
                require_zero(&input[24..36])?;
                None
            }
            1 => Some(StringRef {
                offset: read_u64(input, 24)?,
                length: read_u32(input, 32)?,
            }),
            _ => {
                return Err(MirageError::manifest_invalid(
                    "index revision presence flag is invalid",
                ));
            }
        };
        Ok(Self {
            backend_id: StringRef {
                offset: read_u64(input, 0)?,
                length: read_u32(input, 8)?,
            },
            provider_object_id: StringRef {
                offset: read_u64(input, 16)?,
                length: read_u32(input, 12)?,
            },
            immutable_revision,
            object_length: read_u64(input, 40)?,
            object_hash: array_at(input, 48)?,
            object_kind: decode_object_kind(input[80])?,
            codec: decode_codec(input[81])?,
            pack_offset: read_u64(input, 88)?,
            encoded_length: read_u64(input, 96)?,
        })
    }
}

fn exact_width(bytes: &[u8], expected: u32) -> Result<(), MirageError> {
    if bytes.len() == usize::try_from(expected).unwrap_or(usize::MAX) {
        Ok(())
    } else {
        Err(MirageError::manifest_invalid(
            "mount index record has an invalid width",
        ))
    }
}

fn require_zero(bytes: &[u8]) -> Result<(), MirageError> {
    if bytes.iter().all(|byte| *byte == 0) {
        Ok(())
    } else {
        Err(MirageError::unsupported_layout(
            "mount index reserved record bytes are nonzero",
        ))
    }
}

fn decode_file_class(code: u8) -> Result<FileClass, MirageError> {
    match code {
        0 => Ok(FileClass::NativeExecutable),
        1 => Ok(FileClass::NativeLibrary),
        2 => Ok(FileClass::NativeConfiguration),
        3 => Ok(FileClass::NativeMutable),
        4 => Ok(FileClass::VirtualAsset),
        5 => Ok(FileClass::VirtualContainer),
        _ => Err(MirageError::manifest_invalid("index file class is invalid")),
    }
}

fn object_kind_code(kind: ObjectKind) -> u8 {
    match kind {
        ObjectKind::RepositoryConfig => 0,
        ObjectKind::Pack => 1,
        ObjectKind::Manifest => 2,
        ObjectKind::Commit => 3,
        ObjectKind::Profile => 4,
    }
}

fn decode_object_kind(code: u8) -> Result<ObjectKind, MirageError> {
    match code {
        0 => Ok(ObjectKind::RepositoryConfig),
        1 => Ok(ObjectKind::Pack),
        2 => Ok(ObjectKind::Manifest),
        3 => Ok(ObjectKind::Commit),
        4 => Ok(ObjectKind::Profile),
        _ => Err(MirageError::manifest_invalid(
            "index object kind is invalid",
        )),
    }
}

fn decode_codec(code: u8) -> Result<Codec, MirageError> {
    match code {
        0 => Ok(Codec::None),
        1 => Ok(Codec::Zstd),
        2 => Ok(Codec::Lz4),
        _ => Err(MirageError::manifest_invalid("index codec is invalid")),
    }
}
