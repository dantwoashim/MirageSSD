use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    fs,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use bytes::Bytes;
use mirage_backend::{
    BackendError, BackendErrorClass, BackendId, DeletionProof, FetchClass, ObjectBackend,
    ObjectKind, ProviderObjectId, RemoteObjectRef, UploadSource,
};
use mirage_backend_drive::{
    DriveObjectBackend, HttpRequest, HttpResponse, HttpTransport, RetryingHttpTransport,
    resumable::{self, ResumableSession},
};
use mirage_crypto::{
    aead::RepositoryKey,
    dpapi::ProtectionScope,
    key_store::{load_signer, save_signer},
    repository_key_store::{load_repository_key, save_repository_key},
    signing::RepositorySigner,
};
use mirage_engine::{
    RecoveryHints, publish_base_generation, publish_successor_generation, recover_repository,
    recover_repository_with_hints,
    update::{ManifestOverlay, StagedPageMapping, build_updated_manifest},
};
use mirage_manifest::{Codec, FileClass, ManifestBuilder, ManifestFile, ManifestPage};
use mirage_pack::{
    CompletedPack, EncryptedFrameAad, PackEncryption, PackReadEncryption, PackReader, PackWriter,
    PackWriterOptions, PlainPage, decode_encrypted_frame, encrypted_frame_pack_id,
};
use mirage_types::{
    ByteCount, CheckedRange, ContentHash, GenerationId, MirageError, PageHash, RepositoryId,
    UpdateId,
};
use serde::Serialize;
use tokio_util::sync::CancellationToken;

use super::drive_live;
use crate::output;

const PAGE_BYTES: u64 = 1024 * 1024;
const LOGICAL_PAGES: u32 = 100 * 1024;
const LOGICAL_BYTES: u64 = LOGICAL_PAGES as u64 * PAGE_BYTES;
const BASE_UNIQUE_PAGES: u32 = 64;
const UPDATE_UNIQUE_PAGES: u32 = 16;
const UPDATED_LOGICAL_PAGES: u32 = 25 * 1024;
const UPDATED_LOGICAL_BYTES: u64 = UPDATED_LOGICAL_PAGES as u64 * PAGE_BYTES;
const RANDOM_READS: u64 = 100_000;

#[derive(Debug, Serialize)]
struct DriveGateEvidence {
    evidence_version: u32,
    status: &'static str,
    completed_unix_seconds: u64,
    account_binding_verified: bool,
    exact_drive_file_scope_verified: bool,
    token_refreshes_verified: u32,
    logical_corpus_bytes: u64,
    base_unique_physical_bytes: u64,
    updated_logical_bytes: u64,
    update_unique_physical_bytes: u64,
    encrypted_frames: bool,
    signed_chain_length: usize,
    authoritative_generation: u64,
    gate_d_random_reads: u64,
    gate_f_random_reads: u64,
    incorrect_bytes: u64,
    gate_d_cache_cap_bytes: u64,
    gate_d_peak_cache_bytes: u64,
    gate_f_cache_cap_bytes: u64,
    gate_f_peak_cache_bytes: u64,
    cache_eviction_refetch_verified: bool,
    staging_eviction_refetch_verified: bool,
    stale_hint_fallback_verified: bool,
    resumable_restart_verified: bool,
    injected_http_429: u32,
    injected_disconnects: u32,
    bounded_retry_attempts: bool,
    provider_requests: u64,
    provider_retries: u64,
    uploaded_request_body_bytes: u64,
    downloaded_response_body_bytes: u64,
    quota_available_before_bytes: Option<u64>,
    physical_model: &'static str,
}

