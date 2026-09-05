#![no_main]
use libfuzzer_sys::fuzz_target;
use mirage_manifest::{DecodeLimits, decode_manifest_bounded};
fuzz_target!(|data: &[u8]| {
    let limits = DecodeLimits {
        max_input_bytes: 1 << 20,
        max_directories: 1024,
        max_files: 1024,
        max_extents: 4096,
        max_pages: 4096,
        max_remote_locations: 1024,
    };
    let _ = decode_manifest_bounded(data, limits);
});
