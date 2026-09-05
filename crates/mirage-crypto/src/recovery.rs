use crate::signing::{RepositorySigner, RepositoryVerifier};
use argon2::Argon2;
use chacha20poly1305::{
    XChaCha20Poly1305, XNonce,
    aead::{Aead, KeyInit, Payload},
};
use mirage_types::MirageError;
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