pub fn run(
    client_credentials: &Path,
    token_store: Option<&Path>,
    work_directory: &Path,
    json: bool,
) -> Result<(), MirageError> {
    prepare_work_directory(work_directory)?;
    let evidence =
        futures_executor::block_on(run_live(client_credentials, token_store, work_directory))?;
    let encoded = serde_json::to_vec_pretty(&evidence).map_err(|error| {
        MirageError::internal_invariant("Drive gate evidence serialization failed")
            .with_source(error)
    })?;
    fs::write(work_directory.join("gate-drive-evidence.json"), encoded)
        .map_err(MirageError::from)?;
    if json {
        output::emit_success(&evidence)
    } else {
        println!("Authenticated Google Drive Gates D and F passed.");
        println!("Logical corpus: {} bytes", evidence.logical_corpus_bytes);
        println!(
            "Signed generation: {} (chain length {})",
            evidence.authoritative_generation, evidence.signed_chain_length
        );
        println!(
            "Random reads: {} + {}; incorrect bytes: {}",
            evidence.gate_d_random_reads, evidence.gate_f_random_reads, evidence.incorrect_bytes
        );
        println!(
            "Provider requests: {}; uploaded/downloaded bodies: {}/{} bytes",
            evidence.provider_requests,
            evidence.uploaded_request_body_bytes,
            evidence.downloaded_response_body_bytes
        );
        Ok(())
    }
}

