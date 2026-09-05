use bytes::Bytes;
use mirage_manifest::Codec;
use mirage_pack::{
    DecodedFrame, EncryptedFrameAad, PackReadEncryption, PlainPage, decode_encrypted_frame,
    decode_plain_frame, encrypted_frame_pack_id,
};
use mirage_types::{MirageError, PageHash};

pub fn decode_expected(bytes: &[u8], expected: PageHash) -> Result<DecodedFrame, MirageError> {
    let decoded = decode_plain_frame(bytes)?;
    if decoded.page.hash != expected {
        return Err(MirageError::integrity_mismatch(
            "decoded frame identity differs from requested page",
        ));
    }
    Ok(decoded)
}

pub fn decode_expected_with_encryption(
    bytes: &[u8],
    expected: PageHash,
    frame_offset: u64,
    encryption: Option<&PackReadEncryption>,
) -> Result<DecodedFrame, MirageError> {
    if !bytes.starts_with(b"MENCv001") {
        return decode_expected(bytes, expected);
    }
    let encryption = encryption.ok_or_else(|| {
        MirageError::backend_unauthenticated("encrypted pack requires its repository key")
    })?;
    let logical_length = bytes
        .len()
        .checked_sub(68)
        .and_then(|length| u32::try_from(length).ok())
        .filter(|length| *length != 0)
        .ok_or_else(|| MirageError::integrity_mismatch("encrypted frame length is invalid"))?;
    let plaintext = decode_encrypted_frame(
        &encryption.key,
        bytes,
        EncryptedFrameAad {
            repository: encryption.repository_id,
            pack_id: encrypted_frame_pack_id(bytes)?,
            frame_index: frame_offset,
            plaintext_hash: expected,
            plaintext_length: logical_length,
        },
    )?;
    Ok(DecodedFrame {
        page: PlainPage {
            hash: expected,
            logical_len: logical_length,
            bytes: Bytes::from(plaintext),
        },
        codec: Codec::None,
    })
}
