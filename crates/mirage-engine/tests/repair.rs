use mirage_backend::{BackendId, ObjectKind, ProviderObjectId, RemoteObjectRef};
use mirage_db::{Database, NewRepository, VerifiedGeneration};
use mirage_engine::repair::{LocalMetadataRepairSource, RepairLevel, repair};
use mirage_manifest::{
    Codec, FileClass, ManifestBuilder, ManifestFile, ManifestPage, encode_manifest, manifest_hash,
};
use mirage_types::{
    ByteCount, CommitHash, ContentHash, GenerationId, PageHash, RepositoryId, RepositoryState,
};

#[test]
fn local_metadata_repair_verifies_durable_identity_and_rejects_tampering() {
    let directory = tempfile::tempdir().expect("directory");
    let repository_id = RepositoryId::from_bytes([0x52; 16]);
    let generation_id = GenerationId::from_u64(7);
    let manifest = ManifestBuilder::new(repository_id, generation_id, ByteCount::from_u64(65536))
        .build(vec![ManifestFile {
            relative_path: "assets/data.bin".into(),
            logical_size: ByteCount::from_u64(65536),
            class: FileClass::VirtualAsset,
            pages: vec![ManifestPage {
                hash: PageHash::from_bytes([3; 32]),
                logical_length: 65536,
                object: RemoteObjectRef {
                    backend_id: BackendId::new("local").expect("backend"),
                    provider_object_id: ProviderObjectId::new("pack-1").expect("object"),
                    immutable_revision: None,
                    byte_length: ByteCount::from_u64(131072),
                    content_hash: ContentHash::from_bytes([4; 32]),
                    kind: ObjectKind::Pack,
                },
                offset: 0,
                encoded_length: ByteCount::from_u64(65536),
                codec: Codec::None,
            }],
        }])
        .expect("manifest");
    let manifest_path = directory.path().join("active.mmanifest");
    std::fs::write(&manifest_path, encode_manifest(&manifest).expect("encode")).expect("write");
    let database = Database::open(&directory.path().join("control.db")).expect("database");
    database
        .create_repository(NewRepository {
            repository_id,
            display_name: "repair fixture".into(),
            local_root: directory.path().join("root"),
            owner_sid: "S-1-5-18".into(),
            content_encrypted: false,
            initial_state: RepositoryState::ReadyUnmounted,
            created_at_ns: 1,
        })
        .expect("repository");
    let commit = CommitHash::from_bytes([5; 32]);
    database
        .insert_verified_generation(VerifiedGeneration {
            repository_id,
            generation_id,
            commit_hash: commit,
            manifest_hash: manifest_hash(&manifest).expect("hash"),
            manifest_local_path: manifest_path.clone(),
            mount_index_path: None,
            created_at_ns: 2,
        })
        .expect("generation");
    database
        .activate_generation(repository_id, generation_id, commit, None, 3)
        .expect("activate");
    let source = LocalMetadataRepairSource::new(database, repository_id);
    let report = repair(&source, RepairLevel::Metadata).expect("repair");
    assert_eq!(report.verified_objects, 1);
    assert!(report.missing_objects.is_empty());

    std::fs::write(&manifest_path, b"tampered").expect("tamper");
    assert!(repair(&source, RepairLevel::Metadata).is_err());
}