async fn run_live(
    client_credentials: &Path,
    token_store: Option<&Path>,
    work_directory: &Path,
) -> Result<DriveGateEvidence, MirageError> {
    let first = drive_live::connect_async(client_credentials, token_store).await?;
    let quota_available = first
        .quota
        .limit
        .map(|limit| limit.saturating_sub(first.quota.usage));
    let repository = repository_id(&first.account_id);
    let signer = load_or_create_signer(&work_directory.join("repository-signer.dpapi"))?;
    let repository_key = Arc::new(load_or_create_repository_key(
        &work_directory.join("repository-content-key.dpapi"),
        repository,
    )?);
    let (base_pack, base_pages) = load_or_create_pack(
        &work_directory.join("base-pack"),
        b"base",
        BASE_UNIQUE_PAGES,
        repository,
        Arc::clone(&repository_key),
    )?;
    let base_manifest = build_base_manifest(repository, &base_pack, &base_pages)?;

    let first_metrics = Arc::clone(&first.transport);
    let provider_transport: Arc<dyn HttpTransport> = first.transport.clone();
    let faults = Arc::new(FaultOnceTransport::new(provider_transport));
    let retrying_faults =
        Arc::new(RetryingHttpTransport::new(faults.clone(), 5).map_err(MirageError::from)?);
    let first_backend = DriveObjectBackend::new(
        retrying_faults.clone(),
        first.access_token.clone(),
        repository,
    )
    .map_err(MirageError::from)?;
    let published_base = publish_base_generation(
        &first_backend,
        std::slice::from_ref(&base_pack),
        &base_manifest,
        &signer,
    )
    .await?;
    if faults.injected() != 2 || retrying_faults.retry_count() != 2 {
        return Err(MirageError::internal_invariant(
            "Drive gate did not exercise both bounded retry faults",
        ));
    }
    resumable_restart_probe(
        &first_backend,
        first.transport.as_ref(),
        first.access_token.as_str(),
        repository,
    )
    .await?;

    let second = drive_live::connect_async(client_credentials, token_store).await?;
    if second.account_id != first.account_id {
        return Err(MirageError::backend_unauthenticated(
            "Drive account changed across the forced refresh boundary",
        ));
    }
    let second_metrics = Arc::clone(&second.transport);
    let second_backend = DriveObjectBackend::new(
        second.transport.clone(),
        second.access_token.clone(),
        repository,
    )
    .map_err(MirageError::from)?;
    let stale_hint = RemoteObjectRef {
        backend_id: BackendId::new("drive")?,
        provider_object_id: ProviderObjectId::new("deleted-stale-mirage-gate-hint")?,
        immutable_revision: None,
        byte_length: ByteCount::from_u64(1),
        content_hash: ContentHash::from_bytes([0; 32]),
        kind: ObjectKind::Commit,
    };
    let recovered_base = recover_repository_with_hints(
        &second_backend,
        repository,
        &signer.verifier(),
        RecoveryHints {
            cached_commit: None,
            latest_hint: Some(stale_hint),
        },
    )
    .await?;
    if recovered_base.head_hash != published_base.commit_hash
        || recovered_base.chain.len() != 1
        || recovered_base.manifest.generation_id != GenerationId::ZERO
    {
        return Err(MirageError::integrity_mismatch(
            "Drive base generation did not recover authoritatively",
        ));
    }
    let (gate_d_incorrect, gate_d_peak) = verify_random_reads(
        &second_backend,
        &recovered_base.manifest,
        &repository_key,
        PAGE_BYTES * u64::from(BASE_UNIQUE_PAGES),
        false,
    )
    .await?;
    verify_eviction_refetch(&second_backend, &recovered_base.manifest, &repository_key).await?;

    let pre_update = recover_repository(&second_backend, repository, &signer.verifier()).await?;
    if pre_update.chain.len() != 1 || pre_update.manifest.generation_id != GenerationId::ZERO {
        return Err(MirageError::repository_conflict(
            "Drive update did not begin from generation N",
        ));
    }
    let (update_pack, update_pages) = load_or_create_pack(
        &work_directory.join("update-pack"),
        b"update",
        UPDATE_UNIQUE_PAGES,
        repository,
        Arc::clone(&repository_key),
    )?;
    let update_bytes = fs::read(&update_pack.path).map_err(MirageError::from)?;
    let update_object = second_backend
        .put_immutable(
            ObjectKind::Pack,
            UploadSource::from_bytes(Bytes::from(update_bytes)),
            update_pack.content_hash,
            CancellationToken::new(),
        )
        .await?;
    let update_stat = second_backend.stat(&update_object).await?;
    if update_stat.content_hash != update_pack.content_hash
        || update_stat.byte_length.as_u64() != update_pack.byte_length
    {
        return Err(MirageError::integrity_mismatch(
            "Drive update staging pack failed remote verification",
        ));
    }
    verify_staging_eviction(
        &second_backend,
        &update_object,
        &update_pack,
        &repository_key,
        repository,
    )
    .await?;
    let updated_manifest = build_update_manifest(
        &pre_update.manifest,
        &update_pack,
        &update_pages,
        &update_object,
    )?;
    let parent = pre_update
        .chain
        .last()
        .ok_or_else(|| MirageError::internal_invariant("Drive base chain is empty"))?;
    let published_update = publish_successor_generation(
        &second_backend,
        &updated_manifest,
        parent,
        &signer,
        update_id(repository),
    )
    .await?;

    let third = drive_live::connect_async(client_credentials, token_store).await?;
    if third.account_id != first.account_id {
        return Err(MirageError::backend_unauthenticated(
            "Drive account changed after update restart",
        ));
    }
    let third_metrics = Arc::clone(&third.transport);
    let third_backend =
        DriveObjectBackend::new(third.transport.clone(), third.access_token, repository)
            .map_err(MirageError::from)?;
    let recovered_update =
        recover_repository(&third_backend, repository, &signer.verifier()).await?;
    if recovered_update.head_hash != published_update.commit_hash
        || recovered_update.chain.len() != 2
        || recovered_update.manifest.generation_id != GenerationId::from_u64(1)
    {
        return Err(MirageError::integrity_mismatch(
            "Drive update did not recover as one signed N+1 generation",
        ));
    }
    let gate_f_cap = PAGE_BYTES * u64::from(BASE_UNIQUE_PAGES + UPDATE_UNIQUE_PAGES);
    let (gate_f_incorrect, gate_f_peak) = verify_random_reads(
        &third_backend,
        &recovered_update.manifest,
        &repository_key,
        gate_f_cap,
        true,
    )
    .await?;
    let incorrect_bytes = gate_d_incorrect.saturating_add(gate_f_incorrect);
    if incorrect_bytes != 0 {
        return Err(MirageError::integrity_mismatch(
            "Drive gate random reads differed from the deterministic byte oracle",
        ));
    }

    let metrics = [&first_metrics, &second_metrics, &third_metrics];
    Ok(DriveGateEvidence {
        evidence_version: 1,
        status: "passed",
        completed_unix_seconds: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| MirageError::internal_invariant("system clock precedes Unix epoch"))?
            .as_secs(),
        account_binding_verified: true,
        exact_drive_file_scope_verified: true,
        token_refreshes_verified: 3,
        logical_corpus_bytes: LOGICAL_BYTES,
        base_unique_physical_bytes: base_pack.byte_length,
        updated_logical_bytes: UPDATED_LOGICAL_BYTES,
        update_unique_physical_bytes: update_pack.byte_length,
        encrypted_frames: true,
        signed_chain_length: recovered_update.chain.len(),
        authoritative_generation: recovered_update.manifest.generation_id.as_u64(),
        gate_d_random_reads: RANDOM_READS,
        gate_f_random_reads: RANDOM_READS,
        incorrect_bytes,
        gate_d_cache_cap_bytes: PAGE_BYTES * u64::from(BASE_UNIQUE_PAGES),
        gate_d_peak_cache_bytes: gate_d_peak,
        gate_f_cache_cap_bytes: gate_f_cap,
        gate_f_peak_cache_bytes: gate_f_peak,
        cache_eviction_refetch_verified: true,
        staging_eviction_refetch_verified: true,
        stale_hint_fallback_verified: true,
        resumable_restart_verified: true,
        injected_http_429: 1,
        injected_disconnects: 1,
        bounded_retry_attempts: retrying_faults.retry_count() == 2,
        provider_requests: metrics.iter().map(|value| value.request_count()).sum(),
        provider_retries: metrics.iter().map(|value| value.retry_count()).sum(),
        uploaded_request_body_bytes: metrics.iter().map(|value| value.uploaded_bytes()).sum(),
        downloaded_response_body_bytes: metrics.iter().map(|value| value.downloaded_bytes()).sum(),
        quota_available_before_bytes: quota_available,
        physical_model: "100 GiB logical corpus backed by 64 deterministic encrypted unique pages; 25 GiB logical update backed by 16 new encrypted unique pages",
    })
}

