use mirage_engine::{
    gc::{RemoteCandidate, plan_unreferenced},
    mark_set::MarkSet,
};
use mirage_types::{ContentHash, MirageErrorKind};

fn hash(byte: u8) -> ContentHash {
    ContentHash::from_bytes([byte; 32])
}
#[test]
fn two_pass_gc_preserves_every_root_and_honors_grace() {
    let objects = [
        RemoteCandidate {
            hash: hash(1),
            created_sequence: 1,
        },
        RemoteCandidate {
            hash: hash(2),
            created_sequence: 1,
        },
        RemoteCandidate {
            hash: hash(3),
            created_sequence: 99,
        },
    ];
    let mut first = MarkSet::default();
    first.mark(hash(1));
    let mut second = MarkSet::default();
    second.mark(hash(2));
    assert!(
        plan_unreferenced(&objects, &first, &second, true, 100, 10)
            .expect("plan")
            .is_empty()
    );
    assert_eq!(
        plan_unreferenced(
            &objects,
            &MarkSet::default(),
            &MarkSet::default(),
            true,
            100,
            10
        )
        .expect("plan"),
        vec![hash(1), hash(2)]
    );
    let error =
        plan_unreferenced(&objects, &first, &second, false, 100, 10).expect_err("changed roots");
    assert_eq!(error.kind, MirageErrorKind::RepositoryConflict);
}
