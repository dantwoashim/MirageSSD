use crate::aead::RepositoryKey;
use crate::signing::{RepositorySigner, RepositoryVerifier};
use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::{
    XChaCha20Poly1305, XNonce,
    aead::{Aead, KeyInit, Payload},
};
use mirage_types::{MirageError, RepositoryId};
use zeroize::Zeroizing;

const MAGIC: &[u8; 8] = b"MREKv001";
pub fn export_signer(
    signer: &RepositorySigner,
    recovery_secret: &[u8],
) -> Result<Vec<u8>, MirageError> {
    if recovery_secret.len() < 12 {
        return Err(MirageError::invalid_argument(
            "recovery secret must contain at least 12 bytes",
        ));
    }
    let public = signer.verifier().public_key();
    let mut salt = [0_u8; 16];
    let mut nonce = [0_u8; 24];
    getrandom::fill(&mut salt)
        .map_err(|_| MirageError::internal_invariant("OS randomness unavailable"))?;
    getrandom::fill(&mut nonce)
        .map_err(|_| MirageError::internal_invariant("OS randomness unavailable"))?;
    let mut key = Zeroizing::new([0_u8; 32]);
    Argon2::default()
        .hash_password_into(recovery_secret, &salt, key.as_mut())
        .map_err(|_| MirageError::internal_invariant("recovery key derivation failed"))?;
    let ciphertext = XChaCha20Poly1305::new(key.as_ref().into())
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: signer.secret_bytes().as_ref(),
                aad: &public,
            },
        )
        .map_err(|_| MirageError::internal_invariant("recovery export encryption failed"))?;
    let mut out = Vec::with_capacity(80 + ciphertext.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&public);
    out.extend_from_slice(&salt);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}
pub fn import_signer(
    bytes: &[u8],
    recovery_secret: &[u8],
) -> Result<RepositorySigner, MirageError> {
    if bytes.len() != 128 || &bytes[..8] != MAGIC {
        return Err(MirageError::integrity_mismatch(
            "recovery key record is invalid",
        ));
    }
    let public: [u8; 32] = bytes[8..40].try_into().unwrap();
    let salt: [u8; 16] = bytes[40..56].try_into().unwrap();
    let nonce: [u8; 24] = bytes[56..80].try_into().unwrap();
    let mut key = Zeroizing::new([0_u8; 32]);
    Argon2::default()
        .hash_password_into(recovery_secret, &salt, key.as_mut())
        .map_err(|_| MirageError::internal_invariant("recovery key derivation failed"))?;
    let secret = Zeroizing::new(
        XChaCha20Poly1305::new(key.as_ref().into())
            .decrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: &bytes[80..],
                    aad: &public,
                },
            )
            .map_err(|_| {
                MirageError::integrity_mismatch("recovery secret or key record is invalid")
            })?,
    );
    let signer = RepositorySigner::from_secret(secret.as_ref())?;
    if signer.verifier().public_key() != public {
        return Err(MirageError::integrity_mismatch(
            "recovered key does not match trust root",
        ));
    }
    Ok(signer)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyTransition {
    pub sequence: u64,
    pub previous_key_id: [u8; 16],
    pub new_public_key: [u8; 32],
    pub signature: [u8; 64],
}
impl KeyTransition {
    pub fn create(
        sequence: u64,
        old: &RepositorySigner,
        new: &RepositoryVerifier,
    ) -> Result<Self, MirageError> {
        use mirage_manifest::CommitSigner;
        let body = transition_body(sequence, old.key_id(), new.public_key());
        let signature: [u8; 64] = old
            .sign(&body)?
            .try_into()
            .map_err(|_| MirageError::internal_invariant("Ed25519 signature has wrong length"))?;
        Ok(Self {
            sequence,
            previous_key_id: old.key_id(),
            new_public_key: new.public_key(),
            signature,
        })
    }
    pub fn verify(&self, old: &RepositoryVerifier) -> Result<RepositoryVerifier, MirageError> {
        use mirage_manifest::{CommitVerifier, SignatureAlgorithm, SignatureEnvelope};
        let envelope = SignatureEnvelope {
            algorithm: SignatureAlgorithm::Ed25519,
            key_id: self.previous_key_id,
            signature: self.signature.to_vec(),
        };
        old.verify(
            &transition_body(self.sequence, self.previous_key_id, self.new_public_key),
            &envelope,
        )?;
        RepositoryVerifier::from_public_key(self.new_public_key)
    }
}
fn transition_body(sequence: u64, old: [u8; 16], new: [u8; 32]) -> Vec<u8> {
    let mut body = b"MirageSSD key transition v1\0".to_vec();
    body.extend_from_slice(&sequence.to_le_bytes());
    body.extend_from_slice(&old);
    body.extend_from_slice(&new);
    body
}

