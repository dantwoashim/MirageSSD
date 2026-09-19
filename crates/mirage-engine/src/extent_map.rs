//! Versioned byte-extent map for one inode: immutable base ranges plus
//! durable DIRTY and ZERO extents. A write records only the new bytes —
//! untouched base extents stay untouched, so an offline partial overwrite of
//! an uncached file never needs to fetch the base first. Reads pin a version
//! so writers never mutate a view in progress; truncate drops unreachable
//! extents and clips the last one, and later extension always reads zeros —
//! a shrunken file never exposes stale tail bytes.

use mirage_db::{ByteExtent, ExtentKind};
use mirage_types::{InodeId, MirageError, PageHash, RepositoryId};

/// One contiguous slice a reader must resolve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtentSlice {
    /// Bytes from the immutable base pack `page_hash` at `base_offset`.
    Base {
        start: u64,
        length: u64,
        page_hash: PageHash,
        base_offset: u64,
    },
    /// Locally written bytes in the journaled payload `payload_id` at
    /// `payload_offset` — the file holds exactly the written range.
    Dirty {
        start: u64,
        length: u64,
        payload_id: [u8; 16],
        payload_offset: u64,
    },
    /// Guaranteed zeros — never confused with unknown bytes.
    Zero { start: u64, length: u64 },
}

impl ExtentSlice {
    pub fn start(&self) -> u64 {
        match self {
            Self::Base { start, .. } | Self::Dirty { start, .. } | Self::Zero { start, .. } => {
                *start
            }
        }
    }
    pub fn length(&self) -> u64 {
        match self {
            Self::Base { length, .. } | Self::Dirty { length, .. } | Self::Zero { length, .. } => {
                *length
            }
        }
    }
}

#[derive(Debug, Clone)]
struct Interval {
    start: u64,
    length: u64,
    source: ExtentSource,
}

#[derive(Debug, Clone)]
enum ExtentSource {
    Base {
        page_hash: PageHash,
        base_offset: u64,
    },
    Dirty {
        payload_id: [u8; 16],
    },
    Zero,
}

/// Per-inode extent map. Writers serialize per inode through `Mutex`-like
/// external discipline; readers pin `version` for a stable view.
#[derive(Debug, Default)]
pub struct ExtentMap {
    intervals: Vec<Interval>,
    version: i64,
}

impl ExtentMap {
    /// Rebuilds a map from durable extents at `version`.
    pub fn replay(volume_id: RepositoryId, inode: InodeId, extents: &[ByteExtent]) -> Self {
        let _ = (volume_id, inode);
        let mut map = Self::default();
        for extent in extents {
            map.version = map.version.max(extent.version);
            map.intervals.push(Interval {
                start: extent.start,
                length: extent.length,
                source: match extent.kind {
                    ExtentKind::Base => ExtentSource::Base {
                        page_hash: extent.page_hash.unwrap_or(PageHash::from_bytes([0; 32])),
                        base_offset: extent.base_offset.unwrap_or(0),
                    },
                    ExtentKind::Dirty => ExtentSource::Dirty {
                        payload_id: extent.payload_id.unwrap_or([0; 16]),
                    },
                    ExtentKind::Zero => ExtentSource::Zero,
                },
            });
        }
        map.intervals.sort_by_key(|interval| interval.start);
        map
    }

    /// Current write version; each mutation increments it.
    #[must_use]
    pub fn version(&self) -> i64 {
        self.version
    }

    /// Seeds the map with a single base extent covering `[0, length)` so a
    /// partial write never needs to fetch untouched base bytes first.
    pub fn seed_base(&mut self, length: u64, base_identity: PageHash) {
        if length == 0 || !self.intervals.is_empty() {
            return;
        }
        self.intervals.push(Interval {
            start: 0,
            length,
            source: ExtentSource::Base {
                page_hash: base_identity,
                base_offset: 0,
            },
        });
    }

