use minicbor::Decoder;
use mirage_types::MirageError;

/// Tracks integer keys in one exact CBOR map and rejects duplicates/unknowns.
#[derive(Debug, Default)]
pub(crate) struct FieldSet(u64);

impl FieldSet {
    pub(crate) fn insert(&mut self, key: u32, maximum: u32) -> Result<(), MirageError> {
        if key == 0 || key > maximum || key >= 64 {
            return Err(MirageError::manifest_invalid(
                "CBOR map contains an unknown field",
            ));
        }
        let bit = 1_u64 << key;
        if self.0 & bit != 0 {
            return Err(MirageError::manifest_invalid(
                "CBOR map contains a duplicate field",
            ));
        }
        self.0 |= bit;
        Ok(())
    }

    pub(crate) fn require_all(&self, maximum: u32) -> Result<(), MirageError> {
        let required = (1..=maximum).fold(0_u64, |bits, key| bits | (1_u64 << key));
        if self.0 != required {
            return Err(MirageError::manifest_invalid(
                "CBOR map is missing a required field",
            ));
        }
        Ok(())
    }
}

pub(crate) fn definite_map(
    decoder: &mut Decoder<'_>,
    expected_fields: u64,
) -> Result<u64, MirageError> {
    let fields = decoder
        .map()
        .map_err(decode_error)?
        .ok_or_else(|| MirageError::manifest_invalid("indefinite CBOR maps are forbidden"))?;
    if fields != expected_fields {
        return Err(MirageError::manifest_invalid(
            "CBOR map has an unexpected field count",
        ));
    }
    Ok(fields)
}

pub(crate) fn definite_array(
    decoder: &mut Decoder<'_>,
    maximum: usize,
    label: &str,
) -> Result<usize, MirageError> {
    let count = decoder
        .array()
        .map_err(decode_error)?
        .ok_or_else(|| MirageError::manifest_invalid("indefinite CBOR arrays are forbidden"))?;
    let count = usize::try_from(count)
        .map_err(|_| MirageError::manifest_invalid("CBOR array count overflows usize"))?;
    if count > maximum {
        return Err(MirageError::manifest_invalid(format!(
            "declared {label} count exceeds the decode budget"
        )));
    }
    Ok(count)
}

pub(crate) fn decode_error(error: minicbor::decode::Error) -> MirageError {
    MirageError::manifest_invalid(format!("invalid CBOR: {error}"))
}
