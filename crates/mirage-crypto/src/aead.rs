use chacha20poly1305::{
    XChaCha20Poly1305, XNonce,
    aead::{Aead, KeyInit, Payload},
};
use mirage_types::MirageError;
use zeroize::Zeroizing;

pub const NONCE_LENGTH: usize = 24;
pub struct RepositoryKey(Zeroizing<[u8; 32]>);
impl RepositoryKey {
    pub fn generate() -> Result<Self, MirageError> {
        let mut key = Zeroizing::new([0_u8; 32]);
        getrandom::fill(key.as_mut())
            .map_err(|_| MirageError::internal_invariant("OS randomness unavailable"))?;
        Ok(Self(key))
    }
    #[must_use]
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(Zeroizing::new(bytes))
    }
    #[must_use]
    pub fn secret_bytes(&self) -> Zeroizing<[u8; 32]> {
        Zeroizing::new(*self.0)
    }
    pub fn seal(
        &self,
        plaintext: &[u8],
        aad: &[u8],
    ) -> Result<([u8; NONCE_LENGTH], Vec<u8>), MirageError> {
        let mut nonce = [0_u8; NONCE_LENGTH];
        getrandom::fill(&mut nonce)
            .map_err(|_| MirageError::internal_invariant("OS randomness unavailable"))?;
        let ciphertext = XChaCha20Poly1305::new(self.0.as_ref().into())
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: plaintext,
                    aad,
                },
            )
            .map_err(|_| MirageError::internal_invariant("page encryption failed"))?;
        Ok((nonce, ciphertext))
    }
    pub fn open(
        &self,
        nonce: &[u8; NONCE_LENGTH],
        ciphertext: &[u8],
        aad: &[u8],
    ) -> Result<Vec<u8>, MirageError> {
        XChaCha20Poly1305::new(self.0.as_ref().into())
            .decrypt(
                XNonce::from_slice(nonce),
                Payload {
                    msg: ciphertext,
                    aad,
                },
            )
            .map_err(|_| MirageError::integrity_mismatch("encrypted page authentication failed"))
    }
}
