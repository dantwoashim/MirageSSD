#![allow(unsafe_code)]

use mirage_ffi::{
    MirageEngineHandle, MirageStatus, mirage_engine_create_empty, mirage_engine_destroy,
    mirage_lookup,
};
#[test]
fn create_destroy_and_null_inputs_are_contained() {
    for _ in 0..10_000 {
        let mut handle: *mut MirageEngineHandle = std::ptr::null_mut();
        assert_eq!(
            unsafe { mirage_engine_create_empty(&raw mut handle) },
            MirageStatus::Ok
        );
        assert!(!handle.is_null());
        assert_eq!(unsafe { mirage_engine_destroy(handle) }, MirageStatus::Ok);
    }
    assert_eq!(
        unsafe { mirage_engine_create_empty(std::ptr::null_mut()) },
        MirageStatus::InvalidArgument
    );
    assert_eq!(
        unsafe { mirage_lookup(std::ptr::null(), std::ptr::null(), 0, std::ptr::null_mut()) },
        MirageStatus::InvalidArgument
    );
}
