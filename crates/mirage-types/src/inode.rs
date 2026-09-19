//! Volume-scoped 128-bit inode identifiers and the versioned Windows naming
//! policy used to build directory lookup keys.

use crate::define_id128_type;
use crate::error::MirageError;

define_id128_type!(
    InodeId,
    "Unique 128-bit identifier for a namespace node, stable across renames."
);

/// The inode of a volume's root directory: fixed per volume so bootstrap does
/// not depend on allocation order.
pub const ROOT_INODE_SEED: [u8; 16] = *b"MIRAGE-ROOT-INOD";

/// Derives the root inode for a volume. Deterministic per volume so the root
/// is stable across rebuilds of the same namespace.
#[must_use]
pub fn root_inode(volume: crate::id::RepositoryId) -> InodeId {
    let mut bytes = ROOT_INODE_SEED;
    let volume_bytes = volume.as_bytes();
    for (index, byte) in volume_bytes.iter().enumerate() {
        bytes[index] ^= *byte;
    }
    InodeId::from_bytes(bytes)
}

/// Version of the folded-name policy stored with each volume; the lookup key
/// derivation must be bumped whenever the folding rules change.
pub const NAMING_POLICY_VERSION: u32 = 1;

/// Maximum display-name length in UTF-16 units, matching NTFS.
pub const MAX_NAME_UNITS: usize = 255;

const RESERVED_STEMS: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "CLOCK$", "CONIN$", "CONOUT$", "COM1", "COM2", "COM3", "COM4",
    "COM5", "COM6", "COM7", "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6",
    "LPT7", "LPT8", "LPT9",
];

/// Validates one display name under the v1 Windows naming policy and returns
/// its case-folded lookup key.
///
/// The policy rejects names that cannot exist on NTFS/WinFsp: path
/// separators and wildcard characters, reserved device stems, names ending in
/// a dot or space, control characters, and names over 255 UTF-16 units. The
/// folded key is the Unicode lowercase of the display spelling; the display
/// spelling itself is always preserved separately.
pub fn fold_name(display_name: &str) -> Result<String, MirageError> {
    if display_name.is_empty()
        || display_name.encode_utf16().count() > MAX_NAME_UNITS
        || display_name.ends_with(['.', ' '])
    {
        return Err(MirageError::invalid_argument(
            "name is empty, oversized, or has a trailing dot or space",
        ));
    }
    for character in display_name.chars() {
        if character.is_control() || matches!(character, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
        {
            return Err(MirageError::invalid_argument(
                "name contains a character that is invalid on Windows",
            ));
        }
    }
    let stem = display_name
        .split('.')
        .next()
        .unwrap_or_default()
        .to_uppercase();
    if RESERVED_STEMS.contains(&stem.as_str()) {
        return Err(MirageError::invalid_argument(
            "name uses a reserved Windows device name",
        ));
    }
    Ok(display_name
        .chars()
        .flat_map(char::to_uppercase)
        .collect())
}
