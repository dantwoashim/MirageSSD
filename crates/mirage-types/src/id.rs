//! Strongly typed identifiers for MirageSSD repositories, packs, capsules, sessions, and files.

use core::fmt;
use core::str::FromStr;

#[cfg(feature = "serde")]
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::MirageError;

/// Performs constant-time comparison of two 16-byte slices.
#[inline]
#[must_use]
pub fn constant_time_eq_16(a: &[u8; 16], b: &[u8; 16]) -> bool {
    let mut diff = 0u8;
    for i in 0..16 {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

/// Decodes a single canonical lowercase hexadecimal ASCII byte.
#[inline]
const fn decode_hex_nibble(b: u8) -> Result<u8, &'static str> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        b'A'..=b'F' => Err("uppercase hex character is non-canonical"),
        _ => Err("invalid hexadecimal character"),
    }
}

/// Decodes a canonical lowercase 32-character hex string into a 16-byte array.
pub fn parse_canonical_hex_16(s: &str) -> Result<[u8; 16], MirageError> {
    let bytes = s.as_bytes();
    if bytes.len() != 32 {
        return Err(MirageError::invalid_argument(format!(
            "invalid hex length: expected exactly 32 characters, got {}",
            bytes.len()
        )));
    }

    let mut out = [0u8; 16];
    for (i, target) in out.iter_mut().enumerate() {
        let hi = decode_hex_nibble(bytes[i * 2]).map_err(|e| {
            MirageError::invalid_argument(format!("invalid hex at index {}: {}", i * 2, e))
        })?;
        let lo = decode_hex_nibble(bytes[i * 2 + 1]).map_err(|e| {
            MirageError::invalid_argument(format!("invalid hex at index {}: {}", i * 2 + 1, e))
        })?;
        *target = (hi << 4) | lo;
    }
    Ok(out)
}

/// Formats a 16-byte array as a lowercase 32-character hex string.
pub fn encode_canonical_hex_16(bytes: &[u8; 16]) -> String {
    const HEX_CHARS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(32);
    for &b in bytes {
        out.push(HEX_CHARS[(b >> 4) as usize] as char);
        out.push(HEX_CHARS[(b & 0x0F) as usize] as char);
    }
    out
}

macro_rules! define_id128_type {
    ($name:ident, $doc:expr) => {
        #[doc = $doc]
        #[derive(Clone, Copy)]
        pub struct $name(pub [u8; 16]);

        impl $name {
            /// Creates a new identifier from a 16-byte array.
            #[inline]
            #[must_use]
            pub const fn from_bytes(bytes: [u8; 16]) -> Self {
                Self(bytes)
            }

            /// Returns a reference to the underlying 16-byte array.
            #[inline]
            #[must_use]
            pub const fn as_bytes(&self) -> &[u8; 16] {
                &self.0
            }

            /// Consumes self and returns the raw 16-byte array.
            #[inline]
            #[must_use]
            pub const fn into_bytes(self) -> [u8; 16] {
                self.0
            }

            /// Parses a 32-character lowercase canonical hex string.
            pub fn from_canonical_hex(s: &str) -> Result<Self, MirageError> {
                parse_canonical_hex_16(s).map(Self)
            }

            /// Performs constant-time comparison with another identifier.
            #[inline]
            #[must_use]
            pub fn constant_time_eq(&self, other: &Self) -> bool {
                constant_time_eq_16(&self.0, &other.0)
            }
        }

        impl PartialEq for $name {
            #[inline]
            fn eq(&self, other: &Self) -> bool {
                self.constant_time_eq(other)
            }
        }

        impl Eq for $name {}

        impl PartialOrd for $name {
            #[inline]
            fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
                Some(self.cmp(other))
            }
        }

        impl Ord for $name {
            #[inline]
            fn cmp(&self, other: &Self) -> core::cmp::Ordering {
                self.0.cmp(&other.0)
            }
        }

        impl core::hash::Hash for $name {
            #[inline]
            fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
                self.0.hash(state);
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(
                    f,
                    "{}({})",
                    stringify!($name),
                    encode_canonical_hex_16(&self.0)
                )
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", encode_canonical_hex_16(&self.0))
            }
        }

        impl fmt::LowerHex for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", encode_canonical_hex_16(&self.0))
            }
        }

        impl From<[u8; 16]> for $name {
            #[inline]
            fn from(bytes: [u8; 16]) -> Self {
                Self(bytes)
            }
        }

        impl From<$name> for [u8; 16] {
            #[inline]
            fn from(id: $name) -> Self {
                id.0
            }
        }

        impl FromStr for $name {
            type Err = MirageError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Self::from_canonical_hex(s)
            }
        }

        #[cfg(feature = "serde")]
        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.serialize_str(&encode_canonical_hex_16(&self.0))
            }
        }

        #[cfg(feature = "serde")]
        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let s = String::deserialize(deserializer)?;
                Self::from_canonical_hex(&s).map_err(serde::de::Error::custom)
            }
        }
    };
}