fn prepare_work_directory(path: &Path) -> Result<(), MirageError> {
    if !path.is_absolute() {
        return Err(MirageError::invalid_argument(
            "Drive gate work directory must be absolute",
        ));
    }
    fs::create_dir_all(path).map_err(MirageError::from)?;
    let work = fs::canonicalize(path).map_err(MirageError::from)?;
    let source = fs::canonicalize(std::env::current_dir().map_err(MirageError::from)?)
        .map_err(MirageError::from)?;
    if work.starts_with(source) {
        return Err(MirageError::invalid_argument(
            "Drive gate state must remain outside the source repository",
        ));
    }
    Ok(())
}

fn repository_id(account_id: &str) -> RepositoryId {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"MirageSSD authenticated Drive gate repository v1\0");
    hasher.update(account_id.as_bytes());
    let hash = hasher.finalize();
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&hash.as_bytes()[..16]);
    RepositoryId::from_bytes(bytes)
}

fn update_id(repository: RepositoryId) -> UpdateId {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"MirageSSD authenticated Drive gate update v1\0");
    hasher.update(repository.as_bytes());
    let hash = hasher.finalize();
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&hash.as_bytes()[..16]);
    UpdateId::from_bytes(bytes)
}

fn load_or_create_signer(path: &Path) -> Result<RepositorySigner, MirageError> {
    if path.exists() {
        return load_signer(path);
    }
    let signer = RepositorySigner::generate()?;
    save_signer(path, &signer, ProtectionScope::CurrentUser)?;
    Ok(signer)
}

fn load_or_create_repository_key(
    path: &Path,
    repository: RepositoryId,
) -> Result<RepositoryKey, MirageError> {
    if path.exists() {
        return load_repository_key(path, repository);
    }
    let key = RepositoryKey::generate()?;
    save_repository_key(path, repository, &key, ProtectionScope::CurrentUser)?;
    Ok(key)
}

