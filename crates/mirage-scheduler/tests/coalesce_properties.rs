use mirage_backend::{BackendId, ObjectKind, ProviderObjectId, RemoteObjectRef};
use mirage_scheduler::{FetchPriority, WindowFrame, coalesce};
use mirage_types::{ByteCount, CheckedRange, ContentHash, PageHash};
use proptest::prelude::*;

fn object(length: u64) -> RemoteObjectRef {
    RemoteObjectRef {
        backend_id: BackendId::new("local").expect("backend"),
        provider_object_id: ProviderObjectId::new("pack").expect("object"),
        immutable_revision: None,
        byte_length: ByteCount::from_u64(length),
        content_hash: ContentHash::from_bytes([4; 32]),
        kind: ObjectKind::Pack,
    }
}

proptest! { #[test] fn windows_are_bounded_and_mappings_stay_inside(mut lengths in prop::collection::vec(1_u64..200,1..40)) {
    let total=lengths.iter().sum::<u64>()+lengths.len() as u64*3; let mut offset=0; let mut frames=Vec::new();
    for (i,length) in lengths.drain(..).enumerate(){ frames.push(WindowFrame{page_hash:PageHash::from_bytes([i as u8;32]),object:object(total),range:CheckedRange::new(offset,length).expect("range"),priority:FetchPriority::P4ReadAhead}); offset+=length+3; }
    let windows=coalesce(frames,512,4).expect("coalesce");
    for window in windows { prop_assert!(window.range.len()<=512); prop_assert!(window.range.end_exclusive()<=total); for mapping in window.frames { prop_assert!(mapping.window_offset+mapping.encoded_length<=window.range.len()); } }
} }

#[test]
fn urgent_and_speculative_do_not_mix_and_gaps_are_explainable() {
    let obj = object(100);
    let windows = coalesce(
        vec![
            WindowFrame {
                page_hash: PageHash::from_bytes([1; 32]),
                object: obj.clone(),
                range: CheckedRange::new(0, 10).expect("range"),
                priority: FetchPriority::P0Blocking,
            },
            WindowFrame {
                page_hash: PageHash::from_bytes([2; 32]),
                object: obj.clone(),
                range: CheckedRange::new(12, 10).expect("range"),
                priority: FetchPriority::P1Mandatory,
            },
            WindowFrame {
                page_hash: PageHash::from_bytes([3; 32]),
                object: obj,
                range: CheckedRange::new(24, 10).expect("range"),
                priority: FetchPriority::P4ReadAhead,
            },
        ],
        64,
        4,
    )
    .expect("windows");
    assert_eq!(windows.len(), 2);
    assert_eq!(windows[0].gap_bytes, 2);
}
