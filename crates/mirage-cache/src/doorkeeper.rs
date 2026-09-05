use std::sync::atomic::{AtomicU64, Ordering};

use mirage_types::{MirageError, PageHash};

pub struct Doorkeeper {
    words: Box<[AtomicU64]>,
    mask: usize,
    seed: u64,
}

impl Doorkeeper {
    pub fn new(bit_count: usize, seed: u64) -> Result<Self, MirageError> {
        if bit_count < 64 || !bit_count.is_power_of_two() {
            return Err(MirageError::invalid_argument(
                "doorkeeper size must be a power of two at least 64",
            ));
        }
        Ok(Self {
            words: (0..bit_count / 64).map(|_| AtomicU64::new(0)).collect(),
            mask: bit_count - 1,
            seed,
        })
    }

    pub fn admit(&self, hash: PageHash) -> bool {
        let index = index(hash, self.seed, self.mask);
        let bit = 1_u64 << (index & 63);
        self.words[index >> 6].fetch_or(bit, Ordering::Relaxed) & bit != 0
    }

    pub fn contains(&self, hash: PageHash) -> bool {
        let index = index(hash, self.seed, self.mask);
        let bit = 1_u64 << (index & 63);
        self.words[index >> 6].load(Ordering::Relaxed) & bit != 0
    }

    pub fn clear(&self) {
        for word in &self.words {
            word.store(0, Ordering::Relaxed);
        }
    }
}

pub(crate) fn index(hash: PageHash, seed: u64, mask: usize) -> usize {
    let mut input = [0_u8; 40];
    input[..32].copy_from_slice(hash.as_bytes());
    input[32..].copy_from_slice(&seed.to_le_bytes());
    let bytes = blake3::hash(&input);
    usize::from_le_bytes(
        bytes.as_bytes()[..size_of::<usize>()]
            .try_into()
            .expect("slice"),
    ) & mask
}
