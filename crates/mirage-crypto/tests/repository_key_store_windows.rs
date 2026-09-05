#![cfg(windows)]

use mirage_crypto::{
    aead::RepositoryKey,
    dpapi::ProtectionScope,
    repository_key_store::{load_repository_key, save_repository_key},
};
use mirage_types::RepositoryId;

#[test]
fn protected_repository_key_round_trips_and_is_repository_bound() {
    let directory = tempfile::tempdir().expect("temp");
    let path = directory.path().join("repository-key.dpapi");
    let repository_id = RepositoryId::from_bytes([7; 16]);
    let key = RepositoryKey::generate().expect("key");
    save_repository_key(&path, repository_id, &key, ProtectionScope::CurrentUser).expect("save");
    let loaded = load_repository_key(&path, repository_id).expect("load");
    assert_eq!(loaded.secret_bytes().as_ref(), key.secret_bytes().as_ref());
    assert!(load_repository_key(&path, RepositoryId::from_bytes([8; 16])).is_err());
}