    /// Applies a dirty write of `length` bytes at `start`. Only the written
    /// range is replaced — disjoint base extents are preserved untouched.
    pub fn write(
        &mut self,
        start: u64,
        length: u64,
        payload_id: [u8; 16],
    ) -> Result<(), MirageError> {
        if length == 0 {
            return Ok(());
        }
        let end = start
            .checked_add(length)
            .ok_or_else(|| MirageError::invalid_argument("write range overflows"))?;
        self.splice(
            start,
            end,
            Interval {
                start,
                length,
                source: ExtentSource::Dirty { payload_id },
            },
        );
        self.version += 1;
        Ok(())
    }

    /// Truncates at `length`: extents beyond the end are dropped, the last
    /// extent is clipped, and later extension reads zeros.
    pub fn truncate(&mut self, length: u64) -> Result<(), MirageError> {
        let mut kept = Vec::new();
        for mut interval in std::mem::take(&mut self.intervals) {
            if interval.start >= length {
                continue;
            }
            if interval.start + interval.length > length {
                interval.length = length - interval.start;
            }
            kept.push(interval);
        }
        self.intervals = kept;
        self.version += 1;
        Ok(())
    }

    /// Reads `length` bytes at `start` for the pinned version, resolving to
    /// base/dirty/zero slices. Bytes past the end-of-file extent set are
    /// zeros; bytes inside the set that no extent covers are zeros too —
    /// holes are only created by explicit truncate/regrow or sparse writes
    /// that recorded a ZERO extent, so an unavailable base slice fails at
    /// fetch time, never silently.
    pub fn read(&self, start: u64, length: u64) -> Result<Vec<ExtentSlice>, MirageError> {
        if length == 0 {
            return Ok(Vec::new());
        }
        let end = start
            .checked_add(length)
            .ok_or_else(|| MirageError::invalid_argument("read range overflows"))?;
        let mut slices = Vec::new();
        let mut cursor = start;
        for interval in &self.intervals {
            if interval.start + interval.length <= cursor {
                continue;
            }
            if interval.start > cursor {
                slices.push(ExtentSlice::Zero {
                    start: cursor,
                    length: interval.start.min(end) - cursor,
                });
            }
            if interval.start >= end {
                break;
            }
            let overlap_start = cursor.max(interval.start);
            let overlap_end = end.min(interval.start + interval.length);
            if overlap_end <= overlap_start {
                continue;
            }
            let inside = overlap_start - interval.start;
            slices.push(match &interval.source {
                ExtentSource::Base {
                    page_hash,
                    base_offset,
                } => ExtentSlice::Base {
                    start: overlap_start,
                    length: overlap_end - overlap_start,
                    page_hash: *page_hash,
                    base_offset: base_offset + inside,
                },
                ExtentSource::Dirty { payload_id } => ExtentSlice::Dirty {
                    start: overlap_start,
                    length: overlap_end - overlap_start,
                    payload_id: *payload_id,
                    payload_offset: inside,
                },
                ExtentSource::Zero => ExtentSlice::Zero {
                    start: overlap_start,
                    length: overlap_end - overlap_start,
                },
            });
            cursor = overlap_end;
            if cursor >= end {
                break;
            }
        }
        if cursor < end {
            slices.push(ExtentSlice::Zero {
                start: cursor,
                length: end - cursor,
            });
        }
        Ok(slices)
    }

    /// Materializes the map as durable extents at `version` for the writer
    /// actor.
    pub fn to_extents(
        &self,
        volume_id: RepositoryId,
        inode: InodeId,
        version: i64,
        now_ns: i64,
        mut next_id: impl FnMut() -> [u8; 16],
    ) -> Vec<ByteExtent> {
        self.intervals
            .iter()
            .map(|interval| {
                let (kind, page_hash, base_offset, payload_id) = match &interval.source {
                    ExtentSource::Base {
                        page_hash,
                        base_offset,
                    } => (ExtentKind::Base, Some(*page_hash), Some(*base_offset), None),
                    ExtentSource::Dirty { payload_id } => {
                        (ExtentKind::Dirty, None, None, Some(*payload_id))
                    }
                    ExtentSource::Zero => (ExtentKind::Zero, None, None, None),
                };
                ByteExtent {
                    extent_id: next_id(),
                    volume_id,
                    inode,
                    version,
                    start: interval.start,
                    length: interval.length,
                    kind,
                    page_hash,
                    base_offset,
                    payload_id,
                    created_ns: now_ns,
                }
            })
            .collect()
    }