fn load_or_create_pack(
    directory: &Path,
    domain: &[u8],
    unique_pages: u32,
    repository: RepositoryId,
    key: Arc<RepositoryKey>,
) -> Result<(CompletedPack, Vec<PlainPage>), MirageError> {
    fs::create_dir_all(directory).map_err(MirageError::from)?;
    let pages = (0..unique_pages)
        .map(|index| PlainPage::from_bytes(Bytes::from(gate_page(domain, index))))
        .collect::<Vec<_>>();
    let mut existing = fs::read_dir(directory)
        .map_err(MirageError::from)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|value| value == "bin"))
        .collect::<Vec<_>>();
    existing.sort();
    if existing.len() > 1 {
        return Err(MirageError::repository_conflict(
            "Drive gate pack directory contains multiple immutable packs",
        ));
    }
    if let Some(path) = existing.pop() {
        let mut reader = PackReader::open_verified_encrypted(
            &path,
            PackReadEncryption {
                repository_id: repository,
                key,
            },
        )?;
        if !reader.is_encrypted() || reader.entries().len() != pages.len() {
            return Err(MirageError::integrity_mismatch(
                "persisted Drive gate pack has the wrong encryption or page count",
            ));
        }
        for page in &pages {
            if reader.read_page(page.hash)?.page.bytes != page.bytes {
                return Err(MirageError::integrity_mismatch(
                    "persisted Drive gate pack differs from its deterministic oracle",
                ));
            }
        }
        return Ok((
            CompletedPack {
                path,
                content_hash: reader.content_hash(),
                byte_length: reader.file_length(),
                entries: reader.entries().to_vec(),
            },
            pages,
        ));
    }
    let mut writer = PackWriter::create_encrypted(
        directory,
        PackWriterOptions {
            page_size: PAGE_BYTES as u32,
            target_size: PAGE_BYTES * (u64::from(unique_pages) + 2),
            align_frames_4k: true,
        },
        PackEncryption {
            repository_id: repository,
            key,
        },
    )?;
    for page in &pages {
        writer.append_page(page)?;
    }
    Ok((writer.finish()?, pages))
}

fn build_base_manifest(
    repository: RepositoryId,
    pack: &CompletedPack,
    unique_pages: &[PlainPage],
) -> Result<mirage_manifest::RepositoryManifest, MirageError> {
    let object = local_pack_ref(pack)?;
    let entries = pack
        .entries
        .iter()
        .map(|entry| (entry.page_hash, *entry))
        .collect::<HashMap<_, _>>();
    let pages = (0..LOGICAL_PAGES)
        .map(|ordinal| {
            let page = &unique_pages[(ordinal % BASE_UNIQUE_PAGES) as usize];
            let entry = entries.get(&page.hash).ok_or_else(|| {
                MirageError::internal_invariant("base pack omitted a deterministic page")
            })?;
            Ok(ManifestPage {
                hash: page.hash,
                logical_length: PAGE_BYTES as u32,
                object: object.clone(),
                offset: entry.frame_offset,
                encoded_length: ByteCount::from_u64(entry.frame_length),
                codec: Codec::None,
            })
        })
        .collect::<Result<Vec<_>, MirageError>>()?;
    ManifestBuilder::new(
        repository,
        GenerationId::ZERO,
        ByteCount::from_u64(PAGE_BYTES),
    )
    .build(vec![ManifestFile {
        relative_path: "corpus/large-container.bin".into(),
        logical_size: ByteCount::from_u64(LOGICAL_BYTES),
        class: FileClass::VirtualContainer,
        pages,
    }])
}

fn build_update_manifest(
    base: &mirage_manifest::RepositoryManifest,
    pack: &CompletedPack,
    unique_pages: &[PlainPage],
    object: &RemoteObjectRef,
) -> Result<mirage_manifest::RepositoryManifest, MirageError> {
    let file_id = base
        .files
        .first()
        .ok_or_else(|| MirageError::internal_invariant("Drive gate manifest has no corpus file"))?
        .stable_id;
    let entries = pack
        .entries
        .iter()
        .map(|entry| (entry.page_hash, *entry))
        .collect::<HashMap<_, _>>();
    let mappings = (0..UPDATED_LOGICAL_PAGES)
        .map(|page_index| {
            let page = &unique_pages[(page_index % UPDATE_UNIQUE_PAGES) as usize];
            let entry = entries.get(&page.hash).ok_or_else(|| {
                MirageError::internal_invariant("update pack omitted a deterministic page")
            })?;
            Ok(StagedPageMapping {
                file_id,
                page_index,
                page_hash: page.hash,
                object: object.clone(),
                frame_offset: entry.frame_offset,
                frame_length: entry.frame_length,
                logical_length: entry.logical_length,
                codec: entry.codec,
            })
        })
        .collect::<Result<Vec<_>, MirageError>>()?;
    build_updated_manifest(
        base,
        GenerationId::from_u64(1),
        ManifestOverlay {
            page_mappings: mappings,
            ..Default::default()
        },
    )
}

