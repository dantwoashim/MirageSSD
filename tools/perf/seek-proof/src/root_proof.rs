//! Preferred experiment: use the EXISTING trusted plaintext BLAKE3 root.
//! Proofs and the legacy header are untrusted. No new index trust authority.
//! This is a prototype tree codec, not a production cryptographic format.
use super::*;
use blake3::hazmat::{
    HasherExt, Mode, left_subtree_len, merge_subtrees_non_root, merge_subtrees_root,
};
use zeroize::Zeroizing;

type Cv = [u8; 32];

struct Tree {
    start: usize,
    length: usize,
    cv: Cv,
    children: Option<(Box<Tree>, Box<Tree>)>,
}

impl Tree {
    fn build(plain: &[u8], start: usize, granule: usize) -> Self {
        assert!(!plain.is_empty());
        if plain.len() <= granule {
            let cv = blake3::Hasher::new()
                .set_input_offset(start as u64)
                .update(plain)
                .finalize_non_root();
            Self {
                start,
                length: plain.len(),
                cv,
                children: None,
            }
        } else {
            let split = left_subtree_len(plain.len() as u64) as usize;
            let left = Box::new(Self::build(&plain[..split], start, granule));
            let right = Box::new(Self::build(&plain[split..], start + split, granule));
            let cv = merge_subtrees_non_root(&left.cv, &right.cv, Mode::Hash);
            Self {
                start,
                length: plain.len(),
                cv,
                children: Some((left, right)),
            }
        }
    }

    fn proof(&self, sector_start: usize) -> Vec<Cv> {
        match &self.children {
            None => {
                assert_eq!(self.start, sector_start);
                Vec::new()
            }
            Some((left, right)) if sector_start < right.start => {
                let mut proof = left.proof(sector_start);
                proof.push(right.cv);
                proof
            }
            Some((left, right)) => {
                let mut proof = right.proof(sector_start);
                proof.push(left.cv);
                proof
            }
        }
    }

    fn assert_standard_root(&self, whole_plain: &[u8]) {
        if let Some((left, right)) = &self.children {
            assert_eq!(
                merge_subtrees_root(&left.cv, &right.cv, Mode::Hash),
                blake3::hash(whole_plain)
            );
        } else {
            assert_eq!(self.length, whole_plain.len());
        }
    }
}

fn rebuild(
    plain_sector: &[u8],
    sector_start: usize,
    node_start: usize,
    node_length: usize,
    granule: usize,
    siblings: &mut std::slice::Iter<'_, Cv>,
    root: bool,
) -> ProbeResult<Cv> {
    if node_length <= granule {
        if node_start != sector_start || plain_sector.len() != node_length {
            return Err("wrong leaf coverage");
        }
        if root {
            return Ok(*blake3::hash(plain_sector).as_bytes());
        }
        return Ok(blake3::Hasher::new()
            .set_input_offset(node_start as u64)
            .update(plain_sector)
            .finalize_non_root());
    }
    let split = left_subtree_len(node_length as u64) as usize;
    let (left, right) = if sector_start < node_start + split {
        let left = rebuild(
            plain_sector,
            sector_start,
            node_start,
            split,
            granule,
            siblings,
            false,
        )?;
        let right = *siblings.next().ok_or("missing proof sibling")?;
        (left, right)
    } else {
        let right = rebuild(
            plain_sector,
            sector_start,
            node_start + split,
            node_length - split,
            granule,
            siblings,
            false,
        )?;
        let left = *siblings.next().ok_or("missing proof sibling")?;
        (left, right)
    };
    Ok(if root {
        *merge_subtrees_root(&left, &right, Mode::Hash).as_bytes()
    } else {
        merge_subtrees_non_root(&left, &right, Mode::Hash)
    })
}

fn verify_sector(
    plain: &[u8],
    start: usize,
    aad: EncryptedFrameAad,
    granule: usize,
    proof: &[Cv],
) -> ProbeResult<()> {
    let mut siblings = proof.iter();
    let actual = rebuild(
        plain,
        start,
        0,
        aad.plaintext_length as usize,
        granule,
        &mut siblings,
        true,
    )?;
    if siblings.next().is_some() || &actual != aad.plaintext_hash.as_bytes() {
        return Err("existing plaintext root mismatch");
    }
    Ok(())
}

fn read(
    key: &RepositoryKey,
    aad: EncryptedFrameAad,
    granule: usize,
    offset: usize,
    length: usize,
    untrusted_header: &[u8; PREFIX],
    untrusted_proofs: &[Vec<Cv>],
    mut fetch: impl FnMut(usize, usize) -> ProbeResult<Vec<u8>>,
) -> ProbeResult<Vec<u8>> {
    let total = aad.plaintext_length as usize;
    let end = offset.checked_add(length).ok_or("range overflow")?;
    if !valid_granule(granule) || total > FRAME || end > total {
        return Err("invalid range or geometry");
    }
    if &untrusted_header[..8] != b"MENCv001"
        || untrusted_header[12..28] != aad.pack_id
        || u32::from_le_bytes(untrusted_header[8..12].try_into().unwrap()) as usize != total + 16
    {
        return Err("header mismatch");
    }
    if length == 0 {
        if total == 0 && blake3::hash(&[]).as_bytes() != aad.plaintext_hash.as_bytes() {
            return Err("empty plaintext root mismatch");
        }
        return if untrusted_proofs.is_empty() {
            Ok(Vec::new())
        } else {
            Err("extra empty-read proof")
        };
    }
    let first = offset / granule;
    let last = (end - 1) / granule;
    if untrusted_proofs.len() != last - first + 1 || untrusted_proofs.iter().any(|p| p.len() > 10) {
        return Err("proof coverage or size");
    }
    let aligned = first * granule;
    let needed = ((last + 1) * granule).min(total) - aligned;
    let mut plaintext = Zeroizing::new(fetch(PREFIX + aligned, needed)?);
    if plaintext.len() != needed {
        return Err("range response length");
    }
    let secret = key.secret_bytes();
    let nonce: [u8; 24] = untrusted_header[28..].try_into().unwrap();
    let mut cipher = XChaCha20::new(secret.as_ref().into(), (&nonce).into());
    cipher
        .try_seek(64_u64 + aligned as u64)
        .map_err(|_| "stream seek")?;
    cipher
        .try_apply_keystream(&mut plaintext)
        .map_err(|_| "stream length")?;
    // Never expose any decrypted output until EVERY covered sector proves to
    // the existing trusted root. Wrong nonce/key/proof/ciphertext all fail here.
    for (i, sector) in plaintext.chunks(granule).enumerate() {
        verify_sector(
            sector,
            aligned + i * granule,
            aad,
            granule,
            &untrusted_proofs[i],
        )?;
    }
    Ok(plaintext[offset - aligned..end - aligned].to_vec())
}

fn proofs(tree: Option<&Tree>, offset: usize, length: usize, granule: usize) -> Vec<Vec<Cv>> {
    if length == 0 {
        return Vec::new();
    }
    (offset / granule..=(offset + length - 1) / granule)
        .map(|i| tree.unwrap().proof(i * granule))
        .collect()
}

#[path = "root_experiments.rs"]
mod experiments;
mod transport;

pub(super) fn run(key: &RepositoryKey) -> serde_json::Value {
    experiments::run(key)
}

pub(super) fn transport_run() -> serde_json::Value {
    transport::run()
}
