//! Strongly typed 256-bit cryptographic hashes with canonical hex parsing and constant-time equality.

use core::fmt;
use core::str::FromStr;

#[cfg(feature = "serde")]
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::MirageError;

/// Performs constant-time comparison of two 32-byte slices to prevent timing side channels.
#[inline]
#[must_use]
pub fn constant_time_eq_32(a: &[u8; 32], b: &[u8; 32]) -> bool {
    let mut diff = 0u8;
    for i in 0..32 {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

/// Decodes a single canonical lowercase hexadecimal ASCII byte into its 4-bit nibble value.
/// Returns `Err` if the character is uppercase or non-hexadecimal.
#[inline]
const fn decode_hex_nibble(b: u8) -> Result<u8, &'static str> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        b'A'..=b'F' => Err("uppercase hex character is non-canonical"),
        _ => Err("invalid hexadecimal character"),
    }
}

/// Decodes a canonical lowercase 64-character hex string into a 32-byte array.
pub fn parse_canonical_hex_32(s: &str) -> Result<[u8; 32], MirageError> {
    let bytes = s.as_bytes();
    if bytes.len() != 64 {
        return Err(MirageError::invalid_argument(format!(
            "invalid hex length: expected exactly 64 characters, got {}",
            bytes.len()
        )));
    }

    let mut out = [0u8; 32];
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

/// Formats a 32-byte array as a lowercase 64-character hex string.
pub fn encode_canonical_hex_32(bytes: &[u8; 32]) -> String {
    const HEX_CHARS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(64);
    for &b in bytes {
        out.push(HEX_CHARS[(b >> 4) as usize] as char);
        out.push(HEX_CHARS[(b & 0x0F) as usize] as char);
    }
    out
}

macro_rules! define_hash_type {
    ($name:ident, $doc:expr) => {
        #[doc = $doc]
        #[derive(Clone, Copy)]
        pub struct $name(pub [u8; 32]);

        impl $name {
            /// Creates a new hash instance from raw 32 bytes.
            #[inline]
            #[must_use]
            pub const fn from_bytes(bytes: [u8; 32]) -> Self {
                Self(bytes)
            }

            /// Returns a reference to the underlying 32-byte array.
            #[inline]
            #[must_use]
            pub const fn as_bytes(&self) -> &[u8; 32] {
                &self.0
            }

            /// Consumes self and returns the raw 32-byte array.
            #[inline]
            #[must_use]
            pub const fn into_bytes(self) -> [u8; 32] {
                self.0
            }

            /// Parses a 64-character lowercase canonical hex string.
            pub fn from_canonical_hex(s: &str) -> Result<Self, MirageError> {
                parse_canonical_hex_32(s).map(Self)
            }

            /// Constant-time comparison with another hash of the same type.
            #[inline]
            #[must_use]
            pub fn constant_time_eq(&self, other: &Self) -> bool {
                constant_time_eq_32(&self.0, &other.0)
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
                    encode_canonical_hex_32(&self.0)
                )
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", encode_canonical_hex_32(&self.0))
            }
        }

        impl fmt::LowerHex for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", encode_canonical_hex_32(&self.0))
            }
        }

        impl From<[u8; 32]> for $name {
            #[inline]
            fn from(bytes: [u8; 32]) -> Self {
                Self(bytes)
            }
        }

        impl From<$name> for [u8; 32] {
            #[inline]
            fn from(hash: $name) -> Self {
                hash.0
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
                serializer.serialize_str(&encode_canonical_hex_32(&self.0))
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

define_hash_type!(
    CommitHash,
    "Cryptographic 256-bit BLAKE3 hash identifying an immutable repository commit."
);
define_hash_type!(
    ManifestHash,
    "Cryptographic 256-bit BLAKE3 hash identifying an immutable manifest tree."
);
define_hash_type!(
    PageHash,
    "Cryptographic 256-bit BLAKE3 content hash verifying a 1 MiB virtual storage page."
);
define_hash_type!(
    ContentHash,
    "Cryptographic 256-bit BLAKE3 hash verifying an immutable object or bounded byte source."
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_canonical_hex_parsing_and_formatting() {
        let raw = [0xabu8; 32];
        let hash = CommitHash::from_bytes(raw);
        let hex = hash.to_string();
        assert_eq!(hex.len(), 64);
        assert!(hex.chars().all(|c| c == 'a' || c == 'b'));

        let parsed = CommitHash::from_canonical_hex(&hex).expect("valid lowercase hex");
        assert_eq!(parsed, hash);
    }

    #[test]
    fn test_canonical_hex_rejections() {
        // Uppercase rejected
        let upper = "AB".repeat(32);
        assert!(CommitHash::from_canonical_hex(&upper).is_err());

        // Short length rejected
        let short = "ab".repeat(31);
        assert!(CommitHash::from_canonical_hex(&short).is_err());

        // Long length rejected
        let long = "ab".repeat(33);
        assert!(CommitHash::from_canonical_hex(&long).is_err());

        // Invalid characters rejected
        let invalid = "zz".repeat(32);
        assert!(CommitHash::from_canonical_hex(&invalid).is_err());
    }

    #[test]
    fn test_constant_time_equality() {
        let a = PageHash::from_bytes([0x12; 32]);
        let b = PageHash::from_bytes([0x12; 32]);
        let mut c_bytes = [0x12; 32];
        c_bytes[31] = 0x13;
        let c = PageHash::from_bytes(c_bytes);

        assert!(a.constant_time_eq(&b));
        assert!(!a.constant_time_eq(&c));
        assert_eq!(a, b);
        assert_ne!(a, c);
    }
}
