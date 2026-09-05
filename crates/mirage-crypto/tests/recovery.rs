use mirage_crypto::{
    recovery::{KeyTransition, export_signer, import_signer},
    signing::RepositorySigner,
};
use mirage_manifest::CommitSigner;

#[test]
fn recovery_export_and_signed_rotation_fail_closed() {
    let old = RepositorySigner::generate().expect("old");
    let new = RepositorySigner::generate().expect("new");
    let exported = export_signer(&old, b"correct horse battery").expect("export");
    assert!(import_signer(&exported, b"wrong password here").is_err());
    let recovered = import_signer(&exported, b"correct horse battery").expect("import");
    assert_eq!(recovered.key_id(), old.key_id());
    let transition = KeyTransition::create(2, &old, &new.verifier()).expect("transition");
    assert_eq!(
        transition
            .verify(&old.verifier())
            .expect("verify")
            .public_key(),
        new.verifier().public_key()
    );
    assert!(transition.verify(&new.verifier()).is_err());
}
