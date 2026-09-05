#![cfg(windows)]
use mirage_crypto::{
    dpapi::ProtectionScope,
    key_store::{load_signer, load_trust_root, save_signer},
    signing::RepositorySigner,
};
use mirage_manifest::CommitSigner;

#[test]
fn protected_signing_key_round_trips_and_corruption_fails() {
    let dir = tempfile::tempdir().expect("temp");
    let path = dir.path().join("repository.key");
    let signer = RepositorySigner::generate().expect("key");
    save_signer(&path, &signer, ProtectionScope::CurrentUser).expect("save");
    assert_eq!(load_signer(&path).expect("load").key_id(), signer.key_id());
    assert_eq!(
        load_trust_root(&path).expect("trust").public_key(),
        signer.verifier().public_key()
    );
    let mut bytes = std::fs::read(&path).expect("read");
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    std::fs::write(&path, bytes).expect("corrupt");
    assert!(load_signer(&path).is_err());
}
