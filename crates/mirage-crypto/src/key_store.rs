use crate::{
    dpapi::{self, ProtectionScope},
    signing::{RepositorySigner, RepositoryVerifier},
};
use mirage_types::{MirageError, MirageErrorKind};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
};
use zeroize::Zeroizing;

const MAGIC: &[u8; 8] = b"MKEYv001";

pub fn save_signer(
    path: &Path,
    signer: &RepositorySigner,
    scope: ProtectionScope,
) -> Result<(), MirageError> {
    let public = signer.verifier().public_key();
    let protected = dpapi::protect(signer.secret_bytes().as_ref(), &public, scope)?;
    let length = u32::try_from(protected.len())
        .map_err(|_| MirageError::invalid_argument("protected key is oversized"))?;
    let temp = path.with_extension("partial");
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .map_err(io)?;
    file.write_all(MAGIC)
        .and_then(|_| file.write_all(&public))
        .and_then(|_| file.write_all(&length.to_le_bytes()))
        .and_then(|_| file.write_all(&protected))
        .and_then(|_| file.sync_all())
        .map_err(io)?;
    if path.exists() {
        let _ = fs::remove_file(&temp);
        return Err(MirageError::repository_conflict(
            "key record already exists",
        ));
    }
    fs::rename(temp, path).map_err(io)
}

pub fn load_signer(path: &Path) -> Result<RepositorySigner, MirageError> {
    let bytes = fs::read(path).map_err(io)?;
    if bytes.len() < 44 || &bytes[..8] != MAGIC {
        return Err(MirageError::integrity_mismatch(
            "key record header is invalid",
        ));
    }
    let public: [u8; 32] = bytes[8..40].try_into().unwrap();
    let length = u32::from_le_bytes(bytes[40..44].try_into().unwrap()) as usize;
    if length != bytes.len() - 44 {
        return Err(MirageError::integrity_mismatch(
            "key record length is invalid",
        ));
    }
    let secret: Zeroizing<Vec<u8>> = dpapi::unprotect(&bytes[44..], &public)?;
    let signer = RepositorySigner::from_secret(secret.as_ref())?;
    if signer.verifier().public_key() != public {
        return Err(MirageError::integrity_mismatch(
            "protected key does not match trust record",
        ));
    }
    Ok(signer)
}

pub fn load_trust_root(path: &Path) -> Result<RepositoryVerifier, MirageError> {
    let bytes = fs::read(path).map_err(io)?;
    if bytes.len() < 40 || &bytes[..8] != MAGIC {
        return Err(MirageError::integrity_mismatch(
            "key trust record is invalid",
        ));
    }
    RepositoryVerifier::from_public_key(bytes[8..40].try_into().unwrap())
}

fn io(error: std::io::Error) -> MirageError {
    MirageError::new(
        MirageErrorKind::Io,
        MirageErrorKind::Io.default_code(),
        "key store I/O failed",
    )
    .with_source(error)
}
