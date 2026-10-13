//! Research only: independently authenticated range reads from UNCHANGED legacy
//! ciphertext. Imports the product's actual key and encrypted-frame code.
//! The new sidecar trust/format is experimental and is not installed in Mirage.
extern crate self as mirage_crypto;

#[path = "../../../../crates/mirage-crypto/src/aead.rs"]
pub mod aead;
#[allow(dead_code)]
#[path = "../../../../crates/mirage-pack/src/encrypted_frame.rs"]
mod encrypted_frame;

use aead::RepositoryKey;
use chacha20::XChaCha20;
use chacha20::cipher::{KeyIvInit, StreamCipher, StreamCipherSeek};
use encrypted_frame::{EncryptedFrameAad, decode_encrypted_frame, encode_encrypted_frame};
use mirage_types::{PageHash, RepositoryId};
use serde_json::json;
use std::hint::black_box;
use std::time::Instant;

const PREFIX: usize = 52;
const FRAME: usize = 4 << 20;
type ProbeResult<T> = Result<T, &'static str>;

#[derive(Clone, Copy)]
struct Identity {
    aad: EncryptedFrameAad,
    generation: u64,
    ciphertext_hash: [u8; 32],
    // Separate trust anchor for the sidecar design. It must come from a signed
    // publication or a locally protected record created after full verification.
    // Possession of a decryption key alone must not authorize a new commitment.
    trusted_sidecar_hash: [u8; 32],
}

impl Identity {
    // Supplied by trusted publication metadata, never accepted from the remote
    // sidecar itself. Production must bind provider revision/member location too.
    fn binding(self) -> Vec<u8> {
        let mut v = b"MirageSSD RESEARCH seek-index v1\0".to_vec();
        v.extend_from_slice(self.aad.repository.as_bytes());
        v.extend_from_slice(&self.aad.pack_id);
        v.extend_from_slice(&self.aad.frame_index.to_le_bytes());
        v.extend_from_slice(self.aad.plaintext_hash.as_bytes());
        v.extend_from_slice(&self.aad.plaintext_length.to_le_bytes());
        v.extend_from_slice(&self.generation.to_le_bytes());
        v.extend_from_slice(&self.ciphertext_hash);
        v
    }
}

fn index_key(key: &RepositoryKey) -> RepositoryKey {
    RepositoryKey::from_bytes(blake3::derive_key(
        "MirageSSD RESEARCH seek-index key v1",
        key.secret_bytes().as_ref(),
    ))
}

fn leaf_hash(index: usize, bytes: &[u8]) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(b"MirageSSD RESEARCH ciphertext sector v1\0");
    h.update(&(index as u64).to_le_bytes());
    h.update(&(bytes.len() as u64).to_le_bytes());
    h.update(bytes);
    *h.finalize().as_bytes()
}

fn valid_granule(g: usize) -> bool {
    (4096..=1 << 20).contains(&g) && g.is_power_of_two()
}

fn make_index(
    key: &RepositoryKey,
    legacy: &[u8],
    identity: Identity,
    granule: usize,
) -> ProbeResult<Vec<u8>> {
    if !valid_granule(granule) {
        return Err("invalid geometry");
    }
    // Never generate trusted sector commitments from unverified input.
    let verified = decode_encrypted_frame(key, legacy, identity.aad)
        .map_err(|_| "original AEAD verification failed")?;
    if blake3::hash(legacy).as_bytes() != &identity.ciphertext_hash {
        return Err("wrong source ciphertext");
    }
    let mut body = legacy[..PREFIX].to_vec();
    body.extend_from_slice(&(granule as u32).to_le_bytes());
    for (i, chunk) in legacy[PREFIX..PREFIX + verified.len()]
        .chunks(granule)
        .enumerate()
    {
        body.extend_from_slice(&leaf_hash(i, chunk));
    }
    let (nonce, ciphertext) = index_key(key)
        .seal(&body, &identity.binding())
        .map_err(|_| "index seal failed")?;
    let mut envelope = nonce.to_vec();
    envelope.extend(ciphertext);
    Ok(envelope)
}

struct VerifiedIndex {
    content_key: RepositoryKey,
    header: [u8; PREFIX],
    granule: usize,
    length: usize,
    leaves: Vec<[u8; 32]>,
}

