use std::cmp::Ordering;

use mirage_manifest::MAX_COMPONENT_BYTES;
use mirage_types::MirageError;
use smallvec::SmallVec;

#[must_use]
pub fn ordinal_key(value: &str) -> String {
    value.chars().flat_map(char::to_uppercase).collect()
}

#[must_use]
pub fn compare_names(left: &str, right: &str) -> Ordering {
    compare_ordinal_ignore_case(left, right).then_with(|| left.as_bytes().cmp(right.as_bytes()))
}

#[cfg(windows)]
#[must_use]
#[allow(unsafe_code)]
pub fn compare_ordinal_ignore_case(left: &str, right: &str) -> Ordering {
    use windows_sys::Win32::Globalization::{
        CSTR_EQUAL, CSTR_GREATER_THAN, CSTR_LESS_THAN, CompareStringOrdinal,
    };

    if left.is_ascii() && right.is_ascii() {
        return compare_ascii_ignore_case(left, right);
    }
    let left_wide = left.encode_utf16().collect::<SmallVec<[u16; 256]>>();
    let right_wide = right.encode_utf16().collect::<SmallVec<[u16; 256]>>();
    let Ok(left_length) = i32::try_from(left_wide.len()) else {
        return fallback_compare(left, right);
    };
    let Ok(right_length) = i32::try_from(right_wide.len()) else {
        return fallback_compare(left, right);
    };
    // SAFETY: both pointers reference initialized UTF-16 buffers for the exact lengths
    // supplied. CompareStringOrdinal reads neither terminator nor bytes outside them.
    let result = unsafe {
        CompareStringOrdinal(
            left_wide.as_ptr(),
            left_length,
            right_wide.as_ptr(),
            right_length,
            1,
        )
    };
    match result {
        CSTR_LESS_THAN => Ordering::Less,
        CSTR_EQUAL => Ordering::Equal,
        CSTR_GREATER_THAN => Ordering::Greater,
        _ => fallback_compare(left, right),
    }
}

#[cfg(not(windows))]
#[must_use]
pub fn compare_ordinal_ignore_case(left: &str, right: &str) -> Ordering {
    if left.is_ascii() && right.is_ascii() {
        compare_ascii_ignore_case(left, right)
    } else {
        fallback_compare(left, right)
    }
}

fn compare_ascii_ignore_case(left: &str, right: &str) -> Ordering {
    left.bytes()
        .map(|byte| byte.to_ascii_uppercase())
        .cmp(right.bytes().map(|byte| byte.to_ascii_uppercase()))
}

fn fallback_compare(left: &str, right: &str) -> Ordering {
    ordinal_key(left)
        .as_bytes()
        .cmp(ordinal_key(right).as_bytes())
}

pub fn validate_component(component: &str) -> Result<(), MirageError> {
    if component.is_empty()
        || component == "."
        || component == ".."
        || component.len() > MAX_COMPONENT_BYTES
        || component
            .chars()
            .any(|character| matches!(character, '/' | '\\' | '\0'))
    {
        return Err(MirageError::invalid_argument(
            "lookup path contains an invalid component",
        ));
    }
    Ok(())
}
