use bytes::Bytes;
use mirage_backend::{ObjectBackend, ObjectKind, UploadSource};
use mirage_backend_local::LocalObjectBackend;
use mirage_engine::{
    RecoveryHints, publish_base_generation, recover_repository, recover_repository_with_hints,
};
use mirage_manifest::{
    FileClass, InMemoryTestSigner, RepositoryCommit, commit_hash, encode_commit, sign_commit,
};
use mirage_pack::{ImportPlan, PlannedFile, import_local};
use mirage_types::{CommitHash, ContentHash, GenerationId, MirageErrorKind, RepositoryId};
use tokio_util::sync::CancellationToken;

struct Fixture {
    _source: tempfile::TempDir,
    _import_parent: tempfile::TempDir,
    _backend_root: tempfile::TempDir,
    backend: LocalObjectBackend,
    signer: InMemoryTestSigner,
    base: mirage_engine::PublishedGeneration,
    repository: RepositoryId,
}

fn fixture() -> Fixture {
    let repository = RepositoryId::from_bytes([0x35; 16]);
    let source = tempfile::tempdir().expect("source");
    std::fs::write(source.path().join("data.pak"), vec![0x77; 128 * 1024]).expect("source data");
    let import_parent = tempfile::tempdir().expect("import parent");
    let imported = import_local(&ImportPlan {
        repository_id: repository,
        generation_id: GenerationId::ZERO,
        source_root: source.path().to_path_buf(),
        files: vec![PlannedFile {
            relative_path: "data.pak".into(),
            class: FileClass::VirtualContainer,
        }],
        page_size: 64 * 1024,
        pack_target: 512 * 1024,
        output_staging_directory: import_parent.path().join("import"),
        encryption: None,
    })
    .expect("import");
    let backend_root = tempfile::tempdir().expect("backend root");
    let backend = LocalObjectBackend::open(backend_root.path(), repository).expect("backend");
    let signer = InMemoryTestSigner::new([0x11; 16], [0x22; 32]);
    let base = futures_executor::block_on(publish_base_generation(
        &backend,
        &imported.packs,
        &imported.manifest,
        &signer,
    ))
    .expect("base publish");
    Fixture {
        _source: source,
        _import_parent: import_parent,
        _backend_root: backend_root,
        backend,
        signer,
        base,
        repository,
    }
}

fn child(fixture: &Fixture, parent: CommitHash, sequence: u64, created: i128) -> RepositoryCommit {
    let mut body = fixture.base.commit_body.body.clone();
    body.sequence = sequence;
    body.parent_commit = Some(parent);
    body.created_utc_ns = created;
    sign_commit(body, &fixture.signer).expect("sign child")
}

fn upload_commit(fixture: &Fixture, commit: &RepositoryCommit) -> mirage_backend::RemoteObjectRef {
    let bytes = encode_commit(commit).expect("encode commit");
    let hash = ContentHash::from_bytes(*blake3::hash(&bytes).as_bytes());
    futures_executor::block_on(fixture.backend.put_immutable(
        ObjectKind::Commit,
        UploadSource::from_bytes(Bytes::from(bytes)),
        hash,
        CancellationToken::new(),
    ))
    .expect("upload commit")
}

#[test]
fn missing_or_stale_latest_hint_does_not_block_enumeration_recovery() {
    let fixture = fixture();
    let stale = fixture.base.manifest.clone();
    let recovered = futures_executor::block_on(recover_repository_with_hints(
        &fixture.backend,
        fixture.repository,
        &fixture.signer,
        RecoveryHints {
            cached_commit: None,
            latest_hint: Some(stale),
        },
    ))
    .expect("recover despite stale hint");
    assert_eq!(recovered.head_hash, fixture.base.commit_hash);
}

#[test]
fn corrupt_newest_commit_falls_back_to_valid_older_chain() {
    let fixture = fixture();
    let newest = child(&fixture, fixture.base.commit_hash, 1, 1);
    let object = upload_commit(&fixture, &newest);
    let path = fixture
        .backend
        .root()
        .join(object.provider_object_id.as_str());
    std::fs::write(path, b"corrupt").expect("corrupt newest");
    let recovered = futures_executor::block_on(recover_repository(
        &fixture.backend,
        fixture.repository,
        &fixture.signer,
    ))
    .expect("fallback");
    assert_eq!(recovered.head_hash, fixture.base.commit_hash);
}

#[test]
fn missing_parent_candidate_is_ignored_but_fork_is_explicit_conflict() {
    let fixture = fixture();
    let orphan = child(&fixture, CommitHash::from_bytes([0x99; 32]), 2, 2);
    upload_commit(&fixture, &orphan);
    let recovered = futures_executor::block_on(recover_repository(
        &fixture.backend,
        fixture.repository,
        &fixture.signer,
    ))
    .expect("ignore orphan");
    assert_eq!(recovered.head_hash, fixture.base.commit_hash);

    upload_commit(&fixture, &child(&fixture, fixture.base.commit_hash, 1, 10));
    upload_commit(&fixture, &child(&fixture, fixture.base.commit_hash, 1, 11));
    let error = futures_executor::block_on(recover_repository(
        &fixture.backend,
        fixture.repository,
        &fixture.signer,
    ))
    .expect_err("fork conflict");
    assert_eq!(error.kind, MirageErrorKind::RepositoryConflict);
}

#[test]
fn valid_child_is_selected_as_highest_authoritative_commit() {
    let fixture = fixture();
    let child = child(&fixture, fixture.base.commit_hash, 1, 3);
    let expected = commit_hash(&child).expect("child hash");
    upload_commit(&fixture, &child);
    let recovered = futures_executor::block_on(recover_repository(
        &fixture.backend,
        fixture.repository,
        &fixture.signer,
    ))
    .expect("recover child");
    assert_eq!(recovered.head_hash, expected);
    assert_eq!(recovered.chain.len(), 2);
}
