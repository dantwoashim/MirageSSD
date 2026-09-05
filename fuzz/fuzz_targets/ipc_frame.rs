#![no_main]
use libfuzzer_sys::fuzz_target;
fuzz_target!(|data: &[u8]| {
    let _ = mirage_ipc::decode_frame::<mirage_ipc::Request>(data);
});