// ---------------------------------------------------------------------------
// Portable recovery envelope (format v1)
//
// A recovery envelope moves the secrets needed to read an encrypted
// repository onto a genuinely different machine: the repository content key
// and, optionally, the commit signer secret. Everything but the sealed payload
// is plaintext metadata; the payload is encrypted under a key derived from a
// user-held recovery secret. The AEAD binds the repository identity, KDF
// parameters, and declared contents, so a tampered or mismatched envelope
// fails authentication rather than silently recovering the wrong key.
//
// Recovering the content key restores read access to repository bytes. It
// does NOT confer authority to resume the old writer lineage; writer
// ownership is a separate, explicitly transferred property.
// ---------------------------------------------------------------------------

const ENVELOPE_MAGIC: &[u8; 8] = b"MRECV001";
const ENVELOPE_VERSION: u16 = 1;
const KDF_ARGON2ID: u8 = 1;
const FLAG_CONTENT_KEY: u16 = 1;
const FLAG_SIGNER: u16 = 2;
const ENVELOPE_HEADER_LENGTH: usize = 116;
const MAX_ENVELOPE_LENGTH: usize = 4096;
const MIN_SECRET_LENGTH: usize = 12;

/// KDF parameters recorded in the envelope so later versions can strengthen
/// them without breaking existing envelopes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KdfParameters {
    pub memory_kib: u32,
    pub iterations: u32,
    pub lanes: u8,
}

impl KdfParameters {
    pub const DEFAULT: Self = Self {
        memory_kib: 64 * 1024,
        iterations: 3,
        lanes: 1,
    };
}

/// What an envelope can restore. `Complete` means content decryption plus
/// optional signing authority; `SignerOnly` covers trust history but cannot
/// recover encrypted content and is reported as incomplete for encrypted
/// repositories.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvelopeKind {
    Complete,
    SignerOnly,
    Empty,
}

/// Public metadata of a recovery envelope, readable without the secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvelopeInspection {
    pub repository_id: RepositoryId,
    pub kind: EnvelopeKind,
    pub has_content_key: bool,
    pub has_signer: bool,
    pub legacy_signer_only: bool,
}

/// A decrypted recovery envelope. Callers decide which parts to install;
/// presence of the signer does not authorize this machine to write.
pub struct RecoveryPayload {
    pub repository_id: RepositoryId,
    pub content_key: Option<RepositoryKey>,
    pub signer: Option<RepositorySigner>,
}

