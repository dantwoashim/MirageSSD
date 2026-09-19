use std::fs::File;
use std::path::{Path, PathBuf};

use mirage_types::MirageError;

use crate::arena::{
    AlignedBuf, open_restrictive, open_unbuffered_read, read_exact_at, write_exact_at,
};
use crate::format::{
    ARENA_HEADER_BYTES, ArenaHeader, CacheLayout, SLOT_METADATA_BYTES, SlotMetadata, SlotState,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SparseDiagnostics {
    pub logical_bytes: u64,
    pub allocated_bytes: u64,
}

pub struct ArenaShard {
    path: PathBuf,
    file: File,
    metadata: File,
    layout: CacheLayout,
    unbuffered: bool,
}

impl ArenaShard {
    pub fn create(path: &Path, layout: CacheLayout) -> Result<Self, MirageError> {
        layout.validate()?;
        let file = open_restrictive(path, true).map_err(MirageError::from)?;
        set_sparse(&file)?;
        file.set_len(layout.logical_shard_bytes()?)
            .map_err(MirageError::from)?;
        let header = ArenaHeader {
            page_size: layout.page_size,
            slot_count: layout.slot_count,
        }
        .encode()?;
        write_exact_at(&file, &header, 0).map_err(MirageError::from)?;
        file.sync_all().map_err(MirageError::from)?;
        let metadata_path = metadata_path(path);
        let metadata = open_restrictive(&metadata_path, true).map_err(MirageError::from)?;
        metadata
            .set_len(metadata_length(layout)?)
            .map_err(MirageError::from)?;
        write_exact_at(&metadata, &header, 0).map_err(MirageError::from)?;
        let free = SlotMetadata {
            generation: 0,
            state: SlotState::Free,
            page_hash: mirage_types::PageHash::from_bytes([0; 32]),
            logical_length: 0,
        }
        .encode();
        for slot in 0..layout.slot_count {
            write_exact_at(&metadata, &free, metadata_offset(slot)?).map_err(MirageError::from)?;
        }
        metadata.sync_all().map_err(MirageError::from)?;
        Ok(Self {
            path: path.to_path_buf(),
            file,
            metadata,
            layout,
            unbuffered: false,
        })
    }
    pub fn open(path: &Path, expected: CacheLayout) -> Result<Self, MirageError> {
        Self::open_inner(path, expected, false)
    }
    /// Read-only handle with `FILE_FLAG_NO_BUFFERING` (Windows). For the WinFsp host:
    /// mounted volume data is already kernel-cached, so buffering the arena file here
    /// would double-cache every hot page. Requires a 4096-multiple page size.
    pub fn open_read_unbuffered(path: &Path, expected: CacheLayout) -> Result<Self, MirageError> {
        Self::open_inner(path, expected, true)
    }
    fn open_inner(
        path: &Path,
        expected: CacheLayout,
        unbuffered: bool,
    ) -> Result<Self, MirageError> {
        expected.validate()?;
        if unbuffered && !expected.page_size.as_u64().is_multiple_of(4096) {
            return Err(MirageError::unsupported_layout(
                "unbuffered cache reads require a 4096-multiple page size",
            ));
        }
        let file = open_restrictive(path, false).map_err(MirageError::from)?;
        let mut bytes = [0_u8; ARENA_HEADER_BYTES];
        read_exact_at(&file, &mut bytes, 0).map_err(MirageError::from)?;
        let header = ArenaHeader::decode(&bytes)?;
        if header.page_size != expected.page_size
            || header.slot_count != expected.slot_count
            || file.metadata().map_err(MirageError::from)?.len()
                != expected.logical_shard_bytes()?
        {
            return Err(MirageError::unsupported_layout(
                "cache shard does not match expected layout",
            ));
        }
        let metadata = open_restrictive(&metadata_path(path), false).map_err(MirageError::from)?;
        let mut metadata_header = [0_u8; ARENA_HEADER_BYTES];
        read_exact_at(&metadata, &mut metadata_header, 0).map_err(MirageError::from)?;
        if ArenaHeader::decode(&metadata_header)? != header
            || metadata.metadata().map_err(MirageError::from)?.len() != metadata_length(expected)?
        {
            return Err(MirageError::unsupported_layout(
                "cache metadata sidecar does not match arena",
            ));
        }
        // The header/metadata checks above used a buffered handle; swap in the
        // unbuffered data handle only for the returned shard.
        let file = if unbuffered {
            open_unbuffered_read(path).map_err(MirageError::from)?
        } else {
            file
        };
        Ok(Self {
            path: path.to_path_buf(),
            file,
            metadata,
            layout: expected,
            unbuffered,
        })
    }
    #[must_use]
    pub const fn layout(&self) -> CacheLayout {
        self.layout
    }
    pub fn slot_offset(&self, slot: u32) -> Result<u64, MirageError> {
        self.layout.slot_offset(slot)
    }
    pub fn diagnostics(&self) -> Result<SparseDiagnostics, MirageError> {
        Ok(SparseDiagnostics {
            logical_bytes: self.file.metadata().map_err(MirageError::from)?.len(),
            allocated_bytes: allocated_bytes(&self.path)?
                .checked_add(allocated_bytes(&metadata_path(&self.path))?)
                .ok_or_else(|| {
                    MirageError::invalid_argument("cache allocation accounting overflows")
                })?,
        })
    }
    pub fn write_slot(&self, slot: u32, bytes: &[u8]) -> Result<(), MirageError> {
        if bytes.is_empty() || bytes.len() as u64 > self.layout.page_size.as_u64() {
            return Err(MirageError::invalid_argument(
                "cache slot payload length is invalid",
            ));
        }
        write_exact_at(&self.file, bytes, self.slot_offset(slot)?).map_err(MirageError::from)
    }
    /// Writes `bytes` across `slot_count` consecutive slots starting at
    /// `first_slot` in one I/O. Every slot but the last must be filled to
    /// `page_size`, so the payload for slot `i` lands exactly where
    /// `read_slot(first_slot + i, ..)` expects it. One write over a sparse hole
    /// lets the filesystem allocate the run as one extent instead of one per
    /// page, which is what keeps coalesced reads physically contiguous.
    pub fn write_slots_contiguous(
        &self,
        first_slot: u32,
        slot_count: u32,
        bytes: &[u8],
    ) -> Result<(), MirageError> {
        let page_size = self.layout.page_size.as_u64();
        if slot_count == 0 || bytes.is_empty() {
            return Err(MirageError::invalid_argument(
                "contiguous write covers zero slots or bytes",
            ));
        }
        let last_slot = first_slot
            .checked_add(slot_count - 1)
            .ok_or_else(|| MirageError::invalid_argument("contiguous write slot overflows"))?;
        self.slot_offset(last_slot)?;
        let full_prefix = u64::from(slot_count - 1)
            .checked_mul(page_size)
            .ok_or_else(|| MirageError::invalid_argument("contiguous write window overflows"))?;
        let len = bytes.len() as u64;
        if len <= full_prefix || len > full_prefix + page_size {
            return Err(MirageError::invalid_argument(
                "contiguous write payload does not fill every slot but the last",
            ));
        }
        write_exact_at(&self.file, bytes, self.slot_offset(first_slot)?).map_err(MirageError::from)
    }
    pub fn read_slot(
        &self,
        slot: u32,
        logical_length: u32,
        offset: u32,
        output: &mut [u8],
    ) -> Result<(), MirageError> {
        let end = u64::from(offset)
            .checked_add(output.len() as u64)
            .ok_or_else(|| MirageError::invalid_argument("cache read range overflows"))?;
        if end > u64::from(logical_length)
            || u64::from(logical_length) > self.layout.page_size.as_u64()
        {
            return Err(MirageError::invalid_argument(
                "cache read exceeds resident logical length",
            ));
        }
        if self.unbuffered {
            return self.read_slot_unbuffered(slot, self.layout.page_size.as_u64(), offset, output);
        }
        read_exact_at(
            &self.file,
            output,
            self.slot_offset(slot)?
                .checked_add(u64::from(offset))
                .ok_or_else(|| MirageError::invalid_argument("cache read offset overflows"))?,
        )
        .map_err(MirageError::from)
    }
    /// `FILE_FLAG_NO_BUFFERING` read path: the requested window is rounded out to
    /// 4096 inside the slot window (the slot base and page size keep file offsets
    /// aligned; the aligned end is clamped to `window_bytes`, never past the
    /// arena's length), read into an aligned scratch, then copied to the caller's
    /// window. A single-slot read passes `page_size` as `window_bytes`; a
    /// coalesced read passes `slot_count * page_size`.
    fn read_slot_unbuffered(
        &self,
        slot: u32,
        window_bytes: u64,
        offset: u32,
        output: &mut [u8],
    ) -> Result<(), MirageError> {
        const ALIGN: u64 = 4096;
        let slot_base = self.slot_offset(slot)?;
        if output.is_empty() {
            return Ok(());
        }
        if u64::from(offset).is_multiple_of(ALIGN)
            && output.len().is_multiple_of(ALIGN as usize)
            && (output.as_ptr() as usize).is_multiple_of(ALIGN as usize)
        {
            return read_exact_at(&self.file, output, slot_base + u64::from(offset))
                .map_err(MirageError::from);
        }
        let start = u64::from(offset) & !(ALIGN - 1);
        let want_end = u64::from(offset) + output.len() as u64;
        let end = ((want_end + ALIGN - 1) & !(ALIGN - 1)).min(window_bytes);
        let window = usize::try_from(end - start)
            .map_err(|_| MirageError::internal_invariant("unaligned read window overflows"))?;
        thread_local! {
            static SCRATCH: std::cell::RefCell<Option<AlignedBuf>> = const { std::cell::RefCell::new(None) };
        }
        SCRATCH.with_borrow_mut(|buffer| {
            if buffer.as_ref().is_none_or(|buffer| buffer.len() < window) {
                *buffer = Some(AlignedBuf::new(window).map_err(MirageError::from)?);
            }
            let scratch = buffer
                .as_mut()
                .expect("aligned scratch allocated")
                .as_mut_slice();
            read_exact_at(&self.file, &mut scratch[..window], slot_base + start)
                .map_err(MirageError::from)?;
            let rel = (u64::from(offset) - start) as usize;
            output.copy_from_slice(&scratch[rel..rel + output.len()]);
            Ok(())
        })
    }
    /// Reads a window spanning `slot_count` consecutive slots in one I/O.
    /// `offset` is measured from the first slot's base and `output` must fit
    /// inside the slot window. The caller must hold a live lease on every
    /// contributing slot for the whole call — the shard itself only performs
    /// the read.
    pub fn read_slots_contiguous(
        &self,
        first_slot: u32,
        slot_count: u32,
        offset: u32,
        output: &mut [u8],
    ) -> Result<(), MirageError> {
        if slot_count == 0 {
            return Err(MirageError::invalid_argument(
                "contiguous read covers zero slots",
            ));
        }
        let last_slot = first_slot
            .checked_add(slot_count - 1)
            .ok_or_else(|| MirageError::invalid_argument("contiguous read slot overflows"))?;
        self.slot_offset(last_slot)?;
        let window_bytes = u64::from(slot_count)
            .checked_mul(self.layout.page_size.as_u64())
            .ok_or_else(|| MirageError::invalid_argument("contiguous read window overflows"))?;
        let end = u64::from(offset)
            .checked_add(output.len() as u64)
            .ok_or_else(|| MirageError::invalid_argument("contiguous read range overflows"))?;
        if end > window_bytes {
            return Err(MirageError::invalid_argument(
                "contiguous read exceeds the slot window",
            ));
        }
        if self.unbuffered {
            return self.read_slot_unbuffered(first_slot, window_bytes, offset, output);
        }
        read_exact_at(
            &self.file,
            output,
            self.slot_offset(first_slot)?
                .checked_add(u64::from(offset))
                .ok_or_else(|| MirageError::invalid_argument("contiguous read offset overflows"))?,
        )
        .map_err(MirageError::from)
    }
    pub fn flush(&self) -> Result<(), MirageError> {
        self.file.sync_data().map_err(MirageError::from)
    }
    pub fn write_metadata(&self, slot: u32, value: SlotMetadata) -> Result<(), MirageError> {
        self.slot_offset(slot)?;
        write_exact_at(&self.metadata, &value.encode(), metadata_offset(slot)?)
            .map_err(MirageError::from)
    }
    pub fn read_metadata(&self, slot: u32) -> Result<SlotMetadata, MirageError> {
        self.slot_offset(slot)?;
        let mut bytes = [0_u8; SLOT_METADATA_BYTES];
        read_exact_at(&self.metadata, &mut bytes, metadata_offset(slot)?)
            .map_err(MirageError::from)?;
        SlotMetadata::decode(&bytes)
    }
    pub fn flush_metadata(&self) -> Result<(), MirageError> {
        self.metadata.sync_data().map_err(MirageError::from)
    }
    pub fn deallocate_slot(&self, slot: u32) -> Result<(), MirageError> {
        deallocate(
            &self.file,
            self.slot_offset(slot)?,
            self.layout.page_size.as_u64(),
        )
    }
    pub fn reclaimable_slot_bytes(&self, slot: u32) -> Result<u64, MirageError> {
        // NTFS sparse allocation/deallocation is tracked in 64 KiB compression units. The
        // 4 KiB arena header makes slot boundaries unaligned, so exclude both partial boundary
        // units rather than promising bytes FSCTL_SET_ZERO_DATA may leave allocated.
        const SPARSE_UNIT: u64 = 64 * 1024;
        let slot_start = self.slot_offset(slot)?;
        let slot_end = slot_start
            .checked_add(self.layout.page_size.as_u64())
            .ok_or_else(|| MirageError::invalid_argument("cache slot end overflows"))?;
        let reclaim_start = slot_start
            .checked_add(SPARSE_UNIT - 1)
            .ok_or_else(|| MirageError::invalid_argument("cache slot alignment overflows"))?
            & !(SPARSE_UNIT - 1);
        let reclaim_end = slot_end & !(SPARSE_UNIT - 1);
        if reclaim_end <= reclaim_start {
            return Ok(0);
        }
        allocated_bytes_in_range(&self.file, reclaim_start, reclaim_end - reclaim_start)
    }
    /// Allocated physical extents currently backing the arena payload file.
    /// A layout measurement for diagnostics and benchmarks; unavailable off Windows.
    pub fn physical_extent_count(&self) -> Result<u64, MirageError> {
        physical_extent_count(&self.file)
    }
}

fn metadata_path(path: &Path) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(".meta");
    PathBuf::from(value)
}
fn metadata_offset(slot: u32) -> Result<u64, MirageError> {
    u64::from(slot)
        .checked_mul(SLOT_METADATA_BYTES as u64)
        .and_then(|value| value.checked_add(ARENA_HEADER_BYTES as u64))
        .ok_or_else(|| MirageError::invalid_argument("cache metadata offset overflows"))
}
fn metadata_length(layout: CacheLayout) -> Result<u64, MirageError> {
    layout
        .metadata_bytes()?
        .checked_add(ARENA_HEADER_BYTES as u64)
        .ok_or_else(|| MirageError::invalid_argument("cache metadata length overflows"))
}