fn local_pack_ref(pack: &CompletedPack) -> Result<RemoteObjectRef, MirageError> {
    Ok(RemoteObjectRef {
        backend_id: BackendId::new("local")?,
        provider_object_id: ProviderObjectId::new(
            pack.path
                .file_name()
                .and_then(|value| value.to_str())
                .ok_or_else(|| MirageError::invalid_argument("Drive gate pack name is invalid"))?,
        )?,
        immutable_revision: None,
        byte_length: ByteCount::from_u64(pack.byte_length),
        content_hash: pack.content_hash,
        kind: ObjectKind::Pack,
    })
}

fn gate_page(domain: &[u8], index: u32) -> Vec<u8> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"MirageSSD Drive gate deterministic page v1\0");
    hasher.update(domain);
    hasher.update(&index.to_le_bytes());
    let mut output = vec![0; PAGE_BYTES as usize];
    hasher.finalize_xof().fill(&mut output);
    output
}

fn gate_slice(domain: &[u8], index: u32, offset: usize, length: usize) -> Vec<u8> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"MirageSSD Drive gate deterministic page v1\0");
    hasher.update(domain);
    hasher.update(&index.to_le_bytes());
    let mut reader = hasher.finalize_xof();
    reader.set_position(offset as u64);
    let mut output = vec![0; length];
    reader.fill(&mut output);
    output
}

async fn verify_random_reads(
    backend: &dyn ObjectBackend,
    manifest: &mirage_manifest::RepositoryManifest,
    key: &RepositoryKey,
    cache_cap: u64,
    updated: bool,
) -> Result<(u64, u64), MirageError> {
    let mut cache = PageCache::new(cache_cap);
    let mut state = if updated {
        0x4d_69_72_61_67_65_46_u64
    } else {
        0x4d_69_72_61_67_65_44_u64
    };
    let mut incorrect = 0_u64;
    for _ in 0..RANDOM_READS {
        let random = splitmix64(&mut state);
        let length = 1 + ((random >> 48) & 0xffff) as usize;
        let start = splitmix64(&mut state) % (LOGICAL_BYTES - length as u64 + 1);
        let mut consumed = 0_usize;
        while consumed < length {
            let logical = start + consumed as u64;
            let page_ordinal = (logical / PAGE_BYTES) as u32;
            let in_page = (logical % PAGE_BYTES) as usize;
            let take = (length - consumed).min(PAGE_BYTES as usize - in_page);
            let actual = load_page(backend, manifest, page_ordinal, key, &mut cache).await?;
            let (domain, unique) = if updated && page_ordinal < UPDATED_LOGICAL_PAGES {
                (b"update".as_slice(), page_ordinal % UPDATE_UNIQUE_PAGES)
            } else {
                (b"base".as_slice(), page_ordinal % BASE_UNIQUE_PAGES)
            };
            let expected = gate_slice(domain, unique, in_page, take);
            incorrect = incorrect.saturating_add(
                actual[in_page..in_page + take]
                    .iter()
                    .zip(expected.iter())
                    .filter(|(left, right)| left != right)
                    .count() as u64,
            );
            consumed += take;
        }
    }
    Ok((incorrect, cache.peak))
}

