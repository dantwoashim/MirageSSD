//! Strongly typed numerical primitives for byte counts, file offsets, page ordinals, and cache slot indices.

use core::fmt;

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

/// Strongly typed byte count / size wrapper preventing mixing of offsets and sizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct ByteCount(pub u64);

impl ByteCount {
    /// Zero bytes.
    pub const ZERO: Self = Self(0);

    /// Creates a byte count from a raw `u64`.
    #[inline]
    #[must_use]
    pub const fn from_u64(bytes: u64) -> Self {
        Self(bytes)
    }

    /// Returns the raw `u64` byte count.
    #[inline]
    #[must_use]
    pub const fn as_u64(self) -> u64 {
        self.0
    }

    /// Returns `true` if the byte count is 0.
    #[inline]
    #[must_use]
    pub const fn is_zero(self) -> bool {
        self.0 == 0
    }

    /// Checked addition of two byte counts.
    #[inline]
    #[must_use]
    pub const fn checked_add(self, rhs: Self) -> Option<Self> {
        match self.0.checked_add(rhs.0) {
            Some(v) => Some(Self(v)),
            None => None,
        }
    }

    /// Checked subtraction of two byte counts.
    #[inline]
    #[must_use]
    pub const fn checked_sub(self, rhs: Self) -> Option<Self> {
        match self.0.checked_sub(rhs.0) {
            Some(v) => Some(Self(v)),
            None => None,
        }
    }

    /// Checked multiplication of byte count by a scalar factor.
    #[inline]
    #[must_use]
    pub const fn checked_mul(self, rhs: u64) -> Option<Self> {
        match self.0.checked_mul(rhs) {
            Some(v) => Some(Self(v)),
            None => None,
        }
    }

    /// Checked integer division of byte count by a scalar divisor.
    #[inline]
    #[must_use]
    pub const fn checked_div(self, rhs: u64) -> Option<Self> {
        if rhs == 0 {
            None
        } else {
            Some(Self(self.0 / rhs))
        }
    }

    /// Saturating addition of byte counts.
    #[inline]
    #[must_use]
    pub const fn saturating_add(self, rhs: Self) -> Self {
        Self(self.0.saturating_add(rhs.0))
    }

    /// Constructs a `ByteCount` from KiB (kibibytes = 1,024 bytes).
    #[inline]
    #[must_use]
    pub const fn from_kib(kib: u64) -> Option<Self> {
        Self(kib).checked_mul(1024)
    }

    /// Constructs a `ByteCount` from MiB (mebibytes = 1,048,576 bytes).
    #[inline]
    #[must_use]
    pub const fn from_mib(mib: u64) -> Option<Self> {
        Self(mib).checked_mul(1024 * 1024)
    }

    /// Constructs a `ByteCount` from GiB (gibibytes = 1,073,741,824 bytes).
    #[inline]
    #[must_use]
    pub const fn from_gib(gib: u64) -> Option<Self> {
        Self(gib).checked_mul(1024 * 1024 * 1024)
    }
}

impl fmt::Display for ByteCount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} B", self.0)
    }
}

impl From<u64> for ByteCount {
    #[inline]
    fn from(v: u64) -> Self {
        Self(v)
    }
}

impl From<ByteCount> for u64 {
    #[inline]
    fn from(b: ByteCount) -> Self {
        b.0
    }
}

/// Strongly typed file or container offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct FileOffset(pub u64);

impl FileOffset {
    /// Offset zero (beginning of file).
    pub const ZERO: Self = Self(0);

    /// Creates a file offset from a raw `u64`.
    #[inline]
    #[must_use]
    pub const fn from_u64(offset: u64) -> Self {
        Self(offset)
    }

    /// Returns the raw `u64` offset.
    #[inline]
    #[must_use]
    pub const fn as_u64(self) -> u64 {
        self.0
    }

    /// Checked addition of a byte count to an offset, producing a new `FileOffset`.
    #[inline]
    #[must_use]
    pub const fn checked_add_bytes(self, bytes: ByteCount) -> Option<Self> {
        match self.0.checked_add(bytes.0) {
            Some(v) => Some(Self(v)),
            None => None,
        }
    }

    /// Checked subtraction of a starting offset from this offset, producing the `ByteCount` span between them.
    #[inline]
    #[must_use]
    pub const fn checked_sub_offset(self, start: Self) -> Option<ByteCount> {
        match self.0.checked_sub(start.0) {
            Some(v) => Some(ByteCount(v)),
            None => None,
        }
    }

    /// Checked subtraction of a byte count from an offset.
    #[inline]
    #[must_use]
    pub const fn checked_sub_bytes(self, bytes: ByteCount) -> Option<Self> {
        match self.0.checked_sub(bytes.0) {
            Some(v) => Some(Self(v)),
            None => None,
        }
    }

    /// Computes the zero-based page ordinal containing this offset for a given page size.
    #[inline]
    #[must_use]
    pub const fn page_ordinal(self, page_size: ByteCount) -> Option<PageOrdinal> {
        if page_size.is_zero() {
            return None;
        }
        let page_u64 = self.0 / page_size.0;
        if page_u64 > (u32::MAX as u64) {
            None
        } else {
            Some(PageOrdinal(page_u64 as u32))
        }
    }

