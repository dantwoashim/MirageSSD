use mirage_backend::{BackendId, ObjectKind, ProviderObjectId, RemoteObjectRef};
use mirage_engine::update::{ManifestOverlay, StagedPageMapping, build_updated_manifest};
use mirage_manifest::{Codec, FileClass, ManifestBuilder, ManifestFile, ManifestPage};
use mirage_types::{ByteCount, ContentHash, GenerationId, PageHash, RepositoryId};
fn object(id: &str, hash: u8) -> RemoteObjectRef {
    RemoteObjectRef {
        backend_id: BackendId::new("local").unwrap(),
        provider_object_id: ProviderObjectId::new(id).unwrap(),
        immutable_revision: None,
        byte_length: ByteCount::from_u64(1_000_000),
        content_hash: ContentHash::from_bytes([hash; 32]),
        kind: ObjectKind::Pack,
    }
}
#[test]
fn n_plus_one_reuses_unchanged_replaces_dirty_deletes_and_creates() {
    let original = object("base", 1);
    let base = ManifestBuilder::new(
        RepositoryId::from_bytes([1; 16]),
        GenerationId(1),
        ByteCount::from_u64(65536),
    )
    .build(vec![
        ManifestFile {
            relative_path: "assets/data.bin".into(),
            logical_size: ByteCount::from_u64(131072),
            class: FileClass::VirtualAsset,
            pages: vec![
                ManifestPage {
                    hash: PageHash::from_bytes([1; 32]),
                    logical_length: 65536,
                    object: original.clone(),
                    offset: 10,
                    encoded_length: ByteCount::from_u64(100),
                    codec: Codec::None,
                },
                ManifestPage {
                    hash: PageHash::from_bytes([2; 32]),
                    logical_length: 65536,
                    object: original.clone(),
                    offset: 110,
                    encoded_length: ByteCount::from_u64(100),
                    codec: Codec::None,
                },
            ],
        },
        ManifestFile {
            relative_path: "old.cfg".into(),
            logical_size: ByteCount::from_u64(5),
            class: FileClass::NativeConfiguration,
            pages: vec![],
        },
    ])
    .unwrap();
    let file = base
        .files
        .iter()
        .find(|f| f.name == "data.bin")
        .unwrap()
        .stable_id;
    let old = base
        .files
        .iter()
        .find(|f| f.name == "old.cfg")
        .unwrap()
        .stable_id;
    let staged = object("staged", 9);
    let updated = build_updated_manifest(
        &base,
        GenerationId(2),
        ManifestOverlay {
            page_mappings: vec![StagedPageMapping {
                file_id: file,
                page_index: 1,
                page_hash: PageHash::from_bytes([9; 32]),
                object: staged.clone(),
                frame_offset: 44,
                frame_length: 120,
                logical_length: 65536,
                codec: Codec::None,
            }],
            deleted_files: [old].into(),
            created_files: vec![ManifestFile {
                relative_path: "new.cfg".into(),
                logical_size: ByteCount::from_u64(3),
                class: FileClass::NativeConfiguration,
                pages: vec![],
            }],
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(updated.generation_id, GenerationId(2));
    assert_eq!(updated.files.len(), 2);
    assert_eq!(
        updated.pages[0].plaintext_hash,
        PageHash::from_bytes([1; 32])
    );
    assert_eq!(
        updated.pages[1].plaintext_hash,
        PageHash::from_bytes([9; 32])
    );
    assert_eq!(updated.remote_locations[1].object, staged);
}