define_id128_type!(
    RepositoryId,
    "Unique 128-bit identifier for a MirageSSD virtualized repository."
);
define_id128_type!(
    PackId,
    "Unique 128-bit identifier for an immutable remote pack container."
);
define_id128_type!(
    CapsuleId,
    "Unique 128-bit identifier for a Sealed Session Capsule."
);
define_id128_type!(
    SessionId,
    "Unique 128-bit identifier for an active mount/virtualization session."
);
define_id128_type!(
    SpaceLeaseId,
    "Unique 128-bit identifier for a durable local-capacity promise."
);
define_id128_type!(
    UpdateId,
    "Unique 128-bit identifier for an in-progress or staged update transaction."
);
define_id128_type!(
    DeviceId,
    "Unique 128-bit identifier for the single authorized writer device."
);

/// Monotonically increasing 64-bit generation number for repository state revisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct GenerationId(pub u64);

impl GenerationId {
    /// Initial baseline generation 0.
    pub const ZERO: Self = Self(0);

    /// Creates a generation identifier from a raw u64.
    #[inline]
    #[must_use]
    pub const fn from_u64(val: u64) -> Self {
        Self(val)
    }

    /// Returns the raw u64 value of the generation.
    #[inline]
    #[must_use]
    pub const fn as_u64(&self) -> u64 {
        self.0
    }

    /// Computes the next monotonic generation (N + 1), checking for overflow.
    #[must_use]
    pub const fn next(&self) -> Option<Self> {
        match self.0.checked_add(1) {
            Some(n) => Some(Self(n)),
            None => None,
        }
    }
}

impl fmt::Display for GenerationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "gen-{}", self.0)
    }
}

impl From<u64> for GenerationId {
    #[inline]
    fn from(v: u64) -> Self {
        Self(v)
    }
}

impl From<GenerationId> for u64 {
    #[inline]
    fn from(gen_id: GenerationId) -> Self {
        gen_id.0
    }
}

impl FromStr for GenerationId {
    type Err = MirageError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let trimmed = s.strip_prefix("gen-").unwrap_or(s);
        trimmed
            .parse::<u64>()
            .map(Self)
            .map_err(|e| MirageError::invalid_argument(format!("invalid generation id: {}", e)))
    }
}

/// Stable 64-bit file identifier within a manifest tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct StableFileId(pub u64);

impl StableFileId {
    /// Creates a stable file ID from a raw u64.
    #[inline]
    #[must_use]
    pub const fn from_u64(val: u64) -> Self {
        Self(val)
    }

    /// Returns the raw u64 value.
    #[inline]
    #[must_use]
    pub const fn as_u64(&self) -> u64 {
        self.0
    }

    /// Parses a 16-character canonical lowercase hex string into a StableFileId.
    pub fn from_canonical_hex(s: &str) -> Result<Self, MirageError> {
        let bytes = s.as_bytes();
        if bytes.len() != 16 {
            return Err(MirageError::invalid_argument(format!(
                "invalid StableFileId hex length: expected exactly 16 characters, got {}",
                bytes.len()
            )));
        }

        let mut val = 0u64;
        for &b in bytes {
            let nibble = decode_hex_nibble(b).map_err(|e| {
                MirageError::invalid_argument(format!("invalid hex character: {}", e))
            })?;
            val = (val << 4) | (nibble as u64);
        }
        Ok(Self(val))
    }

    /// Formats the file ID as a 16-character lowercase hex string.
    #[must_use]
    pub fn to_canonical_hex(&self) -> String {
        format!("{:016x}", self.0)
    }
}

impl fmt::Display for StableFileId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:016x}", self.0)
    }
}

impl fmt::LowerHex for StableFileId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:016x}", self.0)
    }
}

impl From<u64> for StableFileId {
    #[inline]
    fn from(v: u64) -> Self {
        Self(v)
    }
}

impl From<StableFileId> for u64 {
    #[inline]
    fn from(id: StableFileId) -> Self {
        id.0
    }
}

impl FromStr for StableFileId {
    type Err = MirageError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.len() == 16 {
            Self::from_canonical_hex(s)
        } else {
            s.parse::<u64>()
                .map(Self)
                .map_err(|e| MirageError::invalid_argument(format!("invalid StableFileId: {}", e)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_id128_hex_round_trip() {
        let raw = [0x42u8; 16];
        let repo_id = RepositoryId::from_bytes(raw);
        let s = repo_id.to_string();
        assert_eq!(s.len(), 32);

        let parsed = RepositoryId::from_canonical_hex(&s).expect("parse valid hex");
        assert_eq!(parsed, repo_id);
    }

    #[test]
    fn test_id128_rejections() {
        let upper = "42".repeat(15) + "4a";
        assert!(RepositoryId::from_canonical_hex(&upper).is_ok());

        let real_upper = "42".repeat(15) + "4A";
        assert!(RepositoryId::from_canonical_hex(&real_upper).is_err());

        let short = "42".repeat(15);
        assert!(RepositoryId::from_canonical_hex(&short).is_err());
    }

    #[test]
    fn test_generation_id_monotonic() {
        let g0 = GenerationId::ZERO;
        let g1 = g0.next().expect("next generation");
        assert_eq!(g1.as_u64(), 1);
        assert_eq!(g1.to_string(), "gen-1");
        assert_eq!("gen-1".parse::<GenerationId>().unwrap(), g1);
    }

    #[test]
    fn test_stable_file_id_hex_round_trip() {
        let id = StableFileId(0x1234_5678_9abc_def0);
        let hex = id.to_canonical_hex();
        assert_eq!(hex, "123456789abcdef0");
        let parsed = StableFileId::from_canonical_hex(&hex).unwrap();
        assert_eq!(parsed, id);
    }
}
