//! Standalone, source-inspired mechanism benchmark, not the mounted engine.
//! Factor thread creation and full-frame cloning independently. All variants
//! return the same 4 KiB and own their frame data across cache eviction.
use std::collections::HashMap;
use std::hint::black_box;
use std::sync::{Arc, Mutex};
use std::time::Instant;

const FRAME: usize = 4 << 20;
const READ: usize = 4096;

fn main() {
    let owned = Mutex::new(
        (0..8u64)
            .map(|k| {
                (
                    k,
                    (0..FRAME)
                        .map(|i| ((i + k as usize) % 251) as u8)
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<HashMap<_, _>>(),
    );
    let shared = Mutex::new(
        owned
            .lock()
            .unwrap()
            .iter()
            .map(|(k, v)| (*k, Arc::<[u8]>::from(v.clone())))
            .collect::<HashMap<_, _>>(),
    );
    let mut samples = Vec::new();
    for round in 0..8 {
        // Every variant occupies every execution position twice.
        for position in 0..4 {
            let variant = (round + position) % 4;
            let mut times = Vec::new();
            let mut checksum = 0u64;
            for i in 0..1000 {
                let key = (i % 8) as u64;
                let offset = (i * 7919) % (FRAME - READ);
                let start = Instant::now();
                let bytes = match variant {
                    0 => {
                        let frame = std::thread::scope(|scope| {
                            scope
                                .spawn(|| owned.lock().unwrap().get(&key).unwrap().clone())
                                .join()
                                .unwrap()
                        });
                        black_box(frame)[offset..offset + READ].to_vec()
                    }
                    1 => {
                        let frame = owned.lock().unwrap().get(&key).unwrap().clone();
                        black_box(frame)[offset..offset + READ].to_vec()
                    }
                    2 => {
                        let frame = std::thread::scope(|scope| {
                            scope
                                .spawn(|| shared.lock().unwrap().get(&key).unwrap().clone())
                                .join()
                                .unwrap()
                        });
                        black_box(frame)[offset..offset + READ].to_vec()
                    }
                    3 => {
                        let frame = shared.lock().unwrap().get(&key).unwrap().clone();
                        black_box(frame)[offset..offset + READ].to_vec()
                    }
                    _ => unreachable!(),
                };
                times.push(start.elapsed().as_nanos() as u64);
                assert!(
                    bytes
                        .iter()
                        .enumerate()
                        .all(|(j, b)| *b == ((offset + j + key as usize) % 251) as u8)
                );
                checksum += black_box(bytes.iter().map(|b| u64::from(*b)).sum::<u64>());
            }
            times.sort_unstable();
            samples.push(format!("{{\"round\":{round},\"variant\":\"{}\",\"p50_ns\":{},\"p99_ns\":{},\"checksum\":{checksum}}}",
                ["thread_vec_clone", "inline_vec_clone", "thread_arc_slice", "inline_arc_slice"][variant], times[500], times[990]));
        }
    }
    println!(
        "{{\"kind\":\"source_inspired_mechanism_only\",\"frame_bytes\":{FRAME},\"read_bytes\":{READ},\"reads_per_round\":1000,\"samples\":[{}]}}",
        samples.join(",")
    );
}
