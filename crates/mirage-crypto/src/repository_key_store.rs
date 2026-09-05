use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
};

use mirage_types::{MirageError, MirageErrorKind, RepositoryId};

use crate::{
    aead::RepositoryKey,
    dpapi::{self, ProtectionScope},
    file_acl::restrict_to_current_user_system_admins,
};

const MAGIC: &[u8; 8] = b"MREPOK01";
const HEADER_LENGTH: usize = 28;

pub fn save_repository_key(
    path: &Path,
    repository_id: RepositoryId,
    key: &RepositoryKey,
    scope: ProtectionScope,
) -> Result<(), MirageError> {
    let entropy = entropy(repository_id);
    let protected = dpapi::protect(key.secret_bytes().as_ref(), &entropy, scope)?;
    let length = u32::try_from(protected.len())
        .map_err(|_| MirageError::invalid_argument("protected repository key is oversized"))?;
    let temporary = path.with_extension("partial");
    if temporary.exists() {
        fs::remove_file(&temporary).map_err(io)?;
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(io)?;
    if let Err(error) = restrict_to_current_user_system_admins(&temporary) {
        drop(file);
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    let result = file
        .write_all(MAGIC)
        .and_then(|_| file.write_all(repository_id.as_bytes()))
        .and_then(|_| file.write_all(&length.to_le_bytes()))
        .and_then(|_| file.write_all(&protected))
        .and_then(|_| file.sync_all());
    if let Err(error) = result {
        let _ = fs::remove_file(&temporary);
        return Err(io(error));
    }
    if path.exists() {
        let _ = fs::remove_file(&temporary);
        return Err(MirageError::repository_conflict(
            "repository key record already exists",
        ));
    }
    fs::rename(temporary, path).map_err(io)
}

pub fn load_repository_key(
    path: &Path,
    expected_repository_id: RepositoryId,
) -> Result<RepositoryKey, MirageError> {
    let bytes = fs::read(path).map_err(io)?;
    if bytes.len() < HEADER_LENGTH || &bytes[..8] != MAGIC {
        return Err(MirageError::integrity_mismatch(
            "repository key record header is invalid",
        ));
    }
    let repository_id =
        RepositoryId::from_bytes(bytes[8..24].try_into().map_err(|_| {
            MirageError::integrity_mismatch("repository key identity is truncated")
        })?);
    if repository_id != expected_repository_id {
        return Err(MirageError::integrity_mismatch(
            "repository key record belongs to a different repository",
        ));
    }
    let length = u32::from_le_bytes(
        bytes[24..28]
            .try_into()
            .map_err(|_| MirageError::integrity_mismatch("repository key length is truncated"))?,
    ) as usize;
    if length != bytes.len() - HEADER_LENGTH {
        return Err(MirageError::integrity_mismatch(
            "repository key record length is invalid",
        ));
    }
    let secret = dpapi::unprotect(&bytes[HEADER_LENGTH..], &entropy(repository_id))?;
    let raw: [u8; 32] = secret
        .as_slice()
        .try_into()
        .map_err(|_| MirageError::integrity_mismatch("repository key length is invalid"))?;
    Ok(RepositoryKey::from_bytes(raw))
}

fn entropy(repository_id: RepositoryId) -> [u8; 48] {
    let mut value = [0_u8; 48];
    value[..32].copy_from_slice(blake3::hash(b"MirageSSD repository content key v1").as_bytes());
    value[32..].copy_from_slice(repository_id.as_bytes());
    value
}

fn io(error: std::io::Error) -> MirageError {
    MirageError::new(
        MirageErrorKind::Io,
        MirageErrorKind::Io.default_code(),
        "repository key store I/O failed",
    )
    .with_source(error)
}
