use mirage_types::MirageError;
use serde::{Serialize, de::DeserializeOwned};
pub const PROTOCOL_VERSION: u16 = 3;
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;
pub fn encode_frame<T: Serialize>(value: &T) -> Result<Vec<u8>, MirageError> {
    let payload = serde_json::to_vec(value)
        .map_err(|_| MirageError::invalid_argument("IPC message cannot be serialized"))?;
    if payload.len() > MAX_FRAME_BYTES {
        return Err(MirageError::invalid_argument("IPC frame exceeds maximum"));
    }
    let length = u32::try_from(payload.len())
        .map_err(|_| MirageError::invalid_argument("IPC frame length overflows"))?;
    let mut frame = Vec::with_capacity(4 + payload.len());
    frame.extend_from_slice(&length.to_le_bytes());
    frame.extend_from_slice(&payload);
    Ok(frame)
}
pub fn decode_frame<T: DeserializeOwned>(frame: &[u8]) -> Result<T, MirageError> {
    if frame.len() < 4 {
        return Err(MirageError::invalid_argument("IPC frame is truncated"));
    }
    let declared = u32::from_le_bytes(frame[..4].try_into().unwrap()) as usize;
    if declared > MAX_FRAME_BYTES || declared != frame.len() - 4 {
        return Err(MirageError::invalid_argument("IPC frame length is invalid"));
    }
    serde_json::from_slice(&frame[4..])
        .map_err(|_| MirageError::invalid_argument("IPC payload is malformed"))
}
