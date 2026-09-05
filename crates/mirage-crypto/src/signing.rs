use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use mirage_manifest::{CommitSigner, CommitVerifier, SignatureAlgorithm, SignatureEnvelope};
use mirage_types::MirageError;
use zeroize::Zeroizing;

use crate::key_id::derive_key_id;

pub struct RepositorySigner(SigningKey);

impl RepositorySigner {
    pub fn generate() -> Result<Self, MirageError> {
        let mut secret = Zeroizing::new([0_u8; 32]);
        getrandom::fill(secret.as_mut())
            .map_err(|_| MirageError::internal_invariant("OS randomness unavailable"))?;
        Ok(Self(SigningKey::from_bytes(&secret)))
    }

    pub fn from_secret(secret: &[u8]) -> Result<Self, MirageError> {
        let bytes: [u8; 32] = secret
            .try_into()
            .map_err(|_| MirageError::invalid_argument("Ed25519 secret must be 32 bytes"))?;
        Ok(Self(SigningKey::from_bytes(&bytes)))
    }

    #[must_use]
    pub fn secret_bytes(&self) -> Zeroizing<[u8; 32]> {
        Zeroizing::new(self.0.to_bytes())
    }
    #[must_use]
    pub fn verifier(&self) -> RepositoryVerifier {
        RepositoryVerifier(self.0.verifying_key())
    }
}

impl CommitSigner for RepositorySigner {
    fn algorithm(&self) -> SignatureAlgorithm {
        SignatureAlgorithm::Ed25519
    }
    fn key_id(&self) -> [u8; 16] {
        derive_key_id(self.0.verifying_key().as_bytes())
    }
    fn sign(&self, body: &[u8]) -> Result<Vec<u8>, MirageError> {
        Ok(self.0.sign(body).to_bytes().to_vec())
    }
}

#[derive(Clone, Copy)]
pub struct RepositoryVerifier(VerifyingKey);
impl RepositoryVerifier {
    pub fn from_public_key(bytes: [u8; 32]) -> Result<Self, MirageError> {
        VerifyingKey::from_bytes(&bytes)
            .map(Self)
            .map_err(|_| MirageError::invalid_argument("invalid Ed25519 public key"))
    }
    #[must_use]
    pub fn public_key(&self) -> [u8; 32] {
        self.0.to_bytes()
    }
}
impl CommitVerifier for RepositoryVerifier {
    fn verify(&self, body: &[u8], envelope: &SignatureEnvelope) -> Result<(), MirageError> {
        if envelope.algorithm != SignatureAlgorithm::Ed25519
            || envelope.key_id != derive_key_id(self.0.as_bytes())
        {
            return Err(MirageError::integrity_mismatch(
                "commit signer is not trusted",
            ));
        }
        let signature = Signature::try_from(envelope.signature.as_slice())
            .map_err(|_| MirageError::integrity_mismatch("invalid Ed25519 signature"))?;
        self.0
            .verify(body, &signature)
            .map_err(|_| MirageError::integrity_mismatch("commit signature verification failed"))
    }
}
