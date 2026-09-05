use mirage_backend::{BackendId, ObjectKind, ProviderObjectId, RemoteObjectRef};
use mirage_engine::{PageLocation, PageLocationMap};
use mirage_types::{ByteCount, CheckedRange, ContentHash, PageHash};

fn location(id: &str, offset: u64) -> PageLocation {
    PageLocation {
        object: RemoteObjectRef {
            backend_id: BackendId::new("local").expect("backend"),
            provider_object_id: ProviderObjectId::new(id).expect("id"),
            immutable_revision: None,
            byte_length: ByteCount::from_u64(100),
            content_hash: ContentHash::from_bytes([2; 32]),
            kind: ObjectKind::Pack,
        },
        encoded_range: CheckedRange::new(offset, 10).expect("range"),
        logical_length: 10,
    }
}
#[test]
fn duplicate_locations_are_idempotent_but_conflicts_and_missing_fail() {
    let hash = PageHash::from_bytes([1; 32]);
    let mut map = PageLocationMap::default();
    map.insert(hash, location("a", 0)).expect("insert");
    map.insert(hash, location("a", 0)).expect("same");
    assert!(map.insert(hash, location("b", 0)).is_err());
    assert!(map.get(PageHash::from_bytes([9; 32])).is_err());
    assert_eq!(map.len(), 1);
}
