use std::path::Path;

use mirage_crypto::{
    dpapi::ProtectionScope,
    key_store,
    recovery::{EnvelopeKind, RecoveryPayload, export_envelope, inspect_envelope, open_envelope},
    repository_key_store::{inspect_repository_key, load_repository_key, save_repository_key},
};
use mirage_types::{MirageError, RepositoryId};
use serde_json::json;
use zeroize::Zeroizing;

use crate::output;

/// The durable marker written beside a repository key record after a complete
/// recovery envelope has been verified against this exact content key. The
/// original-data reclamation gate requires it for encrypted repositories.
const VERIFIED_RECORD: &str = "recovery-verified.json";

pub fn export(
    import: &Path,
    envelope: &Path,
    secret_file: &Path,
    signer_store: Option<&Path>,
    json: bool,
) -> Result<(), MirageError> {
    let key_path = import.join("repository-key.dpapi");
    let repository_id = inspect_repository_key(&key_path)?;
    let content_key = load_repository_key(&key_path, repository_id)?;
    let signer = signer_store.map(key_store::load_signer).transpose()?;
    let secret = read_secret(secret_file)?;
    let bytes = export_envelope(repository_id, Some(&content_key), signer.as_ref(), &secret)?;
    if envelope.exists() {
        return Err(MirageError::repository_conflict(
            "recovery envelope already exists; choose a new path rather than overwriting recovery material",
        ));
    }
    mirage_crypto::durable_file::write_atomic(envelope, &bytes)?;
    mirage_crypto::file_acl::restrict_to_current_user_system_admins(envelope)?;
    emit(
        json,
        json!({
            "report_version": 1,
            "exported": true,
            "repository_id": repository_id.to_string(),
            "has_content_key": true,
            "has_signer_authority": signer.is_some(),
            "envelope_sha256": envelope_sha256(&bytes),
        }),
        "recovery envelope exported; store it and the secret in separate safe places".to_owned(),
    )
}

pub fn verify(
    envelope: &Path,
    secret_file: &Path,
    import: Option<&Path>,
    repository_id: Option<RepositoryId>,
    json: bool,
) -> Result<(), MirageError> {
    let bytes = std::fs::read(envelope).map_err(MirageError::from)?;
    let inspection = inspect_envelope(&bytes)?;
    if inspection.legacy_signer_only {
        return emit(
            json,
            json!({
                "report_version": 1,
                "verified": false,
                "complete": false,
                "reason": "legacy signer-only recovery record; it cannot recover encrypted content",
            }),
            "envelope is a legacy signer-only record: incomplete for encrypted recovery".to_owned(),
        );
    }
    let expected = match (repository_id, import) {
        (Some(id), _) => Some(id),
        (None, Some(dir)) => Some(inspect_repository_key(&dir.join("repository-key.dpapi"))?),
        (None, None) => None,
    };
    let secret = read_secret(secret_file)?;
    let payload = open_envelope(&bytes, &secret, expected)?;
    let mut key_matches: Option<bool> = None;
    if let Some(dir) = import {
        let local = load_repository_key(&dir.join("repository-key.dpapi"), payload.repository_id)?;
        let recovered = payload.content_key.as_ref().ok_or_else(|| {
            MirageError::integrity_mismatch(
                "recovery envelope has no content key; recovery is incomplete",
            )
        })?;
        if recovered.secret_bytes().as_ref() != local.secret_bytes().as_ref() {
            return Err(MirageError::integrity_mismatch(
                "recovery envelope decrypts a different content key than this repository uses",
            ));
        }
        key_matches = Some(true);
        if inspection.has_content_key {
            write_verified_record(dir, &payload, &bytes)?;
        }
    }
    emit(
        json,
        json!({
            "report_version": 1,
            "verified": true,
            "complete": inspection.kind == EnvelopeKind::Complete,
            "repository_id": payload.repository_id.to_string(),
            "has_content_key": inspection.has_content_key,
            "has_signer_authority": inspection.has_signer,
            "content_key_matches_repository": key_matches,
            "verification_recorded": import.is_some() && inspection.has_content_key,
            "writer_authority_transferred": false,
        }),
        "recovery envelope verified".to_owned(),
    )
}

pub fn import(
    envelope: &Path,
    secret_file: &Path,
    destination: &Path,
    json: bool,
) -> Result<(), MirageError> {
    let bytes = std::fs::read(envelope).map_err(MirageError::from)?;
    let secret = read_secret(secret_file)?;
    let payload = open_envelope(&bytes, &secret, None)?;
    std::fs::create_dir_all(destination).map_err(MirageError::from)?;
    let mut content_key_installed = false;
    if let Some(key) = payload.content_key.as_ref() {
        save_repository_key(
            &destination.join("repository-key.dpapi"),
            payload.repository_id,
            key,
            ProtectionScope::LocalMachine,
        )?;
        content_key_installed = true;
    }
    let mut signer_installed = false;
    if let Some(signer) = payload.signer.as_ref() {
        key_store::save_signer(
            &destination.join("repository-signer.dpapi"),
            signer,
            ProtectionScope::CurrentUser,
        )?;
        signer_installed = true;
    }
    emit(
        json,
        json!({
            "report_version": 1,
            "imported": true,
            "repository_id": payload.repository_id.to_string(),
            "content_key_installed": content_key_installed,
            "signer_installed": signer_installed,
            // A restored machine can read content; resuming the old writer
            // lineage is a separate deliberate operation.
            "writer_authority_transferred": false,
        }),
        "recovery envelope imported into a fresh protected key store".to_owned(),
    )
}