    /// Computes the byte offset within the page for a given page size.
    #[inline]
    #[must_use]
    pub const fn page_offset(self, page_size: ByteCount) -> Option<u32> {
        if page_size.is_zero() {
            return None;
        }
        let rem = self.0 % page_size.0;
        if rem > (u32::MAX as u64) {
            None
        } else {
            Some(rem as u32)
        }
    }
}

impl fmt::Display for FileOffset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "offset:0x{:x}", self.0)
    }
}

impl From<u64> for FileOffset {
    #[inline]
    fn from(v: u64) -> Self {
        Self(v)
    }
}

impl From<FileOffset> for u64 {
    #[inline]
    fn from(o: FileOffset) -> Self {
        o.0
    }
}

/// Zero-based index of a 1 MiB virtual storage page within an asset file or container.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct PageOrdinal(pub u32);

impl PageOrdinal {
    /// First page ordinal 0.
    pub const ZERO: Self = Self(0);

    /// Creates a page ordinal from a `u32`.
    #[inline]
    #[must_use]
    pub const fn from_u32(val: u32) -> Self {
        Self(val)
    }

    /// Returns the raw `u32` ordinal.
    #[inline]
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self.0
    }

    /// Returns the ordinal widened to `u64`.
    #[inline]
    #[must_use]
    pub const fn as_u64(self) -> u64 {
        self.0 as u64
    }

    /// Checked addition of a page count delta.
    #[inline]
    #[must_use]
    pub const fn checked_add(self, delta: u32) -> Option<Self> {
        match self.0.checked_add(delta) {
            Some(v) => Some(Self(v)),
            None => None,
        }
    }

    /// Checked subtraction of a page count delta.
    #[inline]
    #[must_use]
    pub const fn checked_sub(self, delta: u32) -> Option<Self> {
        match self.0.checked_sub(delta) {
            Some(v) => Some(Self(v)),
            None => None,
        }
    }

    /// Computes the start `FileOffset` of this page given a page size.
    #[inline]
    #[must_use]
    pub const fn to_file_offset(self, page_size: ByteCount) -> Option<FileOffset> {
        match (self.0 as u64).checked_mul(page_size.0) {
            Some(off) => Some(FileOffset(off)),
            None => None,
        }
    }

    /// Computes the page ordinal from a file offset and page size.
    #[inline]
    #[must_use]
    pub const fn from_file_offset(offset: FileOffset, page_size: ByteCount) -> Option<Self> {
        offset.page_ordinal(page_size)
    }
}

impl fmt::Display for PageOrdinal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "page#{}", self.0)
    }
}

impl From<u32> for PageOrdinal {
    #[inline]
    fn from(v: u32) -> Self {
        Self(v)
    }
}

impl From<PageOrdinal> for u32 {
    #[inline]
    fn from(p: PageOrdinal) -> Self {
        p.0
    }
}

/// Zero-based physical slot index in the sparse fixed-slot SSD cache arena.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct SlotIndex(pub u32);

impl SlotIndex {
    /// Slot index 0.
    pub const ZERO: Self = Self(0);

    /// Creates a slot index from a `u32`.
    #[inline]
    #[must_use]
    pub const fn from_u32(val: u32) -> Self {
        Self(val)
    }

    /// Returns the raw `u32` slot index.
    #[inline]
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self.0
    }

    /// Returns the slot index as `usize` for array indexing.
    #[inline]
    #[must_use]
    pub const fn as_usize(self) -> usize {
        self.0 as usize
    }

    /// Checked addition of a slot delta.
    #[inline]
    #[must_use]
    pub const fn checked_add(self, delta: u32) -> Option<Self> {
        match self.0.checked_add(delta) {
            Some(v) => Some(Self(v)),
            None => None,
        }
    }

    /// Checked subtraction of a slot delta.
    #[inline]
    #[must_use]
    pub const fn checked_sub(self, delta: u32) -> Option<Self> {
        match self.0.checked_sub(delta) {
            Some(v) => Some(Self(v)),
            None => None,
        }
    }
}

impl fmt::Display for SlotIndex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "slot[{}]", self.0)
    }
}

impl From<u32> for SlotIndex {
    #[inline]
    fn from(v: u32) -> Self {
        Self(v)
    }
}

impl From<SlotIndex> for u32 {
    #[inline]
    fn from(s: SlotIndex) -> Self {
        s.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_byte_count_units_and_math() {
        let one_mib = ByteCount::from_mib(1).expect("1 MiB");
        assert_eq!(one_mib.as_u64(), 1024 * 1024);

        let two_mib = one_mib.checked_add(one_mib).expect("2 MiB");
        assert_eq!(two_mib.as_u64(), 2 * 1024 * 1024);

        let sub = two_mib.checked_sub(one_mib).expect("sub");
        assert_eq!(sub, one_mib);
    }

    #[test]
    fn test_file_offset_page_math() {
        let page_size = ByteCount::from_mib(1).expect("1 MiB");
        let offset = FileOffset(2 * 1024 * 1024 + 4096);

        let page = offset.page_ordinal(page_size).expect("page ordinal");
        assert_eq!(page, PageOrdinal(2));

        let in_page_offset = offset.page_offset(page_size).expect("in-page offset");
        assert_eq!(in_page_offset, 4096);

        let back_offset = page.to_file_offset(page_size).expect("back offset");
        assert_eq!(back_offset, FileOffset(2 * 1024 * 1024));
    }
}