/// Export a versioned recovery envelope. At least one secret is required so a
/// malformed export cannot produce an empty envelope that looks successful.
pub fn export_envelope(
    repository_id: RepositoryId,
    content_key: Option<&RepositoryKey>,
    signer: Option<&RepositorySigner>,
    recovery_secret: &[u8],
) -> Result<Vec<u8>, MirageError> {
    if recovery_secret.len() < MIN_SECRET_LENGTH || recovery_secret.len() > 4096 {
        return Err(MirageError::invalid_argument(
            "recovery secret length is outside the allowed bounds",
        ));
    }
    if content_key.is_none() && signer.is_none() {
        return Err(MirageError::invalid_argument(
            "a recovery envelope requires at least the content key or signing authority",
        ));
    }
    let mut flags = 0_u16;
    let mut payload = Zeroizing::new(Vec::with_capacity(64));
    if let Some(key) = content_key {
        flags |= FLAG_CONTENT_KEY;
        payload.extend_from_slice(key.secret_bytes().as_ref());
    }
    let signer_public = if let Some(signer) = signer {
        flags |= FLAG_SIGNER;
        payload.extend_from_slice(signer.secret_bytes().as_ref());
        signer.verifier().public_key()
    } else {
        [0_u8; 32]
    };

    let params = KdfParameters::DEFAULT;
    let mut salt = [0_u8; 16];
    let mut nonce = [0_u8; 24];
    getrandom::fill(&mut salt)
        .map_err(|_| MirageError::internal_invariant("OS randomness unavailable"))?;
    getrandom::fill(&mut nonce)
        .map_err(|_| MirageError::internal_invariant("OS randomness unavailable"))?;

    let mut header = Vec::with_capacity(ENVELOPE_HEADER_LENGTH);
    header.extend_from_slice(ENVELOPE_MAGIC);
    header.extend_from_slice(&ENVELOPE_VERSION.to_le_bytes());
    header.extend_from_slice(&flags.to_le_bytes());
    header.push(KDF_ARGON2ID);
    header.extend_from_slice(&params.memory_kib.to_le_bytes());
    header.extend_from_slice(&params.iterations.to_le_bytes());
    header.push(params.lanes);
    header.extend_from_slice(&[0_u8; 2]);
    header.extend_from_slice(repository_id.as_bytes());
    header.extend_from_slice(&signer_public);
    header.extend_from_slice(&salt);
    header.extend_from_slice(&nonce);
    header.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    debug_assert_eq!(header.len(), ENVELOPE_HEADER_LENGTH);

    let key = derive_envelope_key(recovery_secret, &salt, params)?;
    let ciphertext = XChaCha20Poly1305::new(key.as_ref().into())
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: payload.as_ref(),
                aad: &header,
            },
        )
        .map_err(|_| MirageError::internal_invariant("recovery envelope encryption failed"))?;
    let mut out = header;
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

/// Read an envelope's public metadata without the recovery secret. Legacy
/// `MREKv001` signer records are recognized and reported as signer-only.
pub fn inspect_envelope(bytes: &[u8]) -> Result<EnvelopeInspection, MirageError> {
    if bytes.len() >= 8 && &bytes[..8] == MAGIC {
        return Ok(EnvelopeInspection {
            repository_id: RepositoryId::from_bytes([0_u8; 16]),
            kind: EnvelopeKind::SignerOnly,
            has_content_key: false,
            has_signer: true,
            legacy_signer_only: true,
        });
    }
    if bytes.len() < ENVELOPE_HEADER_LENGTH + 16 || bytes.len() > MAX_ENVELOPE_LENGTH {
        return Err(MirageError::integrity_mismatch(
            "recovery envelope length is invalid",
        ));
    }
    if &bytes[..8] != ENVELOPE_MAGIC {
        return Err(MirageError::integrity_mismatch(
            "recovery envelope is not a recognized format",
        ));
    }
    let version = u16::from_le_bytes(bytes[8..10].try_into().unwrap());
    if version != ENVELOPE_VERSION {
        return Err(MirageError::unsupported_layout(
            "unsupported recovery envelope version",
        ));
    }
    let flags = u16::from_le_bytes(bytes[10..12].try_into().unwrap());
    if flags & !(FLAG_CONTENT_KEY | FLAG_SIGNER) != 0 || flags == 0 {
        return Err(MirageError::integrity_mismatch(
            "recovery envelope flags are invalid",
        ));
    }
    let repository_id = RepositoryId::from_bytes(bytes[24..40].try_into().unwrap());
    let has_content_key = flags & FLAG_CONTENT_KEY != 0;
    let has_signer = flags & FLAG_SIGNER != 0;
    Ok(EnvelopeInspection {
        repository_id,
        kind: match (has_content_key, has_signer) {
            (true, _) => EnvelopeKind::Complete,
            (false, true) => EnvelopeKind::SignerOnly,
            _ => EnvelopeKind::Empty,
        },
        has_content_key,
        has_signer,
        legacy_signer_only: false,
    })
}