/// Opens a recovery envelope and returns its repository content key,
/// requiring it to be present and bound to `expected`. Shared by
/// `recovery import` and `repo extract --envelope`.
pub(crate) fn envelope_content_key(
    envelope: &Path,
    secret_file: &Path,
    expected: RepositoryId,
) -> Result<mirage_crypto::aead::RepositoryKey, MirageError> {
    let bytes = std::fs::read(envelope).map_err(MirageError::from)?;
    let secret = read_secret(secret_file)?;
    let payload = open_envelope(&bytes, &secret, Some(expected))?;
    payload.content_key.ok_or_else(|| {
        MirageError::integrity_mismatch(
            "recovery envelope has no content key; recovery is incomplete",
        )
    })
}

fn write_verified_record(
    import: &Path,
    payload: &RecoveryPayload,
    envelope: &[u8],
) -> Result<(), MirageError> {
    let content_key = payload.content_key.as_ref().ok_or_else(|| {
        MirageError::internal_invariant("verified record requires the content key")
    })?;
    let record = json!({
        "format_version": 1,
        "repository_id": payload.repository_id.to_string(),
        "content_key_blake3": blake3::hash(content_key.secret_bytes().as_ref()).to_hex().to_string(),
        "has_signer_authority": payload.signer.is_some(),
        "envelope_sha256": envelope_sha256(envelope),
        "verified_at_ns": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos() as i64)
            .unwrap_or(0),
    });
    mirage_crypto::durable_file::write_atomic(
        &import.join(VERIFIED_RECORD),
        serde_json::to_vec_pretty(&record)
            .map_err(|_| MirageError::internal_invariant("verification record failed"))?
            .as_slice(),
    )
}

/// The field is named SHA-256, so the digest really is SHA-256 — a BLAKE3
/// value under this name would mislead external verification tooling.
fn envelope_sha256(bytes: &[u8]) -> String {
    use sha2::Digest;
    format!("{:x}", sha2::Sha256::digest(bytes))
}

fn read_secret(path: &Path) -> Result<Zeroizing<Vec<u8>>, MirageError> {
    let mut secret = Zeroizing::new(std::fs::read(path).map_err(MirageError::from)?);
    while matches!(secret.last(), Some(b'\n' | b'\r')) {
        secret.pop();
    }
    if secret.is_empty() {
        return Err(MirageError::invalid_argument(
            "recovery secret file is empty",
        ));
    }
    Ok(secret)
}

fn emit(json: bool, data: serde_json::Value, human: String) -> Result<(), MirageError> {
    if json {
        output::emit_success(&data)
    } else {
        println!("{human}");
        Ok(())
    }
}

// The recovery flow exercises real DPAPI protection, so it is Windows-only.
#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use mirage_crypto::aead::RepositoryKey;

    #[test]
    fn export_verify_import_round_trip() {
        let directory = tempfile::tempdir().unwrap();
        let import_dir = directory.path().join("import");
        let restored = directory.path().join("restored");
        std::fs::create_dir_all(&import_dir).unwrap();
        let repository_id = RepositoryId::from_bytes([11_u8; 16]);
        let key = RepositoryKey::generate().unwrap();
        save_repository_key(
            &import_dir.join("repository-key.dpapi"),
            repository_id,
            &key,
            ProtectionScope::CurrentUser,
        )
        .unwrap();
        let secret_file = directory.path().join("secret.txt");
        std::fs::write(&secret_file, b"an honest test secret\n").unwrap();
        let envelope = directory.path().join("repository.mrecv");

        export(&import_dir, &envelope, &secret_file, None, true).unwrap();
        // Envelopes are immutable; exporting over one must fail.
        assert!(export(&import_dir, &envelope, &secret_file, None, true).is_err());

        verify(&envelope, &secret_file, Some(&import_dir), None, true).unwrap();
        let record = import_dir.join(VERIFIED_RECORD);
        assert!(record.exists());
        let wrong_secret = directory.path().join("wrong.txt");
        std::fs::write(&wrong_secret, b"not the real secret").unwrap();
        assert!(verify(&envelope, &wrong_secret, Some(&import_dir), None, true).is_err());

        import(&envelope, &secret_file, &restored, true).unwrap();
        let restored_key =
            load_repository_key(&restored.join("repository-key.dpapi"), repository_id).unwrap();
        assert_eq!(
            restored_key.secret_bytes().as_ref(),
            key.secret_bytes().as_ref()
        );
        // A second import must not overwrite an existing key record.
        assert!(import(&envelope, &secret_file, &restored, true).is_err());
    }
}
