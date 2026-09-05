mod support;

use mirage_manifest::{
    ChainValidationError, commit_hash, decode_commit_bounded, encode_commit,
    select_highest_valid_chain, sign_commit, validate_commit, validate_link,
};
use mirage_types::{CommitHash, ContentHash, ManifestHash, RepositoryId};

use support::{commit_body, signer};

#[test]
fn ten_commit_fixture_chain_is_canonical_signed_and_linked() {
    let signer = signer();
    let fixture_names = [
        include_bytes!("fixtures/commit-chain/commit-0000.cbor").as_slice(),
        include_bytes!("fixtures/commit-chain/commit-0001.cbor").as_slice(),
        include_bytes!("fixtures/commit-chain/commit-0002.cbor").as_slice(),
        include_bytes!("fixtures/commit-chain/commit-0003.cbor").as_slice(),
        include_bytes!("fixtures/commit-chain/commit-0004.cbor").as_slice(),
        include_bytes!("fixtures/commit-chain/commit-0005.cbor").as_slice(),
        include_bytes!("fixtures/commit-chain/commit-0006.cbor").as_slice(),
        include_bytes!("fixtures/commit-chain/commit-0007.cbor").as_slice(),
        include_bytes!("fixtures/commit-chain/commit-0008.cbor").as_slice(),
        include_bytes!("fixtures/commit-chain/commit-0009.cbor").as_slice(),
    ];
    let commits = fixture_names
        .iter()
        .map(|bytes| decode_commit_bounded(bytes).expect("decode commit fixture"))
        .collect::<Vec<_>>();
    for (index, commit) in commits.iter().enumerate() {
        validate_commit(commit, &signer).expect("valid fixture signature");
        assert_eq!(
            encode_commit(commit).expect("canonical commit"),
            fixture_names[index]
        );
        if index > 0 {
            validate_link(&commits[index - 1], commit, &signer).expect("valid chain link");
        }
    }
    let selected =
        select_highest_valid_chain(&commits[0], &commits[1..], &signer).expect("one valid chain");
    assert_eq!(selected, commits);
}

#[test]
fn signature_parent_sequence_and_manifest_tampering_are_rejected() {
    let signer = signer();
    let repository_id = RepositoryId::from_bytes([0x61; 16]);
    let root = sign_commit(commit_body(repository_id, 0, None, 1), &signer).expect("root");

    let mut bad_signature = root.clone();
    bad_signature.signature.signature[0] ^= 1;
    assert!(validate_commit(&bad_signature, &signer).is_err());

    let mut altered_manifest = root.clone();
    altered_manifest.body.manifest_hash = ManifestHash::from_bytes([0xE1; 32]);
    altered_manifest.body.manifest_object.content_hash = ContentHash::from_bytes([0xE1; 32]);
    assert!(validate_commit(&altered_manifest, &signer).is_err());

    let wrong_parent = sign_commit(
        commit_body(repository_id, 1, Some(CommitHash::from_bytes([9; 32])), 2),
        &signer,
    )
    .expect("structurally valid child");
    assert!(validate_link(&root, &wrong_parent, &signer).is_err());

    let wrong_sequence = sign_commit(
        commit_body(
            repository_id,
            2,
            Some(commit_hash(&root).expect("root hash")),
            3,
        ),
        &signer,
    )
    .expect("structurally valid child");
    assert!(validate_link(&root, &wrong_sequence, &signer).is_err());
}

#[test]
fn valid_fork_returns_explicit_repository_conflict_metadata() {
    let signer = signer();
    let repository_id = RepositoryId::from_bytes([0x41; 16]);
    let root = sign_commit(commit_body(repository_id, 0, None, 1), &signer).expect("root");
    let parent_hash = commit_hash(&root).expect("root hash");
    let first = sign_commit(commit_body(repository_id, 1, Some(parent_hash), 2), &signer)
        .expect("first child");
    let second = sign_commit(commit_body(repository_id, 1, Some(parent_hash), 3), &signer)
        .expect("second child");

    let error = select_highest_valid_chain(&root, &[first, second], &signer)
        .expect_err("fork must not be silently resolved");
    match error {
        ChainValidationError::Conflict { metadata, .. } => {
            assert_eq!(metadata.parent, parent_hash);
            assert_eq!(metadata.children.len(), 2);
        }
        ChainValidationError::Invalid(error) => panic!("expected conflict, got {error}"),
    }
}

#[test]
fn disconnected_high_sequence_commit_cannot_override_trusted_chain() {
    let signer = signer();
    let repository_id = RepositoryId::from_bytes([0x51; 16]);
    let root = sign_commit(commit_body(repository_id, 0, None, 1), &signer).expect("root");
    let child = sign_commit(
        commit_body(
            repository_id,
            1,
            Some(commit_hash(&root).expect("root hash")),
            2,
        ),
        &signer,
    )
    .expect("child");
    let disconnected = sign_commit(
        commit_body(
            repository_id,
            50_000,
            Some(CommitHash::from_bytes([0xFF; 32])),
            3,
        ),
        &signer,
    )
    .expect("disconnected signed commit");
    let selected = select_highest_valid_chain(&root, &[disconnected, child.clone()], &signer)
        .expect("trusted chain");
    assert_eq!(selected, vec![root, child]);
}

#[test]
fn authoritative_chain_code_never_consults_a_latest_hint() {
    assert!(!include_str!("../src/chain.rs").contains("LATEST"));
}