impl VerifiedIndex {
    fn open(key: &RepositoryKey, envelope: &[u8], id: Identity) -> ProbeResult<Self> {
        // Bound resource use before authenticating/parsing untrusted input.
        if !(40..=40 + PREFIX + 4 + FRAME / 4096 * 32).contains(&envelope.len())
            || id.aad.plaintext_length as usize > FRAME
        {
            return Err("index envelope length");
        }
        if blake3::hash(envelope).as_bytes() != &id.trusted_sidecar_hash {
            return Err("untrusted sidecar commitment");
        }
        let nonce = envelope[..24].try_into().map_err(|_| "nonce length")?;
        let body = index_key(key)
            .open(&nonce, &envelope[24..], &id.binding())
            .map_err(|_| "index authentication")?;
        if body.len() < PREFIX + 4 {
            return Err("index body length");
        }
        let header: [u8; PREFIX] = body[..PREFIX].try_into().unwrap();
        let length = id.aad.plaintext_length as usize;
        if &header[..8] != b"MENCv001"
            || header[12..28] != id.aad.pack_id
            || u32::from_le_bytes(header[8..12].try_into().unwrap()) as usize != length + 16
        {
            return Err("legacy header binding");
        }
        let granule = u32::from_le_bytes(body[PREFIX..PREFIX + 4].try_into().unwrap()) as usize;
        if !valid_granule(granule) || body.len() != PREFIX + 4 + length.div_ceil(granule) * 32 {
            return Err("index geometry or coverage");
        }
        let leaves = body[PREFIX + 4..]
            .chunks_exact(32)
            .map(|h| h.try_into().unwrap())
            .collect();
        Ok(Self {
            content_key: RepositoryKey::from_bytes(*key.secret_bytes()),
            header,
            granule,
            length,
            leaves,
        })
    }

    fn read(
        &self,
        offset: usize,
        length: usize,
        mut fetch: impl FnMut(usize, usize) -> ProbeResult<Vec<u8>>,
    ) -> ProbeResult<Vec<u8>> {
        let end = offset.checked_add(length).ok_or("range overflow")?;
        if end > self.length {
            return Err("range beyond EOF");
        }
        if length == 0 {
            return Ok(Vec::new());
        }
        let first = offset / self.granule;
        let last = (end - 1) / self.granule;
        let aligned = first * self.granule;
        let needed = ((last + 1) * self.granule).min(self.length) - aligned;
        // One contiguous range, regardless of the number of covered sectors.
        let mut ciphertext = fetch(PREFIX + aligned, needed)?;
        if ciphertext.len() != needed {
            return Err("short or oversized range");
        }
        for (i, bytes) in ciphertext.chunks(self.granule).enumerate() {
            if leaf_hash(first + i, bytes) != self.leaves[first + i] {
                return Err("ciphertext sector mismatch");
            }
        }
        // Only authenticated ciphertext reaches decryption. The nonce is in the
        // authenticated index. AEAD payload starts at stream block counter 1.
        let raw_key = self.content_key.secret_bytes();
        let nonce: [u8; 24] = self.header[28..52].try_into().unwrap();
        let mut cipher = XChaCha20::new(raw_key.as_ref().into(), (&nonce).into());
        cipher
            .try_seek(64_u64 + aligned as u64)
            .map_err(|_| "cipher seek")?;
        cipher
            .try_apply_keystream(&mut ciphertext)
            .map_err(|_| "cipher range")?;
        Ok(ciphertext[offset - aligned..end - aligned].to_vec())
    }
}

fn fixture(length: usize, salt: u64, key: &RepositoryKey) -> (Vec<u8>, Vec<u8>, Identity) {
    let mut state = salt + 1;
    let plain: Vec<u8> = (0..length)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect();
    let aad = EncryptedFrameAad {
        repository: RepositoryId::from_bytes([0x13; 16]),
        pack_id: [0x27; 16],
        frame_index: salt,
        plaintext_hash: PageHash::from_bytes(*blake3::hash(&plain).as_bytes()),
        plaintext_length: length as u32,
    };
    let encoded = encode_encrypted_frame(key, &plain, aad).unwrap();
    let identity = Identity {
        aad,
        generation: 10,
        ciphertext_hash: *blake3::hash(&encoded).as_bytes(),
        trusted_sidecar_hash: [0; 32],
    };
    (plain, encoded, identity)
}

mod experiments;
mod root_proof;

fn main() {
    let result = if std::env::args().nth(1).as_deref() == Some("--transport") {
        root_proof::transport_run()
    } else {
        experiments::run()
    };
    println!("{}", serde_json::to_string_pretty(&result).unwrap());
}
