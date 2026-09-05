use mirage_crypto::aead::RepositoryKey;
use mirage_pack::{EncryptedFrameAad, decode_encrypted_frame, encode_encrypted_frame};
use mirage_types::{PageHash, RepositoryId};

fn aad(page: &[u8]) -> EncryptedFrameAad {
    EncryptedFrameAad {
        repository: RepositoryId::from_bytes([1; 16]),
        pack_id: [2; 16],
        frame_index: 3,
        plaintext_hash: PageHash::from_bytes(*blake3::hash(page).as_bytes()),
        plaintext_length: page.len() as u32,
    }
}
#[test]
fn encrypted_frames_authenticate_bytes_and_all_metadata() {
    let key = RepositoryKey::generate().expect("key");
    let page = b"random-access page";
    let metadata = aad(page);
    let encoded = encode_encrypted_frame(&key, page, metadata).expect("encode");
    assert_eq!(
        decode_encrypted_frame(&key, &encoded, metadata).expect("decode"),
        page
    );
    let mut changed = encoded.clone();
    let last = changed.len() - 1;
    changed[last] ^= 1;
    assert!(decode_encrypted_frame(&key, &changed, metadata).is_err());
    let mut wrong = metadata;
    wrong.frame_index += 1;
    assert!(decode_encrypted_frame(&key, &encoded, wrong).is_err());
    let mut wrong_pack = metadata;
    wrong_pack.pack_id = [9; 16];
    assert!(decode_encrypted_frame(&key, &encoded, wrong_pack).is_err());
    assert!(
        decode_encrypted_frame(
            &RepositoryKey::generate().expect("wrong"),
            &encoded,
            metadata
        )
        .is_err()
    );
}
