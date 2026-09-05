use bytes::Bytes;
use mirage_backend::{ObjectBackend, ObjectKind, RemoteObjectRef, UploadSource};
use mirage_manifest::{
    COMMIT_FORMAT_VERSION, CommitSigner, RepositoryCommit, RepositoryManifest, UnsignedCommitBody,
    commit_hash, decode_commit_bounded, encode_commit, encode_manifest, manifest_hash, sign_commit,
};
use mirage_pack::CompletedPack;
use mirage_types::{CommitHash, ContentHash, DeviceId, ManifestHash, MirageError, UpdateId};
use tokio_util::sync::CancellationToken;

use crate::object_plan::{pack_set_hash, upload_source_from_file};

#[derive(Debug, Clone)]
pub struct PublishedGeneration {
    pub packs: Vec<RemoteObjectRef>,
    pub published_manifest: RepositoryManifest,
    pub manifest: RemoteObjectRef,
    pub commit: RemoteObjectRef,
    pub commit_hash: CommitHash,
    pub commit_body: RepositoryCommit,
}

pub async fn publish_base_generation(
    backend: &dyn ObjectBackend,
    packs: &[CompletedPack],
    manifest: &RepositoryManifest,
    signer: &dyn CommitSigner,
) -> Result<PublishedGeneration, MirageError> {
    let cancel = CancellationToken::new();
    let mut remote_packs = Vec::with_capacity(packs.len());
    for pack in packs {
        let uploaded = backend
            .put_immutable(
                ObjectKind::Pack,
                upload_source_from_file(&pack.path)?,
                pack.content_hash,
                cancel.child_token(),
            )
            .await?;
        let stat = backend.stat(&uploaded).await?;
        if stat.content_hash != pack.content_hash || stat.byte_length.as_u64() != pack.byte_length {
            return Err(MirageError::integrity_mismatch(
                "published pack identity failed verification",
            ));
        }
        remote_packs.push(uploaded);
    }

    let mut published_manifest = manifest.clone();
    for location in &mut published_manifest.remote_locations {
        location.object = remote_packs
            .iter()
            .find(|object| object.content_hash == location.object.content_hash)
            .cloned()
            .ok_or_else(|| {
                MirageError::integrity_mismatch(
                    "manifest references a pack absent from the publication plan",
                )
            })?;
    }
    let manifest_bytes = encode_manifest(&published_manifest)?;
    let manifest_hash = manifest_hash(&published_manifest)?;
    let manifest_content_hash = ContentHash::from_bytes(*manifest_hash.as_bytes());
    let manifest_object = backend
        .put_immutable(
            ObjectKind::Manifest,
            UploadSource::from_bytes(Bytes::from(manifest_bytes)),
            manifest_content_hash,
            cancel.child_token(),
        )
        .await?;
    let manifest_stat = backend.stat(&manifest_object).await?;
    if manifest_stat.content_hash != manifest_content_hash {
        return Err(MirageError::integrity_mismatch(
            "published manifest identity failed verification",
        ));
    }

    let body = UnsignedCommitBody {
        format_version: COMMIT_FORMAT_VERSION,
        repository_id: manifest.repository_id,
        sequence: 0,
        parent_commit: None,
        manifest_hash: ManifestHash::from_bytes(*manifest_hash.as_bytes()),
        manifest_object: manifest_object.clone(),
        referenced_pack_set_hash: pack_set_hash(
            remote_packs
                .iter()
                .map(|object| object.content_hash)
                .collect(),
        ),
        created_utc_ns: 0,
        writer_device_id: DeviceId::from_bytes([0; 16]),
        update_journal_id: None,
    };
    let commit_body = sign_commit(body, signer)?;
    let commit_bytes = encode_commit(&commit_body)?;
    let commit_hash = commit_hash(&commit_body)?;
    let commit_content_hash = ContentHash::from_bytes(*commit_hash.as_bytes());
    let commit = backend
        .put_immutable(
            ObjectKind::Commit,
            UploadSource::from_bytes(Bytes::from(commit_bytes.clone())),
            commit_content_hash,
            cancel,
        )
        .await?;
    let downloaded = read_complete(backend, &commit, 64 * 1024).await?;
    if downloaded.as_ref() != commit_bytes || decode_commit_bounded(&downloaded)? != commit_body {
        return Err(MirageError::integrity_mismatch(
            "published commit failed readback verification",
        ));
    }
    Ok(PublishedGeneration {
        packs: remote_packs,
        published_manifest,
        manifest: manifest_object,
        commit,
        commit_hash,
        commit_body,
    })
}

