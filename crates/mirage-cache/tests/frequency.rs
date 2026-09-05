use std::sync::Arc;

use mirage_cache::FrequencySketch;
use mirage_types::PageHash;

#[test]
fn counts_age_saturate_and_are_seed_deterministic() {
    let hash = PageHash::from_bytes([7; 32]);
    let sketch = FrequencySketch::new(64, 42, 300).expect("sketch");
    assert_eq!(sketch.estimate(hash), 0);
    for _ in 0..300 {
        sketch.record(hash);
    }
    assert!(sketch.estimate(hash) >= 255);
    assert!(sketch.age_if_needed());
    assert!(sketch.estimate(hash) <= 128);
    assert_eq!(sketch.memory_bytes(), 4 * 64 + 8);
    let same = FrequencySketch::new(64, 42, 300).expect("same seed");
    for _ in 0..3 {
        same.record(hash);
    }
    assert_eq!(same.estimate(hash), 3);
}

#[test]
fn concurrent_recording_is_bounded_and_nonblocking() {
    let sketch = Arc::new(FrequencySketch::new(1024, 9, u64::MAX).expect("sketch"));
    let hash = PageHash::from_bytes([11; 32]);
    let threads = (0..16)
        .map(|_| {
            let sketch = Arc::clone(&sketch);
            std::thread::spawn(move || {
                for _ in 0..10_000 {
                    sketch.record(hash);
                }
            })
        })
        .collect::<Vec<_>>();
    for thread in threads {
        thread.join().expect("thread");
    }
    assert_eq!(sketch.estimate(hash), 256);
    assert_eq!(sketch.memory_bytes(), 4 * 1024 + 128);
}
