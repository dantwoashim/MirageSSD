use mirage_types::{ManifestHash, MirageError};

use crate::{RepositoryManifest, encode_manifest};

pub fn manifest_hash(manifest: &RepositoryManifest) -> Result<ManifestHash, MirageError> {
    let bytes = encode_manifest(manifest)?;
    Ok(ManifestHash::from_bytes(*blake3::hash(&bytes).as_bytes()))
}
