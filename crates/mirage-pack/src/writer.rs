use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use mirage_crypto::aead::RepositoryKey;
use mirage_manifest::Codec;
use mirage_types::{ContentHash, MirageError, PackId, PageHash, RepositoryId};

use crate::encrypted_frame::{EncryptedFrameAad, encode_encrypted_frame};
use crate::footer::PackFooter;
use crate::format::{FOOTER_LEN, PACK_HEADER_LEN, PackHeader};
use crate::frame::encode_plain_frame;
use crate::index::{PackEntry, encode_index};
use crate::page::PlainPage;
use crate::reader::PackReader;

#[derive(Debug, Clone, Copy)]
pub struct PackWriterOptions {
    pub page_size: u32,
    pub target_size: u64,
    pub align_frames_4k: bool,
}

#[derive(Clone)]
pub struct PackEncryption {
    pub repository_id: RepositoryId,
    pub key: Arc<RepositoryKey>,
}

impl std::fmt::Debug for PackEncryption {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PackEncryption")
            .field("repository_id", &self.repository_id)
            .field("key", &"[REDACTED]")
            .finish()
    }
}

impl Default for PackWriterOptions {
    fn default() -> Self {
        Self {
            page_size: 1024 * 1024,
            target_size: 512 * 1024 * 1024,
            align_frames_4k: true,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CompletedPack {
    pub path: PathBuf,
    pub content_hash: ContentHash,
    pub byte_length: u64,
    pub entries: Vec<PackEntry>,
}

pub struct PackWriter {
    staging_directory: PathBuf,
    temporary_path: PathBuf,
    file: File,
    options: PackWriterOptions,
    entries: Vec<PackEntry>,
    known: HashMap<PageHash, PackEntry>,
    position: u64,
    encryption: Option<PackEncryption>,
    pack_id: [u8; 16],
}

impl PackWriter {
    pub fn create(
        staging_directory: &Path,
        options: PackWriterOptions,
    ) -> Result<Self, MirageError> {
        Self::create_inner(staging_directory, options, None)
    }

    pub fn create_encrypted(
        staging_directory: &Path,
        options: PackWriterOptions,
        encryption: PackEncryption,
    ) -> Result<Self, MirageError> {
        Self::create_inner(staging_directory, options, Some(encryption))
    }

    fn create_inner(
        staging_directory: &Path,
        options: PackWriterOptions,
        encryption: Option<PackEncryption>,
    ) -> Result<Self, MirageError> {
        validate_options(options)?;
        std::fs::create_dir_all(staging_directory).map_err(MirageError::from)?;
        let mut writer_id = [0_u8; 16];
        getrandom::fill(&mut writer_id)
            .map_err(|_| MirageError::internal_invariant("secure pack ID generation failed"))?;
        let pack_id = if encryption.is_some() {
            writer_id
        } else {
            [0; 16]
        };
        let temporary_path = staging_directory.join(format!(
            "pack-building-{}.tmp",
            PackId::from_bytes(writer_id)
        ));
        let mut file = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&temporary_path)
            .map_err(MirageError::from)?;
        file.write_all(&[0_u8; PACK_HEADER_LEN])
            .map_err(MirageError::from)?;
        Ok(Self {
            staging_directory: staging_directory.to_path_buf(),
            temporary_path,
            file,
            options,
            entries: Vec::new(),
            known: HashMap::new(),
            position: PACK_HEADER_LEN as u64,
            encryption,
            pack_id,
        })
    }

    pub fn append_page(&mut self, page: &PlainPage) -> Result<PackEntry, MirageError> {
        if let Some(entry) = self.known.get(&page.hash) {
            if entry.logical_length != page.logical_len {
                return Err(MirageError::integrity_mismatch(
                    "duplicate page hash has a different logical length",
                ));
            }
            return Ok(*entry);
        }
        if page.logical_len > self.options.page_size {
            return Err(MirageError::invalid_argument(
                "page exceeds the pack page size",
            ));
        }
        let aligned = if self.options.align_frames_4k {
            align_up(self.position, 4096)?
        } else {
            self.position
        };
        let frame = if let Some(encryption) = &self.encryption {
            encode_encrypted_frame(
                &encryption.key,
                &page.bytes,
                EncryptedFrameAad {
                    repository: encryption.repository_id,
                    pack_id: self.pack_id,
                    frame_index: aligned,
                    plaintext_hash: page.hash,
                    plaintext_length: page.logical_len,
                },
            )?
        } else {
            encode_plain_frame(page)?
        };
        let frame_length = u64::try_from(frame.len())
            .map_err(|_| MirageError::invalid_argument("frame length overflows"))?;
        let end = aligned
            .checked_add(frame_length)
            .ok_or_else(|| MirageError::invalid_argument("pack length overflows"))?;
        if !self.entries.is_empty() && end > self.options.target_size {
            return Err(MirageError::cache_full(
                "pack target size reached; finish and roll over",
            ));
        }
        if aligned > self.position {
            write_zeros(&mut self.file, aligned - self.position)?;
        }
        self.file.write_all(&frame).map_err(MirageError::from)?;
        self.position = end;
        let entry = PackEntry {
            page_hash: page.hash,
            frame_offset: aligned,
            frame_length,
            logical_length: page.logical_len,
            encoded_length: page.logical_len,
            codec: Codec::None,
            encrypted: self.encryption.is_some(),
        };
        self.entries.push(entry);
        self.known.insert(page.hash, entry);
        Ok(entry)
    }

    pub fn would_rollover(&self, page: &PlainPage) -> Result<bool, MirageError> {
        if self.known.contains_key(&page.hash) || self.entries.is_empty() {
            return Ok(false);
        }
        let aligned = if self.options.align_frames_4k {
            align_up(self.position, 4096)?
        } else {
            self.position
        };
        let overhead = if self.encryption.is_some() {
            68_u64
        } else {
            64_u64
        };
        let frame_length = overhead
            .checked_add(u64::from(page.logical_len))
            .ok_or_else(|| MirageError::invalid_argument("frame length overflows"))?;
        let projected_entries = self.entries.len().saturating_add(1) as u64;
        let projected_length = aligned
            .checked_add(frame_length)
            .and_then(|value| value.checked_add(projected_entries.saturating_mul(64)))
            .and_then(|value| value.checked_add(128))
            .ok_or_else(|| MirageError::invalid_argument("projected pack length overflows"))?;
        Ok(projected_length > self.options.target_size)
    }

    #[must_use]
    pub fn contains_page(&self, hash: PageHash) -> bool {
        self.known.contains_key(&hash)
    }

    pub fn abort(self) -> Result<(), MirageError> {
        drop(self.file);
        std::fs::remove_file(self.temporary_path).map_err(MirageError::from)
    }

    pub fn finish(mut self) -> Result<CompletedPack, MirageError> {
        if self.entries.is_empty() {
            return Err(MirageError::invalid_argument("cannot finish an empty pack"));
        }
        let mut sorted = self.entries.clone();
        sorted.sort_by_key(|entry| entry.page_hash);
        let index = encode_index(&sorted);
        let index_offset = self.position;
        self.file.write_all(&index).map_err(MirageError::from)?;
        let index_length = u64::try_from(index.len())
            .map_err(|_| MirageError::invalid_argument("pack index length overflows"))?;
        let content_length = index_offset
            .checked_add(index_length)
            .ok_or_else(|| MirageError::invalid_argument("pack content length overflows"))?;
        let pack_length = content_length
            .checked_add(FOOTER_LEN as u64)
            .ok_or_else(|| MirageError::invalid_argument("pack file length overflows"))?;
        let header = PackHeader {
            page_size: self.options.page_size,
            index_offset,
            index_length,
            entry_count: sorted.len() as u64,
            encrypted: self.encryption.is_some(),
            pack_id: self.pack_id,
        };
        self.file
            .seek(SeekFrom::Start(0))
            .map_err(MirageError::from)?;
        self.file
            .write_all(&header.encode())
            .map_err(MirageError::from)?;
        self.file.flush().map_err(MirageError::from)?;
        self.file.sync_all().map_err(MirageError::from)?;
        let body_hash = hash_prefix(&mut self.file, content_length)?;
        let footer = PackFooter {
            index_hash: ContentHash::from_bytes(*blake3::hash(&index).as_bytes()),
            content_hash: body_hash,
            pack_length,
        };
        self.file
            .seek(SeekFrom::Start(content_length))
            .map_err(MirageError::from)?;
        self.file
            .write_all(&footer.encode())
            .map_err(MirageError::from)?;
        self.file.flush().map_err(MirageError::from)?;
        self.file.sync_all().map_err(MirageError::from)?;
        let content_hash = hash_prefix(&mut self.file, pack_length)?;
        drop(self.file);

        drop(PackReader::open_verified(&self.temporary_path)?);
        let final_path = self
            .staging_directory
            .join(format!("pack-{}.bin", content_hash));
        if final_path.exists() {
            let existing = PackReader::open_verified(&final_path)?;
            if existing.content_hash() != content_hash {
                return Err(MirageError::integrity_mismatch(
                    "existing immutable pack name has different content",
                ));
            }
            std::fs::remove_file(&self.temporary_path).map_err(MirageError::from)?;
        } else {
            std::fs::rename(&self.temporary_path, &final_path).map_err(MirageError::from)?;
        }
        Ok(CompletedPack {
            path: final_path,
            content_hash,
            byte_length: pack_length,
            entries: sorted,
        })
    }
}

fn validate_options(options: PackWriterOptions) -> Result<(), MirageError> {
    if !(64 * 1024..=16 * 1024 * 1024).contains(&options.page_size)
        || !options.page_size.is_power_of_two()
        || options.target_size < u64::from(options.page_size) + 4096
    {
        return Err(MirageError::invalid_argument(
            "pack writer options are invalid",
        ));
    }
    Ok(())
}

fn align_up(value: u64, alignment: u64) -> Result<u64, MirageError> {
    value
        .checked_add(alignment - 1)
        .map(|sum| sum / alignment * alignment)
        .ok_or_else(|| MirageError::invalid_argument("pack alignment overflows"))
}

fn write_zeros(file: &mut File, count: u64) -> Result<(), MirageError> {
    const ZEROES: [u8; 4096] = [0; 4096];
    let mut remaining = count;
    while remaining != 0 {
        let length = usize::try_from(remaining.min(ZEROES.len() as u64))
            .expect("bounded padding fits usize");
        file.write_all(&ZEROES[..length])
            .map_err(MirageError::from)?;
        remaining -= length as u64;
    }
    Ok(())
}

fn hash_prefix(file: &mut File, length: u64) -> Result<ContentHash, MirageError> {
    file.seek(SeekFrom::Start(0)).map_err(MirageError::from)?;
    let mut take = file.take(length);
    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let read = take.read(&mut buffer).map_err(MirageError::from)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    if take.limit() != 0 {
        return Err(MirageError::integrity_mismatch(
            "pack content truncated while hashing",
        ));
    }
    Ok(ContentHash::from_bytes(*hasher.finalize().as_bytes()))
}
