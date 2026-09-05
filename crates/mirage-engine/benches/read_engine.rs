use std::hint::black_box;
use std::time::Instant;

fn main() {
    let source = (0_u8..=255)
        .cycle()
        .take(8 * 1024 * 1024)
        .collect::<Vec<_>>();
    for (size, iterations) in [
        (64 * 1024, 10_000),
        (1024 * 1024, 1_000),
        (8 * 1024 * 1024, 100),
    ] {
        let mut destination = vec![0_u8; size];
        let start = Instant::now();
        for iteration in 0..iterations {
            let offset = (iteration * 4096) % (source.len() - size + 1);
            destination.copy_from_slice(&source[offset..offset + size]);
            black_box(&destination);
        }
        let elapsed = start.elapsed();
        println!(
            "copy_bytes={} iterations={} elapsed_ns={} throughput_bytes_per_second={}",
            size,
            iterations,
            elapsed.as_nanos(),
            (size as u128 * iterations as u128 * 1_000_000_000) / elapsed.as_nanos().max(1)
        );
    }
}
