use mirage_crypto::aead::{NONCE_LENGTH, RepositoryKey};
use mirage_types::{ContentHash, MirageError, PageHash, RepositoryId};

const MAGIC: &[u8; 8] = b"MENCv001";
const PREFIX_LENGTH: usize = 12 + 16 + NONCE_LENGTH;

#[derive(Debug, Clone, Copy)]
pub struct EncryptedFrameAad {
    pub repository: RepositoryId,
    pub pack_id: [u8; 16],
    pub frame_index: u64,
    pub plaintext_hash: PageHash,
    pub plaintext_length: u32,
}
impl EncryptedFrameAad {
    fn encode(self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(76);
        bytes.extend_from_slice(b"MirageSSD encrypted frame v1\0");
        bytes.extend_from_slice(self.repository.as_bytes());
        bytes.extend_from_slice(&self.pack_id);
        bytes.extend_from_slice(&self.frame_index.to_le_bytes());
        bytes.extend_from_slice(self.plaintext_hash.as_bytes());
        bytes.extend_from_slice(&self.plaintext_length.to_le_bytes());
        bytes
    }
}

pub fn encode_encrypted_frame(
    key: &RepositoryKey,
    plaintext: &[u8],
    mut aad: EncryptedFrameAad,
) -> Result<Vec<u8>, MirageError> {
    aad.plaintext_length = u32::try_from(plaintext.len())
        .map_err(|_| MirageError::invalid_argument("encrypted frame plaintext is oversized"))?;
    let actual = ContentHash::from_bytes(*blake3::hash(plaintext).as_bytes());
    if actual.as_bytes() != aad.plaintext_hash.as_bytes() {
        return Err(MirageError::integrity_mismatch(
            "encrypted frame plaintext hash disagrees with metadata",
        ));
    }
    let (nonce, ciphertext) = key.seal(plaintext, &aad.encode())?;
    let length = u32::try_from(ciphertext.len())
        .map_err(|_| MirageError::invalid_argument("encrypted frame ciphertext is oversized"))?;
    let mut output = Vec::with_capacity(PREFIX_LENGTH + ciphertext.len());
    output.extend_from_slice(MAGIC);
    output.extend_from_slice(&length.to_le_bytes());
    output.extend_from_slice(&aad.pack_id);
    output.extend_from_slice(&nonce);
    output.extend_from_slice(&ciphertext);
    Ok(output)
}

pub fn decode_encrypted_frame(
    key: &RepositoryKey,
    frame: &[u8],
    aad: EncryptedFrameAad,
) -> Result<Vec<u8>, MirageError> {
    if frame.len() < PREFIX_LENGTH || &frame[..8] != MAGIC {
        return Err(MirageError::integrity_mismatch(
            "encrypted frame header is invalid",
        ));
    }
    let length = u32::from_le_bytes(frame[8..12].try_into().unwrap()) as usize;
    if length != frame.len() - PREFIX_LENGTH {
        return Err(MirageError::integrity_mismatch(
            "encrypted frame length is invalid",
        ));
    }
    if frame[12..28] != aad.pack_id {
        return Err(MirageError::integrity_mismatch(
            "encrypted frame pack identity is invalid",
        ));
    }
    let nonce: [u8; NONCE_LENGTH] = frame[28..PREFIX_LENGTH].try_into().unwrap();
    let plaintext = key.open(&nonce, &frame[PREFIX_LENGTH..], &aad.encode())?;
    if plaintext.len() != aad.plaintext_length as usize
        || blake3::hash(&plaintext).as_bytes() != aad.plaintext_hash.as_bytes()
    {
        return Err(MirageError::integrity_mismatch(
            "decrypted frame identity is invalid",
        ));
    }
    Ok(plaintext)
}

pub fn encrypted_frame_pack_id(frame: &[u8]) -> Result<[u8; 16], MirageError> {
    if frame.len() < PREFIX_LENGTH || &frame[..8] != MAGIC {
        return Err(MirageError::integrity_mismatch(
            "encrypted frame header is invalid",
        ));
    }
    frame[12..28]
        .try_into()
        .map_err(|_| MirageError::integrity_mismatch("encrypted frame pack ID is truncated"))
}
