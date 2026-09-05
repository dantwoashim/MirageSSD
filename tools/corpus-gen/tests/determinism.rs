use mirage_corpus_gen::{CorpusProfile, PatternKind, describe, fill_at, oracle_byte, plan};

#[test]
fn every_profile_is_byte_for_byte_deterministic_for_one_seed() {
    for profile in [
        CorpusProfile::LargeContainer,
        CorpusProfile::ManyFiles,
        CorpusProfile::DedupeVersions,
        CorpusProfile::RandomRead,
        CorpusProfile::Mmap,
    ] {
        let first = describe(&plan(profile, 42));
        let second = describe(&plan(profile, 42));
        assert_eq!(first, second);
        assert_eq!(
            serde_json::to_vec(&first).unwrap(),
            serde_json::to_vec(&second).unwrap()
        );
        assert_ne!(
            first.oracle_spec_hash,
            describe(&plan(profile, 43)).oracle_spec_hash
        );
    }
}

#[test]
fn declared_scale_matches_the_canonical_profiles_without_allocating_payloads() {
    let large = plan(CorpusProfile::LargeContainer, 1);
    assert_eq!(large.files.len(), 1);
    assert_eq!(large.total_logical_bytes, 100 * 1024_u64.pow(3));
    let many = plan(CorpusProfile::ManyFiles, 1);
    assert_eq!(many.files.len(), 100_000);
    assert!(
        many.files
            .windows(2)
            .all(|pair| pair[0].logical_length >= pair[1].logical_length)
    );
}

#[test]
fn dedupe_versions_change_exactly_one_page_in_each_group_of_four() {
    let seed = 9;
    let page_size = 1024 * 1024_u64;
    for page in 0_u64..16 {
        let offset = page * page_size + 12345;
        let base = oracle_byte(seed, PatternKind::Pseudorandom, offset);
        let next = oracle_byte(seed, PatternKind::ChangedQuarter, offset);
        assert_eq!(base == next, page % 4 != 0);
    }
}

#[test]
fn oracle_slices_are_composable_at_arbitrary_offsets() {
    let mut whole = vec![0_u8; 8193];
    fill_at(77, PatternKind::PageRecognizable, 1_048_573, &mut whole);
    let mut first = vec![0_u8; 4000];
    let mut second = vec![0_u8; 4193];
    fill_at(77, PatternKind::PageRecognizable, 1_048_573, &mut first);
    fill_at(77, PatternKind::PageRecognizable, 1_052_573, &mut second);
    first.extend(second);
    assert_eq!(whole, first);
}
