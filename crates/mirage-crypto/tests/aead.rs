use mirage_crypto::aead::RepositoryKey;

#[test]
fn authenticated_pages_have_unique_nonces_and_reject_tampering() {
    let key = RepositoryKey::generate().expect("key");
    let (nonce1, ciphertext1) = key.seal(b"page", b"repo/pack/frame/hash/4").expect("seal");
    let (nonce2, _) = key
        .seal(b"page", b"repo/pack/frame/hash/4")
        .expect("seal again");
    assert_ne!(nonce1, nonce2);
    assert_eq!(
        key.open(&nonce1, &ciphertext1, b"repo/pack/frame/hash/4")
            .expect("open"),
        b"page"
    );
    let mut changed = ciphertext1;
    changed[0] ^= 1;
    assert!(
        key.open(&nonce1, &changed, b"repo/pack/frame/hash/4")
            .is_err()
    );
    assert!(key.open(&nonce1, &changed, b"wrong aad").is_err());
}
