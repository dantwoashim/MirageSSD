use mirage_crypto::signing::RepositorySigner;
use mirage_manifest::{CommitSigner, CommitVerifier, SignatureAlgorithm, SignatureEnvelope};

#[test]
fn production_signatures_reject_mutation_and_wrong_keys() {
    let signer = RepositorySigner::generate().expect("key");
    let other = RepositorySigner::generate().expect("other key");
    let body = b"canonical commit";
    let envelope = SignatureEnvelope {
        algorithm: SignatureAlgorithm::Ed25519,
        key_id: signer.key_id(),
        signature: signer.sign(body).expect("sign"),
    };
    signer.verifier().verify(body, &envelope).expect("verify");
    assert!(signer.verifier().verify(b"changed", &envelope).is_err());
    assert!(other.verifier().verify(body, &envelope).is_err());
}
