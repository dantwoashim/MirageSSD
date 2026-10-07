//! Drive manifest loading and backend construction.

use super::*;

pub(super) const DRIVE_MANIFEST: &str = "drive-manifest.cbor";

pub(super) fn active_generation(
    database: &Database,
    repository_id: RepositoryId,
) -> Result<mirage_db::ActiveGeneration, MirageError> {
    database
        .load_active_generation(repository_id)?
        .ok_or_else(|| MirageError::invalid_argument("repository has no active generation"))
}

pub(crate) fn load_drive_manifest(
    config: &RuntimeConfig,
    local_manifest: &RepositoryManifest,
) -> Result<RepositoryManifest, MirageError> {
    let path = config.import_root.join(DRIVE_MANIFEST);
    let manifest = decode_manifest_bounded(
        &bounded_read(&path, DecodeLimits::default().max_input_bytes)?,
        DecodeLimits::default(),
    )?;
    validate_drive_manifest(local_manifest, &manifest)?;
    Ok(manifest)
}

pub(super) fn validate_drive_manifest(
    local: &RepositoryManifest,
    drive: &RepositoryManifest,
) -> Result<(), MirageError> {
    if local.remote_locations.len() != drive.remote_locations.len() {
        return Err(MirageError::integrity_mismatch(
            "Drive publication location count differs from the verified local manifest",
        ));
    }
    for (local_location, drive_location) in
        local.remote_locations.iter().zip(&drive.remote_locations)
    {
        if drive_location.object.backend_id.as_str() != "drive"
            || drive_location.object.kind != mirage_backend::ObjectKind::Pack
            || drive_location.object.content_hash != local_location.object.content_hash
            || drive_location.object.byte_length != local_location.object.byte_length
            || drive_location.offset != local_location.offset
            || drive_location.encoded_length != local_location.encoded_length
            || drive_location.codec != local_location.codec
        {
            return Err(MirageError::integrity_mismatch(
                "Drive publication differs from the verified local pack layout",
            ));
        }
    }
    let mut normalized = drive.clone();
    normalized.remote_locations = local.remote_locations.clone();
    if &normalized != local {
        return Err(MirageError::integrity_mismatch(
            "Drive publication changes immutable repository content",
        ));
    }
    Ok(())
}

pub(crate) fn drive_backend(
    repository_id: RepositoryId,
    access_token: &str,
) -> Result<(Arc<dyn ObjectBackend>, Arc<RetryingHttpTransport>), MirageError> {
    let native = Arc::new(NativeHttpTransport::new()?);
    let retrying = Arc::new(RetryingHttpTransport::new(native, 5)?);
    let backend = DriveObjectBackend::new(
        retrying.clone(),
        Zeroizing::new(access_token.to_owned()),
        repository_id,
    )?;
    Ok((Arc::new(backend), retrying))
}

pub(super) fn local_commit_hash(
    manifest: &RepositoryManifest,
    import_root: &Path,
) -> Result<CommitHash, MirageError> {
    let mut objects = BTreeSet::new();
    for location in &manifest.remote_locations {
        objects.insert(location.object.provider_object_id.as_str().to_owned());
    }
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"MirageSSD/local-verified-generation/v1\0");
    hasher.update(manifest_hash(manifest)?.as_bytes());
    for object in objects {
        let reader = PackReader::open_verified(&import_root.join(&object))?;
        hasher.update(reader.content_hash().as_bytes());
    }
    Ok(CommitHash::from_bytes(*hasher.finalize().as_bytes()))
}
