use std::io::Write;
use std::path::Path;

use mirage_backend::ObjectBackend;
use mirage_manifest::RepositoryManifest;
use mirage_pack::{DecodedFrame, EncryptedFrameAad, PackReadEncryption, PlainPage};
use mirage_types::MirageError;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExtractReport {
    pub files_written: u64,
    pub bytes_written: u64,
}

pub async fn extract_virtual_files(
    backend: &dyn ObjectBackend,
    manifest: &RepositoryManifest,
    destination: &Path,
) -> Result<ExtractReport, MirageError> {
    extract_virtual_files_with_encryption(backend, manifest, destination, None).await
}

pub async fn extract_virtual_files_with_encryption(
    backend: &dyn ObjectBackend,
    manifest: &RepositoryManifest,
    destination: &Path,
    encryption: Option<&PackReadEncryption>,
) -> Result<ExtractReport, MirageError> {
    std::fs::create_dir_all(destination).map_err(MirageError::from)?;
    let mut files_written = 0_u64;
    let mut bytes_written = 0_u64;
    for file in manifest.files.iter().filter(|file| file.class.is_virtual()) {
        let relative = manifest_file_path(manifest, file.parent_directory, &file.name)?;
        let output = destination.join(relative);
        if let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent).map_err(MirageError::from)?;
        }
        let mut target = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&output)
            .map_err(|error| {
                MirageError::new(
                    mirage_types::MirageErrorKind::Io,
                    mirage_types::MirageErrorKind::Io.default_code(),
                    "extract refuses overwrite or cannot create output",
                )
                .with_source(error)
            })?;
        let extent_start = file.extent_start as usize;
        let extent_end = extent_start
            .checked_add(file.extent_count as usize)
            .ok_or_else(|| MirageError::manifest_invalid("extract extent slice overflows"))?;
        for extent in &manifest.extents[extent_start..extent_end] {
            let page_start = extent.page_start as usize;
            let page_end = page_start
                .checked_add(extent.page_count as usize)
                .ok_or_else(|| MirageError::manifest_invalid("extract page slice overflows"))?;
            for page in &manifest.pages[page_start..page_end] {
                let location = &manifest.remote_locations[page.remote_location as usize];
                let range = mirage_types::CheckedRange::new(
                    location.offset,
                    location.encoded_length.as_u64(),
                )?;
                let frame = backend
                    .read_range(
                        &location.object,
                        range,
                        mirage_backend::FetchClass::Maintenance,
                        CancellationToken::new(),
                    )
                    .await?
                    .collect_bounded(32 * 1024 * 1024)
                    .await?;
                let decoded = decode_frame(
                    &frame,
                    manifest,
                    location.offset,
                    page.plaintext_hash,
                    page.logical_length,
                    encryption,
                )?;
                if decoded.page.hash != page.plaintext_hash
                    || decoded.page.logical_len != page.logical_length
                {
                    return Err(MirageError::integrity_mismatch(
                        "extracted page differs from manifest",
                    ));
                }
                target
                    .write_all(&decoded.page.bytes)
                    .map_err(MirageError::from)?;
                bytes_written = bytes_written
                    .checked_add(decoded.page.bytes.len() as u64)
                    .ok_or_else(|| MirageError::invalid_argument("extract byte count overflows"))?;
            }
        }
        target.flush().map_err(MirageError::from)?;
        target.sync_all().map_err(MirageError::from)?;
        files_written += 1;
    }
    Ok(ExtractReport {
        files_written,
        bytes_written,
    })
}

fn decode_frame(
    frame: &[u8],
    manifest: &RepositoryManifest,
    frame_offset: u64,
    hash: mirage_types::PageHash,
    logical_length: u32,
    encryption: Option<&PackReadEncryption>,
) -> Result<DecodedFrame, MirageError> {
    if !frame.starts_with(b"MENCv001") {
        return mirage_pack::decode_plain_frame(frame);
    }
    let encryption = encryption.ok_or_else(|| {
        MirageError::backend_unauthenticated("encrypted pack requires its repository key")
    })?;
    if encryption.repository_id != manifest.repository_id {
        return Err(MirageError::integrity_mismatch(
            "repository key context does not match manifest",
        ));
    }
    let plaintext = mirage_pack::decode_encrypted_frame(
        &encryption.key,
        frame,
        EncryptedFrameAad {
            repository: manifest.repository_id,
            pack_id: mirage_pack::encrypted_frame_pack_id(frame)?,
            frame_index: frame_offset,
            plaintext_hash: hash,
            plaintext_length: logical_length,
        },
    )?;
    Ok(DecodedFrame {
        page: PlainPage {
            hash,
            logical_len: logical_length,
            bytes: plaintext.into(),
        },
        codec: mirage_manifest::Codec::None,
    })
}

fn manifest_file_path(
    manifest: &RepositoryManifest,
    mut directory: u32,
    name: &str,
) -> Result<std::path::PathBuf, MirageError> {
    let mut components = vec![name.to_string()];
    loop {
        let record = manifest
            .directories
            .get(directory as usize)
            .ok_or_else(|| MirageError::manifest_invalid("extract directory index is invalid"))?;
        if record.parent.is_none() {
            break;
        }
        components.push(record.name.clone());
        directory = record.parent.expect("checked parent");
    }
    components.reverse();
    Ok(components.into_iter().collect())
}
