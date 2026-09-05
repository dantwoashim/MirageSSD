#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RangeCase {
    pub file_length: u64,
    pub offset: u64,
    pub requested_length: usize,
}

#[must_use]
pub fn deterministic_range_cases(page_size: u64) -> Vec<RangeCase> {
    let tail = page_size / 3;
    let file_length = page_size.saturating_mul(3).saturating_add(tail);
    vec![
        RangeCase {
            file_length: 0,
            offset: 0,
            requested_length: 0,
        },
        RangeCase {
            file_length,
            offset: 0,
            requested_length: 0,
        },
        RangeCase {
            file_length,
            offset: 0,
            requested_length: 1,
        },
        RangeCase {
            file_length,
            offset: page_size.saturating_sub(1),
            requested_length: 2,
        },
        RangeCase {
            file_length,
            offset: file_length.saturating_sub(1),
            requested_length: 4096,
        },
        RangeCase {
            file_length,
            offset: file_length,
            requested_length: 1,
        },
        RangeCase {
            file_length,
            offset: u64::MAX,
            requested_length: 2,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corpus_always_contains_eof_tail_and_overflow_boundaries() {
        let cases = deterministic_range_cases(1024 * 1024);
        assert!(cases.iter().any(|case| case.offset == case.file_length));
        assert!(cases.iter().any(|case| case.offset == u64::MAX));
        assert!(cases.iter().any(|case| case.requested_length == 0));
    }
}
