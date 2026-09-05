use std::collections::HashMap;

use mirage_types::MirageError;

use crate::record::StringRef;

#[derive(Debug, Default)]
pub struct StringInterner {
    bytes: Vec<u8>,
    entries: HashMap<String, StringRef>,
}

impl StringInterner {
    pub fn intern(&mut self, value: &str) -> Result<StringRef, MirageError> {
        if let Some(existing) = self.entries.get(value) {
            return Ok(*existing);
        }
        let reference = StringRef {
            offset: u64::try_from(self.bytes.len()).map_err(|_| {
                MirageError::manifest_invalid("index string table offset overflows")
            })?,
            length: u32::try_from(value.len())
                .map_err(|_| MirageError::manifest_invalid("index string length exceeds u32"))?,
        };
        self.bytes.extend_from_slice(value.as_bytes());
        self.entries.insert(value.to_string(), reference);
        Ok(reference)
    }

    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}