async fn load_page(
    backend: &dyn ObjectBackend,
    manifest: &mirage_manifest::RepositoryManifest,
    page_ordinal: u32,
    key: &RepositoryKey,
    cache: &mut PageCache,
) -> Result<Bytes, MirageError> {
    let page = manifest
        .pages
        .get(page_ordinal as usize)
        .ok_or_else(|| MirageError::manifest_invalid("Drive gate page ordinal is absent"))?;
    if let Some(bytes) = cache.get(page.plaintext_hash) {
        return Ok(bytes);
    }
    let location = manifest
        .remote_locations
        .get(page.remote_location as usize)
        .ok_or_else(|| MirageError::manifest_invalid("Drive gate remote location is absent"))?;
    if location.codec != Codec::None {
        return Err(MirageError::unsupported_layout(
            "Drive gate expected an uncompressed encrypted frame",
        ));
    }
    let response = backend
        .read_range(
            &location.object,
            CheckedRange::new(location.offset, location.encoded_length.as_u64())?,
            FetchClass::BlockingRead,
            CancellationToken::new(),
        )
        .await?;
    let frame = response
        .collect_bounded(location.encoded_length.as_u64())
        .await?;
    let pack_id = encrypted_frame_pack_id(&frame)?;
    let plaintext = decode_encrypted_frame(
        key,
        &frame,
        EncryptedFrameAad {
            repository: manifest.repository_id,
            pack_id,
            frame_index: location.offset,
            plaintext_hash: page.plaintext_hash,
            plaintext_length: page.logical_length,
        },
    )?;
    let bytes = Bytes::from(plaintext);
    cache.insert(page.plaintext_hash, bytes.clone())?;
    Ok(bytes)
}

async fn verify_eviction_refetch(
    backend: &dyn ObjectBackend,
    manifest: &mirage_manifest::RepositoryManifest,
    key: &RepositoryKey,
) -> Result<(), MirageError> {
    let mut cache = PageCache::new(PAGE_BYTES);
    let first = load_page(backend, manifest, 0, key, &mut cache).await?;
    let _second = load_page(backend, manifest, 1, key, &mut cache).await?;
    let reloaded = load_page(backend, manifest, 0, key, &mut cache).await?;
    if first != reloaded || cache.peak > PAGE_BYTES {
        return Err(MirageError::integrity_mismatch(
            "Drive cache eviction/refetch changed a verified page",
        ));
    }
    Ok(())
}

async fn verify_staging_eviction(
    backend: &dyn ObjectBackend,
    object: &RemoteObjectRef,
    pack: &CompletedPack,
    key: &RepositoryKey,
    repository: RepositoryId,
) -> Result<(), MirageError> {
    let mut first = None;
    for (index, entry) in pack.entries.iter().enumerate() {
        let bytes = read_encrypted_entry(backend, object, *entry, key, repository).await?;
        if index == 0 {
            first = Some(bytes);
        }
    }
    let first_entry = pack
        .entries
        .first()
        .ok_or_else(|| MirageError::internal_invariant("Drive update pack is empty"))?;
    let reloaded = read_encrypted_entry(backend, object, *first_entry, key, repository).await?;
    if first.as_ref() != Some(&reloaded) {
        return Err(MirageError::integrity_mismatch(
            "Drive staged page changed after local eviction and remote reread",
        ));
    }
    Ok(())
}

async fn read_encrypted_entry(
    backend: &dyn ObjectBackend,
    object: &RemoteObjectRef,
    entry: mirage_pack::PackEntry,
    key: &RepositoryKey,
    repository: RepositoryId,
) -> Result<Bytes, MirageError> {
    let response = backend
        .read_range(
            object,
            CheckedRange::new(entry.frame_offset, entry.frame_length)?,
            FetchClass::Maintenance,
            CancellationToken::new(),
        )
        .await?;
    let frame = response.collect_bounded(entry.frame_length).await?;
    let pack_id = encrypted_frame_pack_id(&frame)?;
    let plaintext = decode_encrypted_frame(
        key,
        &frame,
        EncryptedFrameAad {
            repository,
            pack_id,
            frame_index: entry.frame_offset,
            plaintext_hash: entry.page_hash,
            plaintext_length: entry.logical_length,
        },
    )?;
    Ok(Bytes::from(plaintext))
}

