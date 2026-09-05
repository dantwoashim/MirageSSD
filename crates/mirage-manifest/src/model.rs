use mirage_backend::RemoteObjectRef;
use mirage_types::{ByteCount, GenerationId, MirageError, PageHash, RepositoryId, StableFileId};
use serde::{Deserialize, Serialize};

pub const MANIFEST_FORMAT_VERSION: u32 = 2;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryManifest {
    pub format_version: u32,
    pub repository_id: RepositoryId,
    pub generation_id: GenerationId,
    pub page_size: ByteCount,
    pub directories: Vec<DirectoryRecord>,
    pub files: Vec<FileRecord>,
    pub extents: Vec<ExtentRecord>,
    pub pages: Vec<PageRecord>,
    pub remote_locations: Vec<RemoteLocation>,
}

impl RepositoryManifest {
    pub fn summary(&self) -> Result<ManifestSummary, MirageError> {
        crate::validate::validate_manifest(self)?;
        crate::validate::manifest_summary(self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectoryRecord {
    pub parent: Option<u32>,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileRecord {
    pub parent_directory: u32,
    pub name: String,
    pub logical_size: ByteCount,
    pub stable_id: StableFileId,
    pub class: FileClass,
    pub extent_start: u32,
    pub extent_count: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileClass {
    NativeExecutable,
    NativeLibrary,
    NativeConfiguration,
    NativeMutable,
    VirtualAsset,
    VirtualContainer,
}

impl FileClass {
    #[must_use]
    pub const fn is_virtual(self) -> bool {
        matches!(self, Self::VirtualAsset | Self::VirtualContainer)
    }

    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::NativeExecutable => 0,
            Self::NativeLibrary => 1,
            Self::NativeConfiguration => 2,
            Self::NativeMutable => 3,
            Self::VirtualAsset => 4,
            Self::VirtualContainer => 5,
        }
    }

    pub(crate) fn from_code(code: u8) -> Result<Self, MirageError> {
        match code {
            0 => Ok(Self::NativeExecutable),
            1 => Ok(Self::NativeLibrary),
            2 => Ok(Self::NativeConfiguration),
            3 => Ok(Self::NativeMutable),
            4 => Ok(Self::VirtualAsset),
            5 => Ok(Self::VirtualContainer),
            _ => Err(MirageError::manifest_invalid("unknown file class")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtentRecord {
    pub logical_offset: u64,
    pub logical_length: ByteCount,
    pub page_start: u32,
    pub page_count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageRecord {
    pub plaintext_hash: PageHash,
    pub logical_length: u32,
    pub remote_location: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Codec {
    None,
    Zstd,
    Lz4,
}

impl Codec {
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::None => 0,
            Self::Zstd => 1,
            Self::Lz4 => 2,
        }
    }

    pub(crate) fn from_code(code: u8) -> Result<Self, MirageError> {
        match code {
            0 => Ok(Self::None),
            1 => Ok(Self::Zstd),
            2 => Ok(Self::Lz4),
            _ => Err(MirageError::manifest_invalid("unknown page codec")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteLocation {
    pub object: RemoteObjectRef,
    pub offset: u64,
    pub encoded_length: ByteCount,
    pub codec: Codec,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestSummary {
    pub total_logical_bytes: ByteCount,
    pub unique_page_count: u64,
    pub native_logical_bytes: ByteCount,
    pub virtual_logical_bytes: ByteCount,
}
