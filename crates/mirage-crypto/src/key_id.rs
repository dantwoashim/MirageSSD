#[must_use]
pub fn derive_key_id(public_key: &[u8; 32]) -> [u8; 16] {
    let hash = blake3::hash(public_key);
    let mut id = [0_u8; 16];
    id.copy_from_slice(&hash.as_bytes()[..16]);
    id
}
