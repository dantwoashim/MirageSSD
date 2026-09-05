#![cfg(windows)]
use mirage_crypto::dpapi::{ProtectionScope, protect, unprotect};
#[test]
fn user_bound_round_trip_rejects_wrong_context_and_corruption() {
    let plaintext = b"refresh-token-never-log";
    let entropy = b"miragessd/oauth/v1";
    let encrypted = protect(plaintext, entropy, ProtectionScope::CurrentUser).expect("protect");
    assert!(
        !encrypted
            .windows(plaintext.len())
            .any(|part| part == plaintext)
    );
    assert_eq!(
        &*unprotect(&encrypted, entropy).expect("unprotect"),
        plaintext
    );
    assert!(unprotect(&encrypted, b"wrong-context").is_err());
    let mut corrupt = encrypted;
    let middle = corrupt.len() / 2;
    corrupt[middle] ^= 0x80;
    assert!(unprotect(&corrupt, entropy).is_err());
}
