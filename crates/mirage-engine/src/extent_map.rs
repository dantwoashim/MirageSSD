//! Versioned byte-extent map for one inode: immutable base ranges plus
//! durable DIRTY and ZERO extents. A write records only the new bytes —
//! untouched base extents stay untouched, so an offline partial overwrite of
//! an uncached file never needs to fetch the base first. Reads pin a version
//! so writers never mutate a view in progress; truncate drops unreachable
//! extents and clips the last one, and later extension always reads zeros —
//! a shrunken file never exposes stale tail bytes.
//!
//! The logical EOF is tracked independently of the interval list: a
//! truncate-to-zero still produces a versioned head (durable via
//! `byte_extent_heads`), and a truncate-grow leaves a hole that reads as
//! zeros without materializing zero extents.

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
    /// `payload_offset` is where this interval's bytes begin inside the
    /// staged payload — a clip that drops the head must advance it so the
    /// surviving tail never re-reads payload byte zero.
    Dirty {
        payload_id: [u8; 16],
        payload_offset: u64,
    },
    Zero,
}

/// Per-inode extent map. Writers serialize per inode through `Mutex`-like
/// external discipline; readers pin `version` for a stable view.
#[derive(Debug, Default, Clone)]
pub struct ExtentMap {
    intervals: Vec<Interval>,
    version: i64,
    /// Logical EOF: tracked separately from the interval list so a
    /// truncate-grow (zero hole) and a truncate-to-zero (no extents) both
    /// report honest end-of-file.
    file_size: u64,
    /// The map was rebuilt from durable history — it must never take the
    /// unchanged-base fast path even when the decode leaves no intervals.
    historical: bool,
}

