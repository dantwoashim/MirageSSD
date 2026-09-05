use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PatternKind {
    Pseudorandom,
    PageRecognizable,
    RepeatedQuarter,
    ChangedQuarter,
}

#[must_use]
pub fn oracle_byte(seed: u64, pattern: PatternKind, offset: u64) -> u8 {
    match pattern {
        PatternKind::Pseudorandom => random_byte(seed, offset),
        PatternKind::PageRecognizable => {
            let within = offset % (1024 * 1024);
            if within < 8 {
                (offset / (1024 * 1024)).to_le_bytes()[within as usize] ^ seed as u8
            } else {
                random_byte(seed, offset)
            }
        }
        PatternKind::RepeatedQuarter => random_byte(seed, offset % (16 * 1024 * 1024)),
        PatternKind::ChangedQuarter => {
            let page = offset / (1024 * 1024);
            let selected = if page.is_multiple_of(4) {
                seed ^ 0xa5a5_a5a5_a5a5_a5a5
            } else {
                seed
            };
            random_byte(selected, offset)
        }
    }
}

pub fn fill_at(seed: u64, pattern: PatternKind, offset: u64, output: &mut [u8]) {
    for (index, byte) in output.iter_mut().enumerate() {
        *byte = oracle_byte(seed, pattern, offset.saturating_add(index as u64));
    }
}

fn random_byte(seed: u64, offset: u64) -> u8 {
    let block = offset / 8;
    let mixed = splitmix64(seed ^ block.wrapping_mul(0x9e37_79b9_7f4a_7c15));
    mixed.to_le_bytes()[(offset % 8) as usize]
}

const fn splitmix64(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}