#[cfg(windows)]
fn set_sparse(file: &File) -> Result<(), MirageError> {
    crate::windows_sparse::set_sparse(file)
}
#[cfg(not(windows))]
fn set_sparse(_file: &File) -> Result<(), MirageError> {
    Ok(())
}
#[cfg(windows)]
fn allocated_bytes(path: &Path) -> Result<u64, MirageError> {
    crate::windows_sparse::allocated_bytes(path)
}

#[cfg(windows)]
fn deallocate(file: &File, offset: u64, length: u64) -> Result<(), MirageError> {
    crate::windows_sparse::deallocate(file, offset, length)
}
#[cfg(windows)]
fn allocated_bytes_in_range(file: &File, offset: u64, length: u64) -> Result<u64, MirageError> {
    crate::windows_sparse::allocated_bytes_in_range(file, offset, length)
}
#[cfg(windows)]
fn physical_extent_count(file: &File) -> Result<u64, MirageError> {
    crate::windows_sparse::physical_extent_count(file)
}
#[cfg(not(windows))]
fn physical_extent_count(_file: &File) -> Result<u64, MirageError> {
    Err(MirageError::provider_unavailable(
        "physical extent accounting is unavailable on this platform",
    ))
}
#[cfg(not(windows))]
fn allocated_bytes_in_range(_file: &File, _offset: u64, _length: u64) -> Result<u64, MirageError> {
    Err(MirageError::provider_unavailable(
        "per-slot physical allocation accounting is unavailable on this platform",
    ))
}
#[cfg(not(windows))]
fn deallocate(file: &File, offset: u64, length: u64) -> Result<(), MirageError> {
    static ZERO_CHUNK: [u8; 64 * 1024] = [0; 64 * 1024];
    let mut remaining = length;
    let mut cursor = offset;
    while remaining != 0 {
        let count = remaining.min(ZERO_CHUNK.len() as u64) as usize;
        write_exact_at(file, &ZERO_CHUNK[..count], cursor).map_err(MirageError::from)?;
        cursor += count as u64;
        remaining -= count as u64;
    }
    file.sync_data().map_err(MirageError::from)
}
#[cfg(not(windows))]
fn allocated_bytes(path: &Path) -> Result<u64, MirageError> {
    use std::os::unix::fs::MetadataExt;
    Ok(std::fs::metadata(path).map_err(MirageError::from)?.blocks() * 512)
}
