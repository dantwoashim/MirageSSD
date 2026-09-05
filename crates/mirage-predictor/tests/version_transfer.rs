use mirage_predictor::version_transfer::*;
use mirage_types::PageHash;
fn page(file: u8, offset: u64, hash: u8) -> VersionPage {
    VersionPage {
        logical: LogicalPage {
            file_id: [file; 16],
            offset,
            length: 1024,
        },
        hash: PageHash::from_bytes([hash; 32]),
    }
}
#[test]
fn identity_remap_and_removal_are_never_confused() {
    let old = [page(1, 0, 1), page(2, 0, 2), page(3, 0, 3)];
    let new = [page(9, 9, 1), page(2, 0, 8)];
    let report = transfer(&old, &new, true, 400_000).unwrap();
    assert_eq!(report.retained_bytes, 1024);
    assert_eq!(report.remapped_bytes, 1024);
    assert_eq!(report.invalidated_bytes, 1024);
    assert_eq!(report.pages[0].kind, TransferKind::ExactHash);
    assert_eq!(report.pages[1].kind, TransferKind::LogicalRemap);
    assert!(
        report
            .pages
            .iter()
            .all(|p| p.kind == TransferKind::ExactHash || p.old.logical == p.new.logical)
    );
}
#[test]
fn logical_reuse_can_be_disabled() {
    let report = transfer(&[page(1, 0, 1)], &[page(1, 0, 2)], false, 0).unwrap();
    assert!(report.pages.is_empty());
    assert_eq!(report.invalidated_bytes, 1024);
}
