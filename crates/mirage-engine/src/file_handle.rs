use core::fmt;

use mirage_types::{ByteCount, GenerationId, MirageError};

/// Platform-neutral requested access capabilities. Win32 flags are mapped only by the FFI crate.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct AccessMask(u8);

impl AccessMask {
    pub const READ_DATA: Self = Self(0b0001);
    pub const READ_METADATA: Self = Self(0b0010);
    pub const WRITE_DATA: Self = Self(0b0100);
    pub const DELETE: Self = Self(0b1000);
    pub const READ_ONLY: Self = Self(Self::READ_DATA.0 | Self::READ_METADATA.0);
    const KNOWN_BITS: u8 =
        Self::READ_DATA.0 | Self::READ_METADATA.0 | Self::WRITE_DATA.0 | Self::DELETE.0;

    pub fn from_bits(bits: u8) -> Result<Self, MirageError> {
        if bits & !Self::KNOWN_BITS != 0 {
            return Err(MirageError::invalid_argument(
                "access mask contains unknown capabilities",
            ));
        }
        Ok(Self(bits))
    }

    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }

    #[must_use]
    pub const fn contains(self, capability: Self) -> bool {
        self.0 & capability.0 == capability.0
    }
}

impl fmt::Debug for AccessMask {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("AccessMask")
            .field(&format_args!("{:#06b}", self.0))
            .finish()
    }
}

impl core::ops::BitOr for AccessMask {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

/// Immutable context returned by path/index resolution and held across reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileHandleContext {
    file_index: u32,
    generation_id: GenerationId,
    logical_size: ByteCount,
    access: AccessMask,
}

impl FileHandleContext {
    #[must_use]
    pub const fn new(
        file_index: u32,
        generation_id: GenerationId,
        logical_size: ByteCount,
        access: AccessMask,
    ) -> Self {
        Self {
            file_index,
            generation_id,
            logical_size,
            access,
        }
    }

    #[must_use]
    pub const fn file_index(&self) -> u32 {
        self.file_index
    }

    #[must_use]
    pub const fn generation_id(&self) -> GenerationId {
        self.generation_id
    }

    #[must_use]
    pub const fn logical_size(&self) -> ByteCount {
        self.logical_size
    }

    #[must_use]
    pub const fn access(&self) -> AccessMask {
        self.access
    }
}