async fn resumable_restart_probe(
    backend: &dyn ObjectBackend,
    transport: &dyn HttpTransport,
    access_token: &str,
    repository: RepositoryId,
) -> Result<(), MirageError> {
    let bytes = Bytes::from(gate_page(b"resumable", 0));
    let hash = ContentHash::from_bytes(*blake3::hash(&bytes).as_bytes());
    let mut properties = BTreeMap::new();
    properties.insert("mirage_repository".into(), repository.to_string());
    properties.insert("mirage_kind".into(), ObjectKind::Profile.as_str().into());
    properties.insert("mirage_hash".into(), hash.to_string());
    let mut original = resumable::start(
        transport,
        access_token,
        "profile-resumable-restart-probe.bin",
        &properties,
        bytes.len() as u64,
    )
    .await?;
    if resumable::upload_chunk(
        transport,
        &mut original,
        bytes.slice(..resumable::CHUNK_ALIGNMENT as usize),
    )
    .await?
    .is_some()
    {
        return Err(MirageError::integrity_mismatch(
            "Drive resumable probe completed before restart",
        ));
    }
    let mut restarted = ResumableSession {
        uri: original.uri,
        committed: 0,
        total: original.total,
    };
    restarted.committed = resumable::query(transport, &restarted).await?;
    if restarted.committed != resumable::CHUNK_ALIGNMENT {
        return Err(MirageError::integrity_mismatch(
            "Drive resumable restart recovered the wrong offset",
        ));
    }
    let start = restarted.committed as usize;
    let completed = resumable::upload_chunk(transport, &mut restarted, bytes.slice(start..))
        .await?
        .ok_or_else(|| MirageError::integrity_mismatch("Drive resumable probe did not complete"))?;
    let object = RemoteObjectRef {
        backend_id: BackendId::new("drive")?,
        provider_object_id: completed.file_id,
        immutable_revision: completed.revision,
        byte_length: ByteCount::from_u64(completed.size),
        content_hash: hash,
        kind: ObjectKind::Profile,
    };
    backend.stat(&object).await?;
    backend
        .delete_immutable(
            &object,
            &DeletionProof {
                repository_id: repository,
                object_hash: hash,
                retained_root_set_hash: ContentHash::from_bytes([0; 32]),
                validated_at_sequence: 0,
            },
            CancellationToken::new(),
        )
        .await?;
    Ok(())
}

fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut value = *state;
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

struct PageCache {
    cap: u64,
    used: u64,
    peak: u64,
    pages: HashMap<PageHash, Bytes>,
    order: VecDeque<PageHash>,
}

impl PageCache {
    fn new(cap: u64) -> Self {
        Self {
            cap,
            used: 0,
            peak: 0,
            pages: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    fn get(&self, hash: PageHash) -> Option<Bytes> {
        self.pages.get(&hash).cloned()
    }

    fn insert(&mut self, hash: PageHash, bytes: Bytes) -> Result<(), MirageError> {
        let length = bytes.len() as u64;
        if length > self.cap {
            return Err(MirageError::cache_full(
                "Drive gate page exceeds the explicit cache cap",
            ));
        }
        while self.used.saturating_add(length) > self.cap {
            let evicted = self.order.pop_front().ok_or_else(|| {
                MirageError::internal_invariant("Drive gate cache accounting diverged")
            })?;
            if let Some(removed) = self.pages.remove(&evicted) {
                self.used = self.used.saturating_sub(removed.len() as u64);
            }
        }
        if let Some(previous) = self.pages.insert(hash, bytes) {
            self.used = self.used.saturating_sub(previous.len() as u64);
        } else {
            self.order.push_back(hash);
        }
        self.used = self.used.saturating_add(length);
        self.peak = self.peak.max(self.used);
        Ok(())
    }
}

struct FaultOnceTransport {
    inner: Arc<dyn HttpTransport>,
    phase: AtomicU8,
}

impl FaultOnceTransport {
    fn new(inner: Arc<dyn HttpTransport>) -> Self {
        Self {
            inner,
            phase: AtomicU8::new(0),
        }
    }

    fn injected(&self) -> u8 {
        self.phase.load(Ordering::Relaxed).min(2)
    }

    fn take_phase(&self, expected: u8) -> bool {
        self.phase
            .compare_exchange(expected, expected + 1, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }
}

#[async_trait]
impl HttpTransport for FaultOnceTransport {
    async fn execute(&self, request: HttpRequest) -> Result<HttpResponse, BackendError> {
        if self.take_phase(0) {
            return Ok(HttpResponse {
                status: 429,
                headers: [("retry-after".into(), "0".into())].into(),
                body: Bytes::new(),
            });
        }
        if self.take_phase(1) {
            return Err(BackendError::new(
                BackendErrorClass::TransientTransport,
                "injected Drive connection interruption",
            ));
        }
        self.inner.execute(request).await
    }
}
