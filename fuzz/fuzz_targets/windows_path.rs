#![no_main]
use libfuzzer_sys::fuzz_target;
fuzz_target!(|data: &[u8]| {
    if let Ok(value) = std::str::from_utf8(data) {
        for component in value.split(['/', '\\']) {
            let _ = mirage_manifest::validate_component(component);
        }
    }
});