    /// True when the map is a single base extent — equivalent to the
    /// committed immutable content, so callers can take the plain page path.
    #[must_use]
    pub fn is_plain_base(&self) -> bool {
        matches!(
            self.intervals.as_slice(),
            [Interval {
                source: ExtentSource::Base { .. },
                ..
            }]
        ) || self.intervals.is_empty()
    }

    /// Splice `[start, end)` out and insert `inserted`, splitting covered
    /// intervals so disjoint writes to one page both survive.
    fn splice(&mut self, start: u64, end: u64, inserted: Interval) {
        let mut out = Vec::with_capacity(self.intervals.len() + 2);
        for interval in std::mem::take(&mut self.intervals) {
            let i_end = interval.start + interval.length;
            if i_end <= start || interval.start >= end {
                out.push(interval);
                continue;
            }
            // Left remainder.
            if interval.start < start {
                out.push(Interval {
                    start: interval.start,
                    length: start - interval.start,
                    source: interval.source.clone(),
                });
            }
            // Right remainder — a base extent keeps the same page hash at an
            // advanced offset.
            if i_end > end {
                let tail = interval.source.clone();
                out.push(Interval {
                    start: end,
                    length: i_end - end,
                    source: match tail {
                        ExtentSource::Base {
                            page_hash,
                            base_offset,
                        } => ExtentSource::Base {
                            page_hash,
                            base_offset: base_offset + (end - interval.start),
                        },
                        other => other,
                    },
                });
            }
        }
        out.push(inserted);
        out.sort_by_key(|interval| interval.start);
        self.intervals = out;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_map() -> ExtentMap {
        let mut map = ExtentMap::default();
        map.intervals.push(Interval {
            start: 0,
            length: 8192,
            source: ExtentSource::Base {
                page_hash: PageHash::from_bytes([0xaa; 32]),
                base_offset: 0,
            },
        });
        map
    }

    #[test]
    fn disjoint_partial_writes_both_survive() {
        let mut map = base_map();
        map.write(100, 4, [1; 16]).unwrap();
        map.write(5000, 4, [2; 16]).unwrap();
        let slices = map.read(0, 8192).unwrap();
        let dirty: Vec<u64> = slices
            .iter()
            .filter_map(|slice| match slice {
                ExtentSlice::Dirty { start, .. } => Some(*start),
                _ => None,
            })
            .collect();
        assert_eq!(dirty, vec![100, 5000]);
        let base_total: u64 = slices
            .iter()
            .filter_map(|slice| match slice {
                ExtentSlice::Base { length, .. } => Some(*length),
                _ => None,
            })
            .sum();
        assert_eq!(base_total, 8192 - 8);
    }

    #[test]
    fn shrink_then_grow_never_exposes_old_tail() {
        let mut map = base_map();
        map.truncate(100).unwrap();
        let slices = map.read(0, 8192).unwrap();
        let tail = slices.last().unwrap();
        assert_eq!(
            *tail,
            ExtentSlice::Zero {
                start: 100,
                length: 8192 - 100
            }
        );
    }

    #[test]
    fn partial_overwrite_splits_base_extent_offsets() {
        let mut map = base_map();
        map.write(4096, 100, [7; 16]).unwrap();
        let slices = map.read(4000, 300).unwrap();
        // 96 bytes of base (page offset 4000), 100 dirty, 104 base at 4196.
        assert_eq!(slices.len(), 3);
        match &slices[2] {
            ExtentSlice::Base {
                start, base_offset, ..
            } => {
                assert_eq!(*start, 4196);
                assert_eq!(*base_offset, 4196);
            }
            _ => panic!("expected trailing base slice"),
        }
    }
}
