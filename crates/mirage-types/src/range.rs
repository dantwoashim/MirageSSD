//! Overflow-safe byte range operations, containment, intersection, EOF clipping, and page splitting.

use core::fmt;

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use crate::bytes::{ByteCount, FileOffset, PageOrdinal};
use crate::error::MirageError;

/// A slice of a `CheckedRange` mapped to a specific 1 MiB virtual storage page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct PageSlice {
    /// Zero-based page ordinal.
    pub page: PageOrdinal,
    /// Byte offset within the page (0 <= offset < page_size).
    pub offset_in_page: u32,
    /// Byte length of data in this page (0 < length <= page_size).
    pub length: u32,
    /// Absolute file offset where this slice starts.
    pub file_offset: FileOffset,
}

/// An immutable, overflow-validated byte range `[start, start + len)`.
///
/// Guarantees at construction time that `start + len` does not overflow `u64::MAX`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct CheckedRange {
    start: u64,
    len: u64,
}

impl CheckedRange {
    /// Constructs a `CheckedRange` from `start` and `len`, returning an error if `start + len` overflows `u64::MAX`.
    pub fn new(start: u64, len: u64) -> Result<Self, MirageError> {
        match start.checked_add(len) {
            Some(_) => Ok(Self { start, len }),
            None => Err(MirageError::invalid_argument(format!(
                "range overflow: start ({}) + len ({}) exceeds u64::MAX",
                start, len
            ))),
        }
    }

    /// Constructs a `CheckedRange` from `start` and `len`, returning `None` if `start + len` overflows `u64::MAX`.
    #[inline]
    #[must_use]
    pub const fn try_new(start: u64, len: u64) -> Option<Self> {
        match start.checked_add(len) {
            Some(_) => Some(Self { start, len }),
            None => None,
        }
    }

    /// Constructs a `CheckedRange` from `start` and half-open `end_exclusive`.
    ///
    /// Returns an error if `end_exclusive < start` or if bounds overflow.
    pub fn from_start_and_end(start: u64, end_exclusive: u64) -> Result<Self, MirageError> {
        if end_exclusive < start {
            return Err(MirageError::invalid_argument(format!(
                "invalid range bounds: end_exclusive ({}) is less than start ({})",
                end_exclusive, start
            )));
        }
        let len = end_exclusive - start;
        Self::new(start, len)
    }

    /// Creates an empty range of length 0 starting at `start`.
    #[inline]
    #[must_use]
    pub const fn empty(start: u64) -> Self {
        Self { start, len: 0 }
    }

    /// Creates a range from typed `FileOffset` and `ByteCount`.
    pub fn from_offsets(start: FileOffset, len: ByteCount) -> Result<Self, MirageError> {
        Self::new(start.0, len.0)
    }

    /// Returns the start byte offset of the range.
    #[inline]
    #[must_use]
    pub const fn start(&self) -> u64 {
        self.start
    }

    /// Returns the byte length of the range.
    #[inline]
    #[must_use]
    pub const fn len(&self) -> u64 {
        self.len
    }

    /// Returns `true` if the range is empty (`len == 0`).
    #[inline]
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Returns the start offset as a strongly typed `FileOffset`.
    #[inline]
    #[must_use]
    pub const fn start_offset(&self) -> FileOffset {
        FileOffset(self.start)
    }

    /// Returns the byte length as a strongly typed `ByteCount`.
    #[inline]
    #[must_use]
    pub const fn byte_count(&self) -> ByteCount {
        ByteCount(self.len)
    }

    /// Returns the exclusive end byte offset `start + len`.
    ///
    /// Because validity of `start + len` is an invariant checked at construction, this is guaranteed not to overflow.
    #[inline]
    #[must_use]
    pub const fn end_exclusive(&self) -> u64 {
        match self.start.checked_add(self.len) {
            Some(end) => end,
            None => u64::MAX,
        }
    }

    /// Returns the exclusive end as a strongly typed `FileOffset`.
    #[inline]
    #[must_use]
    pub const fn end_offset(&self) -> FileOffset {
        FileOffset(self.end_exclusive())
    }

    /// Returns `true` if the given byte offset falls within this range `[start, end_exclusive)`.
    ///
    /// An empty range contains no offsets.
    #[must_use]
    pub fn contains_offset(&self, offset: u64) -> bool {
        if self.is_empty() {
            false
        } else {
            offset >= self.start && offset < self.end_exclusive()
        }
    }

    /// Returns `true` if this range completely encloses `other`.
    #[must_use]
    pub fn contains_range(&self, other: &Self) -> bool {
        if other.is_empty() {
            other.start >= self.start && other.start <= self.end_exclusive()
        } else if self.is_empty() {
            false
        } else {
            other.start >= self.start && other.end_exclusive() <= self.end_exclusive()
        }
    }

    /// Computes the intersection with another range, returning `None` if they do not overlap.
    #[must_use]
    pub fn intersection(&self, other: &Self) -> Option<Self> {
        if self.is_empty() || other.is_empty() {
            return None;
        }
        let max_start = self.start.max(other.start);
        let min_end = self.end_exclusive().min(other.end_exclusive());
        if max_start < min_end {
            Some(Self {
                start: max_start,
                len: min_end - max_start,
            })
        } else {
            None
        }
    }

    /// Returns `true` if this range overlaps with `other` (non-empty intersection).
    #[must_use]
    pub fn overlaps(&self, other: &Self) -> bool {
        self.intersection(other).is_some()
    }

