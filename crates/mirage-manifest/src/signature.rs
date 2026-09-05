use mirage_types::MirageError;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignatureAlgorithm {
    Ed25519,
    TestOnlyBlake3Keyed,
}

impl SignatureAlgorithm {
    #[must_use]
    pub const fn code(self) -> u16 {
        match self {
            Self::Ed25519 => 1,
            Self::TestOnlyBlake3Keyed => u16::MAX,
        }
    }

    pub(crate) fn from_code(code: u16) -> Result<Self, MirageError> {
        match code {
            1 => Ok(Self::Ed25519),
            u16::MAX => Ok(Self::TestOnlyBlake3Keyed),
            _ => Err(MirageError::manifest_invalid(
                "unknown commit signature algorithm",
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignatureEnvelope {
    pub algorithm: SignatureAlgorithm,
    pub key_id: [u8; 16],
    pub signature: Vec<u8>,
}

impl SignatureEnvelope {
    pub fn validate_shape(&self) -> Result<(), MirageError> {
        if self.signature.is_empty() || self.signature.len() > 128 {
            return Err(MirageError::manifest_invalid(
                "commit signature length is outside 1..=128 bytes",
            ));
        }
        Ok(())
    }
}

pub trait CommitSigner: Send + Sync {
    fn algorithm(&self) -> SignatureAlgorithm;
    fn key_id(&self) -> [u8; 16];
    fn sign(&self, canonical_unsigned_body: &[u8]) -> Result<Vec<u8>, MirageError>;
}

pub trait CommitVerifier: Send + Sync {
    fn verify(
        &self,
        canonical_unsigned_body: &[u8],
        signature: &SignatureEnvelope,
    ) -> Result<(), MirageError>;
}

#[cfg(feature = "test-signing")]
#[derive(Clone)]
pub struct InMemoryTestSigner {
    key_id: [u8; 16],
    key: [u8; 32],
}

#[cfg(feature = "test-signing")]
impl InMemoryTestSigner {
    #[must_use]
    pub const fn new(key_id: [u8; 16], key: [u8; 32]) -> Self {
        Self { key_id, key }
    }
}

#[cfg(feature = "test-signing")]
impl CommitSigner for InMemoryTestSigner {
    fn algorithm(&self) -> SignatureAlgorithm {
        SignatureAlgorithm::TestOnlyBlake3Keyed
    }

    fn key_id(&self) -> [u8; 16] {
        self.key_id
    }

    fn sign(&self, canonical_unsigned_body: &[u8]) -> Result<Vec<u8>, MirageError> {
        Ok(blake3::keyed_hash(&self.key, canonical_unsigned_body)
            .as_bytes()
            .to_vec())
    }
}

#[cfg(feature = "test-signing")]
impl CommitVerifier for InMemoryTestSigner {
    fn verify(
        &self,
        canonical_unsigned_body: &[u8],
        signature: &SignatureEnvelope,
    ) -> Result<(), MirageError> {
        signature.validate_shape()?;
        if signature.algorithm != SignatureAlgorithm::TestOnlyBlake3Keyed
            || signature.key_id != self.key_id
        {
            return Err(MirageError::integrity_mismatch(
                "commit signature key or algorithm does not match verifier",
            ));
        }
        let expected = blake3::keyed_hash(&self.key, canonical_unsigned_body);
        if !constant_time_equal(expected.as_bytes(), &signature.signature) {
            return Err(MirageError::integrity_mismatch(
                "commit signature verification failed",
            ));
        }
        Ok(())
    }
}

#[cfg(feature = "test-signing")]
fn constant_time_equal(expected: &[u8], actual: &[u8]) -> bool {
    if expected.len() != actual.len() {
        return false;
    }
    let mut difference = 0_u8;
    for (left, right) in expected.iter().zip(actual) {
        difference |= left ^ right;
    }
    difference == 0
}
