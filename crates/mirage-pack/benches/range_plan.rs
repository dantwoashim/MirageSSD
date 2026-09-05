use std::time::Instant;

use mirage_manifest::Codec;
use mirage_pack::{PackEntry, plan_ranges};
use mirage_types::PageHash;

fn main() {
    let entries: Vec<_> = (0_u32..100_000)
        .map(|index| PackEntry {
            page_hash: PageHash::from_bytes(*blake3::hash(&index.to_le_bytes()).as_bytes()),
            frame_offset: 64 + u64::from(index) * 69_632,
            frame_length: 65_600,
            logical_length: 65_536,
            encoded_length: 65_536,
            codec: Codec::None,
            encrypted: false,
        })
        .collect();
    let pack_length = entries
        .last()
        .map_or(64, |entry| entry.frame_offset + entry.frame_length + 128);
    let started = Instant::now();
    let planned = plan_ranges(&entries, pack_length, 4096, 1024 * 1024).expect("valid range plan");
    println!(
        "entries={} ranges={} elapsed_us={}",
        entries.len(),
        planned.len(),
        started.elapsed().as_micros()
    );
}
