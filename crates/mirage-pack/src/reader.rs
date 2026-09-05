use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Arc;

use bytes::Bytes;
use mirage_crypto::aead::RepositoryKey;
use mirage_manifest::Codec;
use mirage_types::{ContentHash, MirageError, PageHash, RepositoryId};

use crate::encrypted_frame::{EncryptedFrameAad, decode_encrypted_frame, encrypted_frame_pack_id};
use crate::footer::PackFooter;
use crate::format::{FOOTER_LEN, PACK_HEADER_LEN, PackHeader};
use crate::frame::{DecodedFrame, decode_plain_frame};
use crate::index::{PackEntry, decode_index};

pub struct PackReader {
    file: File,
    file_length: u64,
    object_hash: ContentHash,
    entries: Vec<PackEntry>,
    encrypted: bool,
    pack_id: [u8; 16],
    encryption: Option<PackReadEncryption>,
}

#[derive(Clone)]
pub struct PackReadEncryption {
    pub repository_id: RepositoryId,
    pub key: Arc<RepositoryKey>,
}

impl PackReader {
    pub fn open_verified(path: &Path) -> Result<Self, MirageError> {
        Self::open_inner(path, None)
    }

    pub fn open_verified_encrypted(
        path: &Path,
        encryption: PackReadEncryption,
    ) -> Result<Self, MirageError> {
        Self::open_inner(path, Some(encryption))
    }

    fn open_inner(
        path: &Path,
        encryption: Option<PackReadEncryption>,
    ) -> Result<Self, MirageError> {
        let mut file = File::open(path).map_err(MirageError::from)?;
        let file_length = file.metadata().map_err(MirageError::from)?.len();
        let minimum = (PACK_HEADER_LEN + FOOTER_LEN) as u64;
        if file_length < minimum {
            return Err(MirageError::manifest_invalid(
                "pack is shorter than its fixed structures",
            ));
        }
        let mut header_bytes = [0_u8; PACK_HEADER_LEN];
        file.read_exact(&mut header_bytes)
            .map_err(MirageError::from)?;
        let header = PackHeader::decode(&header_bytes)?;
        file.seek(SeekFrom::Start(file_length - FOOTER_LEN as u64))
            .map_err(MirageError::from)?;
        let mut footer_bytes = [0_u8; FOOTER_LEN];
        file.read_exact(&mut footer_bytes)
            .map_err(MirageError::from)?;
        let footer = PackFooter::decode(&footer_bytes)?;
        if footer.pack_length != file_length
            || header.index_offset < PACK_HEADER_LEN as u64
            || header.index_offset.checked_add(header.index_length)
                != Some(file_length - FOOTER_LEN as u64)
        {
            return Err(MirageError::manifest_invalid(
                "pack header, footer, and file length disagree",
            ));
        }
        let index_len = usize::try_from(header.index_length)
            .map_err(|_| MirageError::manifest_invalid("pack index cannot fit memory"))?;
        let mut index_bytes = vec![0_u8; index_len];
        file.seek(SeekFrom::Start(header.index_offset))
            .map_err(MirageError::from)?;
        file.read_exact(&mut index_bytes)
            .map_err(MirageError::from)?;
        if blake3::hash(&index_bytes).as_bytes() != footer.index_hash.as_bytes() {
            return Err(MirageError::integrity_mismatch(
                "pack index hash is invalid",
            ));
        }
        let body_hash = hash_prefix(&mut file, file_length - FOOTER_LEN as u64)?;
        if body_hash != footer.content_hash {
            return Err(MirageError::integrity_mismatch(
                "pack content hash is invalid",
            ));
        }
        let object_hash = hash_prefix(&mut file, file_length)?;
        let entries = decode_index(&index_bytes, header.entry_count)?;
        validate_entries(&entries, header.index_offset, header.page_size)?;
        if entries
            .iter()
            .any(|entry| entry.encrypted != header.encrypted)
        {
            return Err(MirageError::integrity_mismatch(
                "pack header and frame encryption flags disagree",
            ));
        }
        Ok(Self {
            file,
            file_length,
            object_hash,
            entries,
            encrypted: header.encrypted,
            pack_id: header.pack_id,
            encryption,
        })
    }

    #[must_use]
    pub const fn content_hash(&self) -> ContentHash {
        self.object_hash
    }

    #[must_use]
    pub const fn file_length(&self) -> u64 {
        self.file_length
    }

    #[must_use]
    pub fn entries(&self) -> &[PackEntry] {
        &self.entries
    }

    #[must_use]
    pub const fn is_encrypted(&self) -> bool {
        self.encrypted
    }

    #[must_use]
    pub const fn pack_id(&self) -> [u8; 16] {
        self.pack_id
    }

    pub fn lookup(&self, hash: PageHash) -> Option<PackEntry> {
        self.entries
            .binary_search_by_key(&hash, |entry| entry.page_hash)
            .ok()
            .map(|index| self.entries[index])
    }

