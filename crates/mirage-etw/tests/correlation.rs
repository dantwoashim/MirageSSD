use mirage_etw::correlate::Correlator;
use std::path::PathBuf;

#[test]
fn reuse_unknown_and_capacity_are_explicit() {
    let mut correlator = Correlator::new(2);
    correlator.name(1, PathBuf::from("a.bin"));
    correlator.name(2, PathBuf::from("b.bin"));
    assert_eq!(
        correlator.io(1, 7, 1, 10, 4, false).path.unwrap(),
        PathBuf::from("a.bin")
    );
    correlator.name(3, PathBuf::from("c.bin"));
    assert!(correlator.io(2, 7, 1, 0, 1, false).path.is_none());
    correlator.name(2, PathBuf::from("renamed.bin"));
    assert_eq!(
        correlator.io(3, 8, 2, 0, 2, true).path.unwrap(),
        PathBuf::from("renamed.bin")
    );
    correlator.remove(2);
    assert!(correlator.io(4, 8, 2, 0, 2, false).path.is_none());
}
