#![cfg(windows)]
use mirage_backend_drive::token_store::TokenStore;
use zeroize::Zeroizing;

#[test]
fn refresh_token_is_encrypted_redacted_and_deleted() {
    let directory = tempfile::tempdir().unwrap();
    let store = TokenStore::new(directory.path().join("drive-token.json"));
    let secret = b"refresh-secret-123".to_vec();
    let mut token = Zeroizing::new(secret.clone());
    let scopes = vec!["https://www.googleapis.com/auth/drive.file".to_owned()];
    store
        .save(
            "123456.apps.googleusercontent.com",
            "account@example.test",
            &scopes,
            42,
            &mut token,
        )
        .unwrap();
    assert!(token.iter().all(|byte| *byte == 0));
    let disk = std::fs::read(store.path()).unwrap();
    assert!(!disk.windows(secret.len()).any(|part| part == secret));
    let (metadata, loaded) = store.load().unwrap();
    assert_eq!(metadata.account_id, "account@example.test");
    assert_eq!(
        metadata.client_id.as_deref(),
        Some("123456.apps.googleusercontent.com")
    );
    assert_eq!(&*loaded, &secret);
    assert_eq!(store.metadata().unwrap(), metadata);
    assert!(store.load_client_secret_optional().unwrap().is_none());
    let client_secret = b"desktop-client-secret".to_vec();
    let mut protected_client_secret = Zeroizing::new(client_secret.clone());
    store
        .bind_client_secret(
            "123456.apps.googleusercontent.com",
            &mut protected_client_secret,
        )
        .unwrap();
    assert!(protected_client_secret.iter().all(|byte| *byte == 0));
    let disk = std::fs::read(store.path()).unwrap();
    assert!(
        !disk
            .windows(client_secret.len())
            .any(|part| part == client_secret)
    );
    assert_eq!(
        &*store.load_client_secret_optional().unwrap().unwrap(),
        &client_secret
    );
    assert_eq!(&*store.load_client_secret().unwrap(), &client_secret);
    assert_eq!(&*store.load().unwrap().1, &secret);
    assert!(store.delete().unwrap());
    assert!(!store.delete().unwrap());
}