/// Open a recovery envelope. Returns an error for a tampered envelope, wrong
/// secret, wrong repository, or a legacy signer-only record (which cannot
/// recover encrypted content; it remains readable through `import_signer`).
pub fn open_envelope(
    bytes: &[u8],
    recovery_secret: &[u8],
    expected_repository: Option<RepositoryId>,
) -> Result<RecoveryPayload, MirageError> {
    let inspection = inspect_envelope(bytes)?;
    if inspection.legacy_signer_only {
        return Err(MirageError::unsupported_layout(
            "legacy recovery record contains signing authority only; it cannot recover encrypted content",
        ));
    }
    if let Some(expected) = expected_repository
        && expected != inspection.repository_id
    {
        return Err(MirageError::integrity_mismatch(
            "recovery envelope belongs to a different repository",
        ));
    }
    let flags = u16::from_le_bytes(bytes[10..12].try_into().unwrap());
    let kdf_algorithm = bytes[12];
    if kdf_algorithm != KDF_ARGON2ID {
        return Err(MirageError::unsupported_layout(
            "unsupported recovery envelope KDF",
        ));
    }
    let params = KdfParameters {
        memory_kib: u32::from_le_bytes(bytes[13..17].try_into().unwrap()),
        iterations: u32::from_le_bytes(bytes[17..21].try_into().unwrap()),
        lanes: bytes[21],
    };
    let signer_public: [u8; 32] = bytes[40..72].try_into().unwrap();
    let salt: [u8; 16] = bytes[72..88].try_into().unwrap();
    let nonce: [u8; 24] = bytes[88..112].try_into().unwrap();
    let payload_length = u32::from_le_bytes(bytes[112..116].try_into().unwrap()) as usize;
    let expected_payload =
        (flags & FLAG_CONTENT_KEY != 0) as usize * 32 + (flags & FLAG_SIGNER != 0) as usize * 32;
    if payload_length != expected_payload
        || bytes.len() != ENVELOPE_HEADER_LENGTH + payload_length + 16
    {
        return Err(MirageError::integrity_mismatch(
            "recovery envelope payload length is inconsistent",
        ));
    }
    let key = derive_envelope_key(recovery_secret, &salt, params)?;
    let plaintext = Zeroizing::new(
        XChaCha20Poly1305::new(key.as_ref().into())
            .decrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: &bytes[ENVELOPE_HEADER_LENGTH..],
                    aad: &bytes[..ENVELOPE_HEADER_LENGTH],
                },
            )
            .map_err(|_| {
                MirageError::integrity_mismatch(
                    "recovery secret or envelope integrity check failed",
                )
            })?,
    );
    let mut offset = 0_usize;
    let content_key = if flags & FLAG_CONTENT_KEY != 0 {
        let raw: [u8; 32] = plaintext[offset..offset + 32].try_into().unwrap();
        offset += 32;
        Some(RepositoryKey::from_bytes(raw))
    } else {
        None
    };
    let signer = if flags & FLAG_SIGNER != 0 {
        let signer = RepositorySigner::from_secret(&plaintext[offset..offset + 32])?;
        if signer.verifier().public_key() != signer_public {
            return Err(MirageError::integrity_mismatch(
                "recovered signer does not match the envelope trust root",
            ));
        }
        Some(signer)
    } else {
        None
    };
    Ok(RecoveryPayload {
        repository_id: inspection.repository_id,
        content_key,
        signer,
    })
}

