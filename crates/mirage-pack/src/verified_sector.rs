//! Derived verification knowledge for unchanged MENCv001 frames.
//!
//! Bootstrap verifies the original complete AEAD frame. Later sector reads
//! authenticate selected plaintext to its existing trusted BLAKE3 root. They
//! do not authenticate unread ciphertext or the original whole-frame tag.
//! Catalogs are bounded, local, ephemeral, and never a new trust authority.

use blake3::hazmat::{
    HasherExt, Mode, left_subtree_len, merge_subtrees_non_root, merge_subtrees_root,
};
use chacha20::XChaCha20;
use chacha20::cipher::{KeyIvInit, StreamCipher, StreamCipherSeek};
use mirage_crypto::aead::RepositoryKey;
use mirage_types::MirageError;
use zeroize::Zeroizing;

use crate::encrypted_frame::{EncryptedFrameAad, decode_encrypted_frame};

pub const VERIFIED_SECTOR_BYTES: usize = 64 * 1024;
pub const MAX_VERIFIED_FRAME_BYTES: usize = 4 * 1024 * 1024;
pub const ENCRYPTED_FRAME_PREFIX_BYTES: usize = 52;

type Cv = [u8; 32];

/// Only constructed from a frame that passed complete AEAD and root checks.
/// The compact leaf values use absolute BLAKE3 chunk counters, not standalone
/// sector hashes. No plaintext is retained in this catalog.
pub struct FrameProofCatalog {
    header: [u8; ENCRYPTED_FRAME_PREFIX_BYTES],
    root: Cv,
    total: usize,
    leaves: Box<[Cv]>,
    repository: mirage_types::RepositoryId,
    frame_index: u64,
}

impl FrameProofCatalog {
    #[must_use]
    pub fn memory_bytes(&self) -> usize {
        std::mem::size_of::<Self>() + self.leaves.len() * std::mem::size_of::<Cv>()
    }
}

pub fn learn_verified_frame(
    key: &RepositoryKey,
    encoded: &[u8],
    aad: EncryptedFrameAad,
) -> Result<(Vec<u8>, FrameProofCatalog), MirageError> {
    if aad.plaintext_length as usize > MAX_VERIFIED_FRAME_BYTES {
        return Err(invalid());
    }
    let plaintext = decode_encrypted_frame(key, encoded, aad)?;
    let leaves = plaintext
        .chunks(VERIFIED_SECTOR_BYTES)
        .enumerate()
        .map(|(index, bytes)| {
            blake3::Hasher::new()
                .set_input_offset((index * VERIFIED_SECTOR_BYTES) as u64)
                .update(bytes)
                .finalize_non_root()
        })
        .collect();
    let header = encoded[..ENCRYPTED_FRAME_PREFIX_BYTES]
        .try_into()
        .map_err(|_| invalid())?;
    let catalog = FrameProofCatalog {
        header,
        root: *aad.plaintext_hash.as_bytes(),
        total: plaintext.len(),
        leaves,
        repository: aad.repository,
        frame_index: aad.frame_index,
    };
    Ok((plaintext, catalog))
}

/// Decrypt exactly one aligned sector (or the shorter final sector). Output
/// stays private and is zeroized on failure, including a wrong nonce or key.
/// The caller supplies the trusted frame root and length from publication.
pub fn decode_verified_sector(
    key: &RepositoryKey,
    catalog: &FrameProofCatalog,
    aad: EncryptedFrameAad,
    offset: usize,
    ciphertext: &[u8],
) -> Result<Vec<u8>, MirageError> {
    let total = aad.plaintext_length as usize;
    if total == 0
        || total > MAX_VERIFIED_FRAME_BYTES
        || catalog.total != total
        || catalog.repository != aad.repository
        || catalog.frame_index != aad.frame_index
        || catalog.root != *aad.plaintext_hash.as_bytes()
        || catalog.leaves.len() != total.div_ceil(VERIFIED_SECTOR_BYTES)
        || !offset.is_multiple_of(VERIFIED_SECTOR_BYTES)
        || offset >= total
        || ciphertext.len() != VERIFIED_SECTOR_BYTES.min(total - offset)
        || &catalog.header[..8] != b"MENCv001"
        || catalog.header[12..28] != aad.pack_id
        || u32::from_le_bytes(catalog.header[8..12].try_into().map_err(|_| invalid())?) as usize
            != total + 16
    {
        return Err(invalid());
    }
    let nonce: [u8; 24] = catalog.header[28..].try_into().map_err(|_| invalid())?;
    let secret = key.secret_bytes();
    let mut cipher = XChaCha20::new(secret.as_ref().into(), (&nonce).into());
    let mut plaintext = Zeroizing::new(ciphertext.to_vec());
    // AEAD uses stream block zero for the Poly1305 key; payload starts at 64.
    cipher.try_seek(64 + offset as u64).map_err(|_| invalid())?;
    cipher
        .try_apply_keystream(&mut plaintext)
        .map_err(|_| invalid())?;
    let actual = if total <= VERIFIED_SECTOR_BYTES {
        *blake3::hash(&plaintext).as_bytes()
    } else {
        let selected = blake3::Hasher::new()
            .set_input_offset(offset as u64)
            .update(&plaintext)
            .finalize_non_root();
        rebuild(
            &catalog.leaves,
            0,
            total,
            offset / VERIFIED_SECTOR_BYTES,
            selected,
            true,
        )?
    };
    if actual != *aad.plaintext_hash.as_bytes() {
        return Err(invalid());
    }
    Ok(plaintext.to_vec())
}

