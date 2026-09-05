use bytes::Bytes;
use mirage_types::PageHash;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlainPage {
    pub hash: PageHash,
    pub logical_len: u32,
    pub bytes: Bytes,
}

impl PlainPage {
    #[must_use]
    pub fn from_bytes(bytes: Bytes) -> Self {
        let hash = PageHash::from_bytes(*blake3::hash(&bytes).as_bytes());
        Self {
            hash,
            logical_len: u32::try_from(bytes.len()).unwrap_or(u32::MAX),
            bytes,
        }
    }
}