pub async fn publish_successor_generation(
    backend: &dyn ObjectBackend,
    manifest: &RepositoryManifest,
    parent: &RepositoryCommit,
    signer: &dyn CommitSigner,
    update_id: UpdateId,
) -> Result<PublishedGeneration, MirageError> {
    if parent.body.repository_id != manifest.repository_id
        || manifest.generation_id.as_u64() <= parent.body.sequence
    {
        return Err(MirageError::repository_conflict(
            "successor manifest does not advance its parent repository",
        ));
    }
    let mut remote_packs = manifest
        .remote_locations
        .iter()
        .map(|location| location.object.clone())
        .collect::<Vec<_>>();
    remote_packs.sort_by_key(|object| object.content_hash);
    remote_packs.dedup_by_key(|object| object.content_hash);
    for pack in &remote_packs {
        if pack.kind != ObjectKind::Pack {
            return Err(MirageError::manifest_invalid(
                "successor manifest references a non-pack page object",
            ));
        }
        let stat = backend.stat(pack).await?;
        if stat.content_hash != pack.content_hash || stat.byte_length != pack.byte_length {
            return Err(MirageError::integrity_mismatch(
                "successor pack identity failed verification",
            ));
        }
    }
    let manifest_bytes = encode_manifest(manifest)?;
    let manifest_hash = manifest_hash(manifest)?;
    let manifest_content_hash = ContentHash::from_bytes(*manifest_hash.as_bytes());
    let cancel = CancellationToken::new();
    let manifest_object = backend
        .put_immutable(
            ObjectKind::Manifest,
            UploadSource::from_bytes(Bytes::from(manifest_bytes)),
            manifest_content_hash,
            cancel.child_token(),
        )
        .await?;
    let sequence = parent
        .body
        .sequence
        .checked_add(1)
        .ok_or_else(|| MirageError::invalid_argument("commit sequence overflows"))?;
    let body = UnsignedCommitBody {
        format_version: COMMIT_FORMAT_VERSION,
        repository_id: manifest.repository_id,
        sequence,
        parent_commit: Some(commit_hash(parent)?),
        manifest_hash: ManifestHash::from_bytes(*manifest_hash.as_bytes()),
        manifest_object: manifest_object.clone(),
        referenced_pack_set_hash: pack_set_hash(
            remote_packs
                .iter()
                .map(|object| object.content_hash)
                .collect(),
        ),
        created_utc_ns: 0,
        writer_device_id: DeviceId::from_bytes([0; 16]),
        update_journal_id: Some(update_id),
    };
    let commit_body = sign_commit(body, signer)?;
    let commit_bytes = encode_commit(&commit_body)?;
    let commit_hash = commit_hash(&commit_body)?;
    let commit = backend
        .put_immutable(
            ObjectKind::Commit,
            UploadSource::from_bytes(Bytes::from(commit_bytes.clone())),
            ContentHash::from_bytes(*commit_hash.as_bytes()),
            cancel,
        )
        .await?;
    let downloaded = read_complete(backend, &commit, 64 * 1024).await?;
    if downloaded.as_ref() != commit_bytes || decode_commit_bounded(&downloaded)? != commit_body {
        return Err(MirageError::integrity_mismatch(
            "published successor commit failed readback verification",
        ));
    }
    Ok(PublishedGeneration {
        packs: remote_packs,
        published_manifest: manifest.clone(),
        manifest: manifest_object,
        commit,
        commit_hash,
        commit_body,
    })
}

pub(crate) async fn read_complete(
    backend: &dyn ObjectBackend,
    object: &RemoteObjectRef,
    maximum: u64,
) -> Result<Bytes, MirageError> {
    if object.byte_length.as_u64() > maximum {
        return Err(MirageError::manifest_invalid(
            "immutable metadata object exceeds its read bound",
        ));
    }
    let range = mirage_types::CheckedRange::new(0, object.byte_length.as_u64())?;
    Ok(backend
        .read_range(
            object,
            range,
            mirage_backend::FetchClass::MandatoryAdmission,
            CancellationToken::new(),
        )
        .await?
        .collect_bounded(maximum)
        .await?)
}