fn derive_envelope_key(
    recovery_secret: &[u8],
    salt: &[u8; 16],
    params: KdfParameters,
) -> Result<Zeroizing<[u8; 32]>, MirageError> {
    if params.memory_kib == 0
        || params.memory_kib > 4 * 1024 * 1024
        || params.iterations == 0
        || params.iterations > 64
        || params.lanes == 0
        || params.lanes > 8
    {
        return Err(MirageError::invalid_argument(
            "recovery envelope KDF parameters are outside safe bounds",
        ));
    }
    let kdf = Argon2::new(
        Algorithm::Argon2id,
        Version::V0x13,
        Params::new(
            params.memory_kib,
            params.iterations,
            u32::from(params.lanes),
            Some(32),
        )
        .map_err(|_| MirageError::invalid_argument("recovery envelope KDF is invalid"))?,
    );
    let mut key = Zeroizing::new([0_u8; 32]);
    kdf.hash_password_into(recovery_secret, salt, key.as_mut())
        .map_err(|_| MirageError::internal_invariant("recovery key derivation failed"))?;
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secret() -> Vec<u8> {
        b"correct horse battery staple".to_vec()
    }

    #[test]
    fn envelope_round_trips_content_key_and_signer() {
        let repository = RepositoryId::from_bytes([7_u8; 16]);
        let content = RepositoryKey::generate().unwrap();
        let signer = RepositorySigner::generate().unwrap();
        let envelope =
            export_envelope(repository, Some(&content), Some(&signer), &secret()).unwrap();
        let inspection = inspect_envelope(&envelope).unwrap();
        assert_eq!(inspection.repository_id, repository);
        assert_eq!(inspection.kind, EnvelopeKind::Complete);
        let payload = open_envelope(&envelope, &secret(), Some(repository)).unwrap();
        assert_eq!(
            payload.content_key.unwrap().secret_bytes().as_ref(),
            content.secret_bytes().as_ref()
        );
        assert_eq!(
            payload.signer.unwrap().verifier().public_key(),
            signer.verifier().public_key()
        );
    }

    #[test]
    fn wrong_secret_wrong_repository_and_tampering_fail() {
        let repository = RepositoryId::from_bytes([9_u8; 16]);
        let content = RepositoryKey::generate().unwrap();
        let envelope = export_envelope(repository, Some(&content), None, &secret()).unwrap();
        assert!(open_envelope(&envelope, b"the wrong secret", Some(repository)).is_err());
        let other = RepositoryId::from_bytes([8_u8; 16]);
        assert!(open_envelope(&envelope, &secret(), Some(other)).is_err());
        let mut tampered = envelope.clone();
        let last = tampered.len() - 1;
        tampered[last] ^= 1;
        assert!(open_envelope(&tampered, &secret(), Some(repository)).is_err());
        let mut truncated = envelope.clone();
        truncated.truncate(truncated.len() - 3);
        assert!(open_envelope(&truncated, &secret(), Some(repository)).is_err());
    }

    #[test]
    fn signer_only_envelope_is_identified_incomplete() {
        let repository = RepositoryId::from_bytes([3_u8; 16]);
        let signer = RepositorySigner::generate().unwrap();
        let envelope = export_envelope(repository, None, Some(&signer), &secret()).unwrap();
        let inspection = inspect_envelope(&envelope).unwrap();
        assert_eq!(inspection.kind, EnvelopeKind::SignerOnly);
        assert!(!inspection.has_content_key);
        let payload = open_envelope(&envelope, &secret(), Some(repository)).unwrap();
        assert!(payload.content_key.is_none());
        assert!(payload.signer.is_some());
    }

    #[test]
    fn legacy_signer_record_is_reported_not_silently_accepted() {
        let signer = RepositorySigner::generate().unwrap();
        let legacy = export_signer(&signer, &secret()).unwrap();
        let inspection = inspect_envelope(&legacy).unwrap();
        assert!(inspection.legacy_signer_only);
        assert_eq!(inspection.kind, EnvelopeKind::SignerOnly);
        assert!(open_envelope(&legacy, &secret(), None).is_err());
        // The legacy path still works for its original purpose.
        let restored = import_signer(&legacy, &secret()).unwrap();
        assert_eq!(
            restored.verifier().public_key(),
            signer.verifier().public_key()
        );
    }

    #[test]
    fn empty_envelope_and_weak_secret_are_rejected() {
        let repository = RepositoryId::from_bytes([1_u8; 16]);
        assert!(export_envelope(repository, None, None, &secret()).is_err());
        let content = RepositoryKey::generate().unwrap();
        assert!(export_envelope(repository, Some(&content), None, b"short").is_err());
    }
}
