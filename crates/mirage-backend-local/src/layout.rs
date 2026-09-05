use std::path::{Path, PathBuf};

use mirage_backend::{
    BackendError, BackendErrorClass, ObjectKind, ProviderObjectId, RemoteObjectRef,
};
use mirage_types::{ContentHash, RepositoryId};

use crate::backend::local_error;

pub(crate) fn repository_root(root: &Path, repository: RepositoryId) -> PathBuf {
    root.join("repositories").join(repository.to_string())
}

pub(crate) fn object_id(kind: ObjectKind, hash: ContentHash) -> ProviderObjectId {
    ProviderObjectId::new(format!("{}/{}.bin", kind_directory(kind), hash))
        .expect("content-addressed local object ID is valid")
}

pub(crate) fn object_path(
    repository_root: &Path,
    object: &RemoteObjectRef,
) -> Result<PathBuf, BackendError> {
    let expected = format!(
        "{}/{}.bin",
        kind_directory(object.kind),
        object.content_hash
    );
    if object.provider_object_id.as_str() != expected {
        return Err(local_error(
            BackendErrorClass::Permanent,
            "local object ID is not canonical for its kind and content hash",
        ));
    }
    Ok(repository_root
        .join(kind_directory(object.kind))
        .join(format!("{}.bin", object.content_hash)))
}

pub(crate) const fn kind_directory(kind: ObjectKind) -> &'static str {
    match kind {
        ObjectKind::RepositoryConfig => "config",
        ObjectKind::Pack => "packs",
        ObjectKind::Manifest => "manifests",
        ObjectKind::Commit => "commits",
        ObjectKind::Profile => "profiles",
    }
}