    /// Clips this range to the specified `file_size` EOF boundary `[0, file_size)`.
    ///
    /// If the range starts at or after EOF, returns an empty range at `file_size`.
    #[must_use]
    pub fn clip_to_eof(&self, file_size: u64) -> Self {
        if self.start >= file_size {
            Self::empty(file_size)
        } else {
            let clamped_end = self.end_exclusive().min(file_size);
            Self {
                start: self.start,
                len: clamped_end - self.start,
            }
        }
    }

    /// Splits this range into individual page slices for a given virtual page size.
    ///
    /// Returns an error if `page_size == 0` or if page ordinals exceed `u32::MAX`.
    pub fn split_to_pages(&self, page_size: u64) -> Result<Vec<PageSlice>, MirageError> {
        if page_size == 0 {
            return Err(MirageError::invalid_argument(
                "page_size must be greater than 0",
            ));
        }
        if page_size > (u32::MAX as u64) {
            return Err(MirageError::unsupported_layout(
                "page_size exceeds maximum supported 32-bit page size",
            ));
        }
        if self.is_empty() {
            return Ok(Vec::new());
        }

        let end = self.end_exclusive();
        let first_page = self.start / page_size;
        let last_page = (end - 1) / page_size;

        if last_page > (u32::MAX as u64) {
            return Err(MirageError::unsupported_layout(
                "page ordinal exceeds u32::MAX",
            ));
        }

        let slice_count = (last_page - first_page + 1) as usize;
        let mut slices = Vec::with_capacity(slice_count);
        let mut cur_offset = self.start;

        for page in first_page..=last_page {
            let page_start = page * page_size;
            let offset_in_page = (cur_offset - page_start) as u32;
            let remaining_in_page = page_size - (offset_in_page as u64);
            let remaining_in_range = end - cur_offset;
            let bytes_in_page = remaining_in_page.min(remaining_in_range) as u32;

            slices.push(PageSlice {
                page: PageOrdinal(page as u32),
                offset_in_page,
                length: bytes_in_page,
                file_offset: FileOffset(cur_offset),
            });

            cur_offset = match cur_offset.checked_add(bytes_in_page as u64) {
                Some(next) => next,
                None => break,
            };
        }

        Ok(slices)
    }
}

impl fmt::Display for CheckedRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "[{}..{}) len={}",
            self.start,
            self.end_exclusive(),
            self.len
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_overflow_protection() {
        assert!(CheckedRange::new(u64::MAX, 1).is_err());
        assert!(CheckedRange::new(u64::MAX - 10, 11).is_err());
        assert!(CheckedRange::new(u64::MAX - 10, 10).is_ok());
        assert_eq!(
            CheckedRange::new(u64::MAX - 10, 10)
                .unwrap()
                .end_exclusive(),
            u64::MAX
        );
    }

    #[test]
    fn test_containment_and_intersection() {
        let r1 = CheckedRange::new(100, 200).unwrap(); // [100, 300)
        assert!(r1.contains_offset(100));
        assert!(r1.contains_offset(299));
        assert!(!r1.contains_offset(300));
        assert!(!r1.contains_offset(99));

        let r2 = CheckedRange::new(150, 50).unwrap(); // [150, 200)
        assert!(r1.contains_range(&r2));
        assert!(!r2.contains_range(&r1));

        let r3 = CheckedRange::new(250, 100).unwrap(); // [250, 350)
        let inter = r1.intersection(&r3).unwrap();
        assert_eq!(inter.start(), 250);
        assert_eq!(inter.len(), 50);
        assert_eq!(inter.end_exclusive(), 300);

        let disjoint = CheckedRange::new(400, 50).unwrap();
        assert!(r1.intersection(&disjoint).is_none());
    }

    #[test]
    fn test_eof_clipping() {
        let r = CheckedRange::new(50, 100).unwrap(); // [50, 150)
        let clipped = r.clip_to_eof(120);
        assert_eq!(clipped.start(), 50);
        assert_eq!(clipped.len(), 70);
        assert_eq!(clipped.end_exclusive(), 120);

        let out_of_bounds = CheckedRange::new(200, 50).unwrap();
        let clipped_empty = out_of_bounds.clip_to_eof(120);
        assert!(clipped_empty.is_empty());
        assert_eq!(clipped_empty.start(), 120);
    }

    #[test]
    fn test_split_to_pages() {
        let page_size = 1024 * 1024; // 1 MiB
        // Range spans across 3 pages: page 0 (end part), page 1 (full), page 2 (start part)
        let start = 512 * 1024; // 512 KiB (in page 0)
        let len = 2 * 1024 * 1024; // 2 MiB -> reaches 2.5 MiB (in page 2)
        let range = CheckedRange::new(start, len).unwrap();

        let slices = range.split_to_pages(page_size).unwrap();
        assert_eq!(slices.len(), 3);

        assert_eq!(slices[0].page, PageOrdinal(0));
        assert_eq!(slices[0].offset_in_page, 512 * 1024);
        assert_eq!(slices[0].length, 512 * 1024);
        assert_eq!(slices[0].file_offset, FileOffset(512 * 1024));

        assert_eq!(slices[1].page, PageOrdinal(1));
        assert_eq!(slices[1].offset_in_page, 0);
        assert_eq!(slices[1].length, 1024 * 1024);
        assert_eq!(slices[1].file_offset, FileOffset(1024 * 1024));

        assert_eq!(slices[2].page, PageOrdinal(2));
        assert_eq!(slices[2].offset_in_page, 0);
        assert_eq!(slices[2].length, 512 * 1024);
        assert_eq!(slices[2].file_offset, FileOffset(2 * 1024 * 1024));

        let total_len: u64 = slices.iter().map(|s| s.length as u64).sum();
        assert_eq!(total_len, len);
    }
}