impl ExtentMap {
    /// Rebuilds a map from durable extents at `version`. The file size is the
    /// last extent's end; callers that have the durable head should apply
    /// `set_file_size` so a trailing hole or empty file reports its true EOF.
    pub fn replay(volume_id: RepositoryId, inode: InodeId, extents: &[ByteExtent]) -> Self {
        let _ = (volume_id, inode);
        let mut map = Self::default();
        for extent in extents {
            map.version = map.version.max(extent.version);
            map.file_size = map
                .file_size
                .max(extent.start.saturating_add(extent.length));
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
                        // Pre-migration dirty rows carry no payload offset;
                        // zero is correct only for rows written before the
                        // tail-corruption fix and never for a clipped tail.
                        payload_offset: extent.payload_offset.unwrap_or(0),
                    },
                    ExtentKind::Zero => ExtentSource::Zero,
                },
            });
        }
        map.intervals.sort_by_key(|interval| interval.start);
        map.historical = true;
        map
    }

    /// Current write version; each mutation increments it.
    #[must_use]
    pub fn version(&self) -> i64 {
        self.version
    }

    /// The current logical EOF.
    #[must_use]
    pub fn file_size(&self) -> u64 {
        self.file_size
    }

    /// Applies the durable head — an empty or hole-tailed extent set cannot
    /// express its own newest version or EOF.
    pub fn set_head(&mut self, version: i64, eof: u64) {
        self.version = self.version.max(version);
        self.file_size = eof;
        self.historical = true;
    }

    /// A map is plain base only while it carries no mutation at all: exactly
    /// one unwritten base extent covering the whole file, or nothing. Any
    /// write, truncate, or replayed history disqualifies it, so a truncated
    /// base can never be mistaken for the unchanged file by the fast path.
    #[must_use]
    pub fn is_plain_base(&self) -> bool {
        if self.version != 0 || self.historical {
            return false;
        }
        match self.intervals.as_slice() {
            [] => true,
            [interval] => {
                interval.start == 0
                    && interval.length == self.file_size
                    && matches!(interval.source, ExtentSource::Base { base_offset: 0, .. })
            }
            _ => false,
        }
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
        self.file_size = self.file_size.max(length);
    }

    /// Applies a dirty write of `length` bytes at `start`. Only the written
    /// range is replaced — disjoint base extents are preserved untouched —
    /// and the EOF extends when the write reaches past it.
    pub fn write(
        &mut self,
        start: u64,
        length: u64,
        payload_id: [u8; 16],
    ) -> Result<(), MirageError> {
        self.write_at(start, length, payload_id, 0)
    }

    /// Like `write`, but the interval points at `payload_offset` inside the
    /// payload — used when several logical ranges share one staged payload.
    /// Contiguous writes into the same payload coalesce with the existing
    /// interval, so a sequential copy stays a single extent instead of one
    /// row per write chunk.
    pub fn write_at(
        &mut self,
        start: u64,
        length: u64,
        payload_id: [u8; 16],
        payload_offset: u64,
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
                source: ExtentSource::Dirty {
                    payload_id,
                    payload_offset,
                },
            },
        );
        self.coalesce();
        self.file_size = self.file_size.max(end);
        self.version += 1;
        Ok(())
    }

    /// Merges every adjacent pair of dirty intervals that describe one
    /// contiguous run inside the same payload (logical contiguity AND
    /// payload-offset contiguity). Runs until no pair merges.
    fn coalesce(&mut self) {
        loop {
            let mut merged = false;
            for idx in 0..self.intervals.len().saturating_sub(1) {
                let (left, right) = (self.intervals[idx].clone(), self.intervals[idx + 1].clone());
                if let (
                    ExtentSource::Dirty {
                        payload_id: left_id,
                        payload_offset: left_off,
                    },
                    ExtentSource::Dirty {
                        payload_id: right_id,
                        payload_offset: right_off,
                    },
                ) = (left.source, right.source)
                    && left_id == right_id
                    && left.start + left.length == right.start
                    && left_off + left.length == right_off
                {
                    self.intervals[idx].length += right.length;
                    self.intervals.remove(idx + 1);
                    merged = true;
                    break;
                }
            }
            if !merged {
                break;
            }
        }
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
        self.file_size = length;
        self.version += 1;
        Ok(())
    }

    /// Reads `length` bytes at `start` for the pinned version, resolving to
    /// base/dirty/zero slices. Bytes past the EOF are not emitted — a read
    /// reaching past the end returns short, never a fabricated tail. Bytes
    /// inside the EOF that no extent covers are zeros — holes are only
    /// created by explicit truncate/regrow or sparse writes — so an
    /// unavailable base slice fails at fetch time, never silently.
    pub fn read(&self, start: u64, length: u64) -> Result<Vec<ExtentSlice>, MirageError> {
        if length == 0 || start >= self.file_size {
            return Ok(Vec::new());
        }
        let end = start
            .checked_add(length)
            .ok_or_else(|| MirageError::invalid_argument("read range overflows"))?
            .min(self.file_size);
        let mut slices = Vec::new();
        let mut cursor = start;
        for interval in &self.intervals {
            if interval.start + interval.length <= cursor {
                continue;
            }
            if interval.start > cursor {
                // Uncovered gap: emit zeros once and advance the cursor so a
                // gap before the first extent is never emitted twice.
                let gap_end = interval.start.min(end);
                slices.push(ExtentSlice::Zero {
                    start: cursor,
                    length: gap_end - cursor,
                });
                cursor = gap_end;
            }
            if interval.start >= end || cursor >= end {
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
                ExtentSource::Dirty {
                    payload_id,
                    payload_offset,
                } => ExtentSlice::Dirty {
                    start: overlap_start,
                    length: overlap_end - overlap_start,
                    payload_id: *payload_id,
                    payload_offset: payload_offset + inside,
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

    /// Serializes the map as a version's extent set — written atomically
    /// with the operation that produced it. Uncovered holes, including the
    /// trailing hole of a truncate-grow, are materialized as Zero extents so
    /// the durable set alone reproduces the file size and read semantics.
    pub fn to_extents(
        &self,
        volume_id: RepositoryId,
        inode: InodeId,
        version: i64,
        now_ns: i64,
        mut next_extent_id: impl FnMut() -> [u8; 16],
    ) -> Vec<ByteExtent> {
        let mut intervals: Vec<Interval> = Vec::with_capacity(self.intervals.len() + 1);
        let mut cursor = 0u64;
        for interval in &self.intervals {
            if interval.start > cursor {
                intervals.push(Interval {
                    start: cursor,
                    length: interval.start - cursor,
                    source: ExtentSource::Zero,
                });
            }
            intervals.push(interval.clone());
            cursor = cursor.max(interval.start + interval.length);
        }
        if cursor < self.file_size {
            intervals.push(Interval {
                start: cursor,
                length: self.file_size - cursor,
                source: ExtentSource::Zero,
            });
        }
        intervals
            .iter()
            .map(|interval| {
                let (kind, page_hash, base_offset, payload_id, payload_offset) =
                    match &interval.source {
                        ExtentSource::Base {
                            page_hash,
                            base_offset,
                        } => (
                            ExtentKind::Base,
                            Some(*page_hash),
                            Some(*base_offset),
                            None,
                            None,
                        ),
                        ExtentSource::Dirty {
                            payload_id,
                            payload_offset,
                        } => (
                            ExtentKind::Dirty,
                            None,
                            None,
                            Some(*payload_id),
                            Some(*payload_offset),
                        ),
                        ExtentSource::Zero => (ExtentKind::Zero, None, None, None, None),
                    };
                ByteExtent {
                    extent_id: next_extent_id(),
                    volume_id,
                    inode,
                    version,
                    start: interval.start,
                    length: interval.length,
                    kind,
                    page_hash,
                    base_offset,
                    payload_id,
                    payload_offset,
                    created_ns: now_ns,
                }
            })
            .collect()
    }

    /// Replaces `[start, end)` with `interval`, splitting overlapped
    /// intervals and preserving each survivor's offset into its own source —
    /// a clipped dirty tail keeps pointing at its payload position.
    fn splice(&mut self, start: u64, end: u64, interval: Interval) {
        let mut next = Vec::with_capacity(self.intervals.len() + 2);
        for existing in std::mem::take(&mut self.intervals) {
            let existing_end = existing.start + existing.length;
            if existing_end <= start || existing.start >= end {
                next.push(existing);
                continue;
            }
            if existing.start < start {
                next.push(Interval {
                    start: existing.start,
                    length: start - existing.start,
                    source: existing.source.clone(),
                });
            }
            if existing_end > end {
                let clipped = end - existing.start;
                let tail_source = match existing.source {
                    ExtentSource::Base {
                        page_hash,
                        base_offset,
                    } => ExtentSource::Base {
                        page_hash,
                        base_offset: base_offset + clipped,
                    },
                    ExtentSource::Dirty {
                        payload_id,
                        payload_offset,
                    } => ExtentSource::Dirty {
                        payload_id,
                        payload_offset: payload_offset + clipped,
                    },
                    ExtentSource::Zero => ExtentSource::Zero,
                };
                next.push(Interval {
                    start: end,
                    length: existing_end - end,
                    source: tail_source,
                });
            }
        }
        next.push(interval);
        next.sort_by_key(|entry| entry.start);
        self.intervals = next;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page() -> PageHash {
        PageHash::from_bytes([3; 32])
    }

    #[test]
    fn disjoint_writes_produce_disjoint_dirty_slices() {
        let mut map = ExtentMap::default();
        map.seed_base(100, page());
        map.write(10, 4, [1; 16]).unwrap();
        map.write(50, 4, [2; 16]).unwrap();
        let slices = map.read(0, 100).unwrap();
        let dirty: Vec<_> = slices
            .iter()
            .filter(|slice| matches!(slice, ExtentSlice::Dirty { .. }))
            .collect();
        assert_eq!(dirty.len(), 2);
    }

    #[test]
    fn overlapping_write_splits_and_offsets_tail() {
        let mut map = ExtentMap::default();
        map.write(0, 10, [1; 16]).unwrap();
        map.write(5, 4, [2; 16]).unwrap();
        let slices = map.read(0, 10).unwrap();
        let dirty: Vec<_> = slices
            .iter()
            .filter(|s| matches!(s, ExtentSlice::Dirty { .. }))
            .collect();
        assert_eq!(dirty.len(), 3);
        match dirty[2] {
            ExtentSlice::Dirty {
                start,
                length,
                payload_id,
                payload_offset,
            } => {
                assert_eq!((*start, *length), (9, 1));
                assert_eq!(*payload_id, [1; 16]);
                assert_eq!(*payload_offset, 9);
            }
            _ => panic!("expected dirty tail"),
        }
    }

    #[test]
    fn truncate_shrinks_and_grows_with_zero_fill() {
        let mut map = ExtentMap::default();
        map.seed_base(100, page());
        map.truncate(10).unwrap();
        assert_eq!(map.file_size(), 10);
        assert!(!map.is_plain_base());
        let slices = map.read(0, 100).unwrap();
        let covered: u64 = slices.iter().map(ExtentSlice::length).sum();
        assert_eq!(covered, 10);
        assert!(map.read(10, 4).unwrap().is_empty());
        map.truncate(100).unwrap();
        assert_eq!(map.file_size(), 100);
        let tail = map.read(90, 10).unwrap();
        assert_eq!(
            tail,
            vec![ExtentSlice::Zero {
                start: 90,
                length: 10
            }]
        );
    }

    #[test]
    fn truncate_to_zero_survives_replay() {
        let mut map = ExtentMap::default();
        map.write(0, 4, [1; 16]).unwrap();
        map.truncate(0).unwrap();
        assert!(map.read(0, 4).unwrap().is_empty());
        let replayed = ExtentMap::replay(
            RepositoryId::from_bytes([7; 16]),
            InodeId::from_bytes([9; 16]),
            &map.to_extents(
                RepositoryId::from_bytes([7; 16]),
                InodeId::from_bytes([9; 16]),
                2,
                1,
                || [2; 16],
            ),
        );
        assert_eq!(replayed.version(), 0);
        assert!(replayed.read(0, 4).unwrap().is_empty());
    }

    #[test]
    fn sparse_read_gaps_emit_single_zero_slice() {
        let mut map = ExtentMap::default();
        map.write(100, 4, [1; 16]).unwrap();
        let slices = map.read(0, 10).unwrap();
        assert_eq!(
            slices,
            vec![ExtentSlice::Zero {
                start: 0,
                length: 10
            }]
        );
    }

    #[test]
    fn base_slice_tracks_base_offset() {
        let mut map = ExtentMap::default();
        map.seed_base(100, page());
        map.write(0, 10, [1; 16]).unwrap();
        let slices = map.read(10, 10).unwrap();
        match &slices[0] {
            ExtentSlice::Base { base_offset, .. } => assert_eq!(*base_offset, 10),
            other => panic!("expected base slice, got {other:?}"),
        }
    }
}

#[cfg(test)]
mod coalesce_tests {
    use super::*;
    use mirage_types::{InodeId, RepositoryId};

    fn page() -> PageHash {
        PageHash::from_bytes([3; 32])
    }

    #[test]
    fn contiguous_same_payload_writes_collapse_to_one_interval() {
        let mut map = ExtentMap::default();
        map.seed_base(100, page());
        let payload = [9; 16];
        map.write_at(0, 64, payload, 0).unwrap();
        map.write_at(64, 64, payload, 64).unwrap();
        map.write_at(128, 32, payload, 128).unwrap();
        let dirty: Vec<_> = map
            .read(0, 160)
            .unwrap()
            .into_iter()
            .filter(|s| matches!(s, ExtentSlice::Dirty { .. }))
            .collect();
        assert_eq!(dirty.len(), 1);
        match dirty[0] {
            ExtentSlice::Dirty {
                start,
                length,
                payload_offset,
                ..
            } => {
                assert_eq!((start, length, payload_offset), (0, 160, 0));
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn different_payload_or_offset_gap_stays_split() {
        let mut map = ExtentMap::default();
        map.seed_base(100, page());
        map.write_at(0, 32, [1; 16], 0).unwrap();
        map.write_at(32, 32, [2; 16], 0).unwrap();
        map.write_at(64, 32, [1; 16], 96).unwrap(); // same payload, wrong offset
        let dirty: Vec<_> = map
            .read(0, 96)
            .unwrap()
            .into_iter()
            .filter(|s| matches!(s, ExtentSlice::Dirty { .. }))
            .collect();
        assert_eq!(dirty.len(), 3);
    }

    #[test]
    fn coalesced_map_reads_and_serializes_identically() {
        let mut merged = ExtentMap::default();
        merged.seed_base(200, page());
        merged.write_at(10, 20, [7; 16], 0).unwrap();
        merged.write_at(30, 20, [7; 16], 20).unwrap();
        let mut unmerged = ExtentMap::default();
        unmerged.seed_base(200, page());
        unmerged.write_at(10, 40, [7; 16], 0).unwrap();
        assert_eq!(merged.read(0, 200).unwrap(), unmerged.read(0, 200).unwrap());
        let mut next_id = || [9; 16];
        let a = merged.to_extents(
            RepositoryId::from_bytes([1; 16]),
            InodeId::from_bytes([9; 16]),
            1,
            0,
            &mut next_id,
        );
        let b = unmerged.to_extents(
            RepositoryId::from_bytes([1; 16]),
            InodeId::from_bytes([9; 16]),
            1,
            0,
            &mut next_id,
        );
        assert_eq!(a.len(), b.len());
        for (x, y) in a.iter().zip(b.iter()) {
            assert_eq!(
                (x.start, x.length, x.payload_id, x.payload_offset),
                (y.start, y.length, y.payload_id, y.payload_offset)
            );
        }
        assert_eq!(merged.file_size(), unmerged.file_size());
    }
}
