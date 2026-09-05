use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

use mirage_backend_drive::DriveObjectBackend;
use mirage_engine::publish_base_generation;
use mirage_manifest::{DecodeLimits, decode_manifest_bounded, encode_manifest};
use mirage_pack::{CompletedPack, PackReader};
use mirage_types::{MirageError, RepositoryId};

use super::{drive_live, repo_local};
use crate::output;

const DRIVE_MANIFEST: &str = "drive-manifest.cbor";

pub fn publish(
    import: &Path,
    repository_id: RepositoryId,
    client_credentials: &Path,
    token_store: Option<&Path>,
    key_id_hex: &str,
    test_key_hex: &str,
    json: bool,
) -> Result<(), MirageError> {
    let manifest_bytes = fs::read(import.join("base-manifest.cbor")).map_err(MirageError::from)?;
    let manifest = decode_manifest_bounded(&manifest_bytes, DecodeLimits::default())?;
    if manifest.repository_id != repository_id {
        return Err(MirageError::invalid_argument(
            "import manifest repository does not match --repository-id",
        ));
    }
    let mut paths = fs::read_dir(import)
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

    let session = drive_live::connect(client_credentials, token_store)?;
    let backend = DriveObjectBackend::new(session.transport, session.access_token, repository_id)?;
    let signer = repo_local::signer(key_id_hex, test_key_hex)?;
    let published = futures_executor::block_on(publish_base_generation(
        &backend, &packs, &manifest, &signer,
    ))?;
    let encoded = encode_manifest(&published.published_manifest)?;
    let path = import.join(DRIVE_MANIFEST);
    write_new_or_identical(&path, &encoded)?;

    let data = serde_json::json!({
        "report_version": 1,
        "repository_id": repository_id.to_string(),
        "account_id": session.account_id,
        "pack_count": published.packs.len(),
        "commit_hash": published.commit_hash.to_string(),
        "drive_manifest": path,
        "encrypted_pack_requirement": "enforced"
    });
    if json {
        output::emit_success(&data)
    } else {
        println!(
            "Drive publication complete: {} encrypted pack(s), commit {}",
            published.packs.len(),
            published.commit_hash
        );
        Ok(())
    }
}

fn write_new_or_identical(path: &Path, bytes: &[u8]) -> Result<(), MirageError> {
    if path.exists() {
        return if fs::read(path).map_err(MirageError::from)? == bytes {
            Ok(())
        } else {
            Err(MirageError::repository_conflict(
                "Drive publication manifest already exists with different bytes",
            ))
        };
    }
    let temporary = path.with_extension("cbor.partial");
    if temporary.exists() {
        fs::remove_file(&temporary).map_err(MirageError::from)?;
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(MirageError::from)?;
    let result = file
        .write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(MirageError::from);
    drop(file);
    if let Err(error) = result {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    fs::rename(temporary, path).map_err(MirageError::from)
}
