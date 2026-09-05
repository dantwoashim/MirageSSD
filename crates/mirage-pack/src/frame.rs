use bytes::Bytes;
use mirage_manifest::Codec;
use mirage_types::{MirageError, PageHash};

use crate::encode::{crc32, read_u16, read_u32, take};
use crate::format::{FRAME_HEADER_LEN, MAX_ENCODED_FRAME_LEN};
use crate::page::PlainPage;

const FRAME_MAGIC: &[u8; 4] = b"MPF1";
const FRAME_VERSION: u16 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedFrame {
    pub page: PlainPage,
    pub codec: Codec,
}

pub fn encode_plain_frame(page: &PlainPage) -> Result<Vec<u8>, MirageError> {
    if page.bytes.len() != page.logical_len as usize || page.logical_len == 0 {
        return Err(MirageError::invalid_argument(
            "plain page length is inconsistent",
        ));
    }
    if page.logical_len > MAX_ENCODED_FRAME_LEN {
        return Err(MirageError::invalid_argument(
            "plain page exceeds the frame bound",
        ));
    }
    let actual_hash = blake3::hash(&page.bytes);
    if actual_hash.as_bytes() != page.hash.as_bytes() {
        return Err(MirageError::integrity_mismatch(
            "plain page hash does not match its bytes",
        ));
    }
    let mut output = vec![0_u8; FRAME_HEADER_LEN + page.bytes.len()];
    output[0..4].copy_from_slice(FRAME_MAGIC);
    output[4..6].copy_from_slice(&FRAME_VERSION.to_le_bytes());
    output[8..40].copy_from_slice(page.hash.as_bytes());
    output[40..44].copy_from_slice(&page.logical_len.to_le_bytes());
    output[44..48].copy_from_slice(&page.logical_len.to_le_bytes());
    output[48] = Codec::None.code();
    let checksum = crc32(&output[..52]);
    output[52..56].copy_from_slice(&checksum.to_le_bytes());
    output[FRAME_HEADER_LEN..].copy_from_slice(&page.bytes);
    Ok(output)
}

pub fn decode_plain_frame(bytes: &[u8]) -> Result<DecodedFrame, MirageError> {
    if bytes.len() < FRAME_HEADER_LEN || &bytes[..4] != FRAME_MAGIC {
        return Err(MirageError::manifest_invalid(
            "frame magic or length is invalid",
        ));
    }
    if read_u16(bytes, 4)? != FRAME_VERSION {
        return Err(MirageError::unsupported_layout(
            "frame version is unsupported",
        ));
    }
    if read_u16(bytes, 6)? != 0
        || bytes[49] != 0
        || read_u16(bytes, 50)? != 0
        || take::<12>(bytes, 52)?[4..] != [0; 8]
    {
        return Err(MirageError::unsupported_layout(
            "frame flags, encryption metadata, or reserved bytes are unsupported",
        ));
    }
    if read_u32(bytes, 52)? != crc32(&bytes[..52]) {
        return Err(MirageError::integrity_mismatch(
            "frame header checksum is invalid",
        ));
    }
    let logical_len = read_u32(bytes, 40)?;
    let encoded_len = read_u32(bytes, 44)?;
    if logical_len == 0
        || encoded_len == 0
        || encoded_len > MAX_ENCODED_FRAME_LEN
        || encoded_len != logical_len
    {
        return Err(MirageError::manifest_invalid(
            "frame lengths are invalid for codec none",
        ));
    }
    if bytes[48] != Codec::None.code() {
        return Err(MirageError::unsupported_layout(
            "frame codec is unsupported",
        ));
    }
    let payload_end = FRAME_HEADER_LEN
        .checked_add(encoded_len as usize)
        .ok_or_else(|| MirageError::manifest_invalid("frame payload length overflowed"))?;
    if payload_end != bytes.len() {
        return Err(MirageError::manifest_invalid(
            "frame payload length does not match its range",
        ));
    }
    let expected = PageHash::from_bytes(take(bytes, 8)?);
    let payload = &bytes[FRAME_HEADER_LEN..payload_end];
    if blake3::hash(payload).as_bytes() != expected.as_bytes() {
        return Err(MirageError::integrity_mismatch(
            "frame plaintext hash is invalid",
        ));
    }
    Ok(DecodedFrame {
        page: PlainPage {
            hash: expected,
            logical_len,
            bytes: Bytes::copy_from_slice(payload),
        },
        codec: Codec::None,
    })
}
