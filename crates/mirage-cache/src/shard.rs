use std::fs::File;
use std::path::{Path, PathBuf};

use mirage_types::MirageError;

use crate::arena::{open_restrictive, read_exact_at, write_exact_at};
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
        })
    }
    pub fn open(path: &Path, expected: CacheLayout) -> Result<Self, MirageError> {
        expected.validate()?;
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
        Ok(Self {
            path: path.to_path_buf(),
            file,
            metadata,
            layout: expected,
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
        read_exact_at(
            &self.file,
            output,
            self.slot_offset(slot)?
                .checked_add(u64::from(offset))
                .ok_or_else(|| MirageError::invalid_argument("cache read offset overflows"))?,
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