    pub fn read_page(&mut self, hash: PageHash) -> Result<DecodedFrame, MirageError> {
        let entry = self.lookup(hash).ok_or_else(|| {
            MirageError::remote_object_missing("page is absent from immutable pack")
        })?;
        let length = usize::try_from(entry.frame_length)
            .map_err(|_| MirageError::manifest_invalid("frame length cannot fit memory"))?;
        let mut bytes = vec![0_u8; length];
        self.file
            .seek(SeekFrom::Start(entry.frame_offset))
            .map_err(MirageError::from)?;
        self.file
            .read_exact(&mut bytes)
            .map_err(MirageError::from)?;
        let decoded = decode_entry_frame(&bytes, entry, self.encryption.as_ref(), self.pack_id)?;
        if decoded.page.hash != hash {
            return Err(MirageError::integrity_mismatch(
                "decoded frame hash differs from index",
            ));
        }
        Ok(decoded)
    }

    pub fn decode_page_from_range(
        range_start: u64,
        bytes: &[u8],
        entry: PackEntry,
    ) -> Result<DecodedFrame, MirageError> {
        let relative = entry.frame_offset.checked_sub(range_start).ok_or_else(|| {
            MirageError::invalid_argument("coalesced range starts after requested frame")
        })?;
        let start = usize::try_from(relative)
            .map_err(|_| MirageError::invalid_argument("frame offset cannot fit memory"))?;
        let length = usize::try_from(entry.frame_length)
            .map_err(|_| MirageError::invalid_argument("frame length cannot fit memory"))?;
        let end = start
            .checked_add(length)
            .ok_or_else(|| MirageError::invalid_argument("frame slice overflows"))?;
        let frame = bytes
            .get(start..end)
            .ok_or_else(|| MirageError::integrity_mismatch("coalesced range is truncated"))?;
        let decoded = decode_plain_frame(frame)?;
        if decoded.page.hash != entry.page_hash {
            return Err(MirageError::integrity_mismatch(
                "coalesced frame differs from index",
            ));
        }
        Ok(decoded)
    }

    pub fn decode_page_from_range_encrypted(
        range_start: u64,
        bytes: &[u8],
        entry: PackEntry,
        encryption: &PackReadEncryption,
    ) -> Result<DecodedFrame, MirageError> {
        let relative = entry.frame_offset.checked_sub(range_start).ok_or_else(|| {
            MirageError::invalid_argument("coalesced range starts after requested frame")
        })?;
        let start = usize::try_from(relative)
            .map_err(|_| MirageError::invalid_argument("frame offset cannot fit memory"))?;
        let length = usize::try_from(entry.frame_length)
            .map_err(|_| MirageError::manifest_invalid("frame length cannot fit memory"))?;
        let end = start
            .checked_add(length)
            .ok_or_else(|| MirageError::invalid_argument("frame slice overflows"))?;
        let frame = bytes
            .get(start..end)
            .ok_or_else(|| MirageError::integrity_mismatch("coalesced range is truncated"))?;
        let pack_id = encrypted_frame_pack_id(frame)?;
        let decoded = decode_entry_frame(frame, entry, Some(encryption), pack_id)?;
        if decoded.page.hash != entry.page_hash {
            return Err(MirageError::integrity_mismatch(
                "coalesced encrypted frame differs from index",
            ));
        }
        Ok(decoded)
    }
}

fn decode_entry_frame(
    frame: &[u8],
    entry: PackEntry,
    encryption: Option<&PackReadEncryption>,
    pack_id: [u8; 16],
) -> Result<DecodedFrame, MirageError> {
    if !entry.encrypted {
        return decode_plain_frame(frame);
    }
    let encryption = encryption.ok_or_else(|| {
        MirageError::backend_unauthenticated("encrypted pack requires its repository key")
    })?;
    let plaintext = decode_encrypted_frame(
        &encryption.key,
        frame,
        EncryptedFrameAad {
            repository: encryption.repository_id,
            pack_id,
            frame_index: entry.frame_offset,
            plaintext_hash: entry.page_hash,
            plaintext_length: entry.logical_length,
        },
    )?;
    Ok(DecodedFrame {
        page: crate::PlainPage {
            hash: entry.page_hash,
            logical_len: entry.logical_length,
            bytes: Bytes::from(plaintext),
        },
        codec: Codec::None,
    })
}

fn validate_entries(
    entries: &[PackEntry],
    index_offset: u64,
    page_size: u32,
) -> Result<(), MirageError> {
    let mut physical = entries.to_vec();
    physical.sort_by_key(|entry| entry.frame_offset);
    let mut previous_end = PACK_HEADER_LEN as u64;
    for entry in physical {
        let end = entry
            .frame_offset
            .checked_add(entry.frame_length)
            .ok_or_else(|| MirageError::manifest_invalid("pack frame range overflows"))?;
        if entry.frame_offset < previous_end
            || end > index_offset
            || entry.logical_length > page_size
        {
            return Err(MirageError::manifest_invalid(
                "pack frame ranges overlap or exceed content",
            ));
        }
        previous_end = end;
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
            "pack truncated while hashing",
        ));
    }
    Ok(ContentHash::from_bytes(*hasher.finalize().as_bytes()))
}
