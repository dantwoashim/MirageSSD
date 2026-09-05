use std::path::Path;

use mirage_backend_local::LocalObjectBackend;
use mirage_crypto::repository_key_store::load_repository_key;
use mirage_engine::{
    extract_virtual_files_with_encryption, publish_base_generation, recover_repository,
};
use mirage_manifest::{DecodeLimits, InMemoryTestSigner, decode_manifest_bounded};
use mirage_pack::{CompletedPack, PackReadEncryption, PackReader};
use mirage_types::{MirageError, RepositoryId};

use crate::output;

pub fn commit_local(
    import: &Path,
    backend_root: &Path,
    repository_id: RepositoryId,
    key_id_hex: &str,
    test_key_hex: &str,
    json: bool,
) -> Result<(), MirageError> {
    let signer = signer(key_id_hex, test_key_hex)?;
    let manifest_bytes =
        std::fs::read(import.join("base-manifest.cbor")).map_err(MirageError::from)?;
    let manifest = decode_manifest_bounded(&manifest_bytes, DecodeLimits::default())?;
    if manifest.repository_id != repository_id {
        return Err(MirageError::invalid_argument(
            "import manifest repository does not match --repository-id",
        ));
    }
    let mut paths = std::fs::read_dir(import)
        .map_err(MirageError::from)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("pack-") && name.ends_with(".bin"))
        })
        .collect::<Vec<_>>();
    paths.sort();
    let packs = paths
        .into_iter()
        .map(|path| {
            let reader = PackReader::open_verified(&path)?;
            Ok(CompletedPack {
                path,
                content_hash: reader.content_hash(),
                byte_length: reader.file_length(),
                entries: reader.entries().to_vec(),
            })
        })
        .collect::<Result<Vec<_>, MirageError>>()?;
    if packs.is_empty() && !manifest.pages.is_empty() {
        return Err(MirageError::invalid_argument(
            "import contains no pack files",
        ));
    }
    let backend = LocalObjectBackend::open(backend_root, repository_id)?;
    let published = futures_executor::block_on(publish_base_generation(
        &backend, &packs, &manifest, &signer,
    ))?;
    emit(
        json,
        serde_json::json!({
            "report_version": 1,
            "commit_hash": published.commit_hash,
            "pack_count": published.packs.len(),
        }),
        format!("local commit complete: {} packs", published.packs.len()),
    )
}

pub fn verify(
    backend_root: &Path,
    repository_id: RepositoryId,
    key_id_hex: &str,
    test_key_hex: &str,
    level: &str,
    json: bool,
) -> Result<(), MirageError> {
    if level != "metadata" {
        return Err(MirageError::invalid_argument(
            "only --level metadata is implemented in this milestone",
        ));
    }
    let signer = signer(key_id_hex, test_key_hex)?;
    let backend = LocalObjectBackend::open(backend_root, repository_id)?;
    let recovered =
        futures_executor::block_on(recover_repository(&backend, repository_id, &signer))?;
    emit(
        json,
        serde_json::json!({
            "report_version": 1,
            "level": "metadata",
            "head_commit": recovered.head_hash,
            "chain_length": recovered.chain.len(),
            "verified_pack_count": recovered.verified_pack_count,
        }),
        format!(
            "metadata verified: {} commits, {} packs",
            recovered.chain.len(),
            recovered.verified_pack_count
        ),
    )
}

pub fn extract(
    backend_root: &Path,
    destination: &Path,
    repository_id: RepositoryId,
    key_id_hex: &str,
    test_key_hex: &str,
    repository_key: Option<&Path>,
    json: bool,
) -> Result<(), MirageError> {
    let signer = signer(key_id_hex, test_key_hex)?;
    let backend = LocalObjectBackend::open(backend_root, repository_id)?;
    let recovered =
        futures_executor::block_on(recover_repository(&backend, repository_id, &signer))?;
    let encryption = repository_key
        .map(|path| {
            Ok::<PackReadEncryption, MirageError>(PackReadEncryption {
                repository_id,
                key: std::sync::Arc::new(load_repository_key(path, repository_id)?),
            })
        })
        .transpose()?;
    let report = futures_executor::block_on(extract_virtual_files_with_encryption(
        &backend,
        &recovered.manifest,
        destination,
        encryption.as_ref(),
    ))?;
    emit(
        json,
        serde_json::json!({
            "report_version": 1,
            "files_written": report.files_written,
            "bytes_written": report.bytes_written,
        }),
        format!(
            "extract complete: {} files, {} bytes",
            report.files_written, report.bytes_written
        ),
    )
}

pub(super) fn signer(key_id_hex: &str, key_hex: &str) -> Result<InMemoryTestSigner, MirageError> {
    Ok(InMemoryTestSigner::new(
        parse_hex::<16>(key_id_hex, "key id")?,
        parse_hex::<32>(key_hex, "test key")?,
    ))
}

fn parse_hex<const N: usize>(value: &str, label: &str) -> Result<[u8; N], MirageError> {
    if value.len() != N * 2 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(MirageError::invalid_argument(format!(
            "{label} must be exactly {} hexadecimal characters",
            N * 2
        )));
    }
    let mut output = [0_u8; N];
    for (index, slot) in output.iter_mut().enumerate() {
        *slot = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
            .map_err(|_| MirageError::invalid_argument(format!("{label} is not hexadecimal")))?;
    }
    Ok(output)
}

fn emit(json: bool, data: serde_json::Value, human: String) -> Result<(), MirageError> {
    if json {
        output::emit_success(&data)
    } else {
        println!("{human}");
        Ok(())
    }
}