fn rebuild(
    leaves: &[Cv],
    start: usize,
    length: usize,
    selected_index: usize,
    selected: Cv,
    root: bool,
) -> Result<Cv, MirageError> {
    if length <= VERIFIED_SECTOR_BYTES {
        let index = start / VERIFIED_SECTOR_BYTES;
        return if index == selected_index {
            Ok(selected)
        } else {
            leaves.get(index).copied().ok_or_else(invalid)
        };
    }
    let split = left_subtree_len(length as u64) as usize;
    let left = rebuild(leaves, start, split, selected_index, selected, false)?;
    let right = rebuild(
        leaves,
        start + split,
        length - split,
        selected_index,
        selected,
        false,
    )?;
    Ok(if root {
        *merge_subtrees_root(&left, &right, Mode::Hash).as_bytes()
    } else {
        merge_subtrees_non_root(&left, &right, Mode::Hash)
    })
}

fn invalid() -> MirageError {
    MirageError::integrity_mismatch("encrypted sector does not prove the published frame identity")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encrypted_frame::encode_encrypted_frame;
    use mirage_types::{PageHash, RepositoryId};

    fn fixture(length: usize) -> (RepositoryKey, Vec<u8>, Vec<u8>, EncryptedFrameAad) {
        let key = RepositoryKey::from_bytes([7; 32]);
        let plain: Vec<u8> = (0..length)
            .map(|i| (i.wrapping_mul(31) % 251) as u8)
            .collect();
        let aad = EncryptedFrameAad {
            repository: RepositoryId::from_bytes([4; 16]),
            pack_id: [5; 16],
            frame_index: 8,
            plaintext_hash: PageHash::from_bytes(*blake3::hash(&plain).as_bytes()),
            plaintext_length: length as u32,
        };
        let encoded = encode_encrypted_frame(&key, &plain, aad).unwrap();
        (key, plain, encoded, aad)
    }

    #[test]
    fn exact_sectors_match_legacy_frames_including_unbalanced_trees() {
        for length in [
            0, 1, 1023, 1024, 1025, 65535, 65536, 65537, 196613, 4194297, 4194304,
        ] {
            let (key, plain, encoded, aad) = fixture(length);
            let (verified, catalog) = learn_verified_frame(&key, &encoded, aad).unwrap();
            assert_eq!(verified, plain);
            for offset in (0..length).step_by(VERIFIED_SECTOR_BYTES) {
                let end = (offset + VERIFIED_SECTOR_BYTES).min(length);
                let sector = &encoded
                    [ENCRYPTED_FRAME_PREFIX_BYTES + offset..ENCRYPTED_FRAME_PREFIX_BYTES + end];
                assert_eq!(
                    decode_verified_sector(&key, &catalog, aad, offset, sector).unwrap(),
                    plain[offset..end]
                );
            }
        }
    }

    #[test]
    fn damaged_ciphertext_catalog_nonce_key_and_metadata_fail_closed() {
        let (key, _, encoded, aad) = fixture(3 * VERIFIED_SECTOR_BYTES + 31);
        let (_, mut catalog) = learn_verified_frame(&key, &encoded, aad).unwrap();
        let offset = VERIFIED_SECTOR_BYTES;
        let sector = &encoded[52 + offset..52 + offset + VERIFIED_SECTOR_BYTES];
        let mut bad_sector = sector.to_vec();
        bad_sector[29] ^= 1;
        assert!(decode_verified_sector(&key, &catalog, aad, offset, &bad_sector).is_err());
        assert!(
            decode_verified_sector(
                &RepositoryKey::from_bytes([9; 32]),
                &catalog,
                aad,
                offset,
                sector
            )
            .is_err()
        );
        catalog.header[28] ^= 1;
        assert!(decode_verified_sector(&key, &catalog, aad, offset, sector).is_err());
        catalog.header[28] ^= 1;
        catalog.leaves[0][0] ^= 1;
        assert!(decode_verified_sector(&key, &catalog, aad, offset, sector).is_err());
        catalog.leaves[0][0] ^= 1;
        let mut bad_aad = aad;
        bad_aad.plaintext_hash = PageHash::from_bytes([0; 32]);
        assert!(decode_verified_sector(&key, &catalog, bad_aad, offset, sector).is_err());
        bad_aad = aad;
        bad_aad.plaintext_length -= 1;
        assert!(decode_verified_sector(&key, &catalog, bad_aad, offset, sector).is_err());
        bad_aad = aad;
        bad_aad.pack_id[0] ^= 1;
        assert!(decode_verified_sector(&key, &catalog, bad_aad, offset, sector).is_err());
        bad_aad = aad;
        bad_aad.frame_index += 1;
        assert!(decode_verified_sector(&key, &catalog, bad_aad, offset, sector).is_err());
        bad_aad = aad;
        bad_aad.repository = RepositoryId::from_bytes([99; 16]);
        assert!(decode_verified_sector(&key, &catalog, bad_aad, offset, sector).is_err());
        assert!(decode_verified_sector(&key, &catalog, aad, offset + 1, sector).is_err());
        assert!(decode_verified_sector(&key, &catalog, aad, usize::MAX, sector).is_err());
        assert!(
            decode_verified_sector(&key, &catalog, aad, offset, &sector[..sector.len() - 1])
                .is_err()
        );
        let last = encoded.len() - 1;
        let mut bad_frame = encoded.clone();
        bad_frame[last] ^= 1;
        assert!(learn_verified_frame(&key, &bad_frame, aad).is_err());
    }
}
