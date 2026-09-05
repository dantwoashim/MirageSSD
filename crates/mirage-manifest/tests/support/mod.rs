#![allow(dead_code)]

use mirage_backend::{BackendId, ImmutableRevision, ObjectKind, ProviderObjectId, RemoteObjectRef};
use mirage_manifest::{
    COMMIT_FORMAT_VERSION, Codec, CommitSigner, CommitVerifier, DirectoryRecord, ExtentRecord,
    FileClass, FileRecord, MANIFEST_FORMAT_VERSION, PageRecord, RemoteLocation, RepositoryManifest,
    SignatureAlgorithm, SignatureEnvelope, UnsignedCommitBody,
};
use mirage_types::{
    ByteCount, CommitHash, ContentHash, DeviceId, GenerationId, ManifestHash, MirageError,
    PageHash, RepositoryId, StableFileId,
};

pub const PAGE_SIZE: u64 = 1024 * 1024;

pub fn pack_object(length: u64) -> RemoteObjectRef {
    RemoteObjectRef {
        backend_id: BackendId::new("memory").expect("valid backend ID"),
        provider_object_id: ProviderObjectId::new("pack-0001").expect("valid provider ID"),
        immutable_revision: Some(ImmutableRevision::new("revision-0001").expect("valid revision")),
        byte_length: ByteCount::from_u64(length),
        content_hash: ContentHash::from_bytes([0xA5; 32]),
        kind: ObjectKind::Pack,
    }
}

pub fn minimal_manifest() -> RepositoryManifest {
    RepositoryManifest {
        format_version: MANIFEST_FORMAT_VERSION,
        repository_id: RepositoryId::from_bytes([0x11; 16]),
        generation_id: GenerationId::from_u64(1),
        page_size: ByteCount::from_u64(PAGE_SIZE),
        directories: vec![DirectoryRecord {
            parent: None,
            name: String::new(),
        }],
        files: vec![FileRecord {
            parent_directory: 0,
            name: "asset.pak".to_string(),
            logical_size: ByteCount::from_u64(6),
            stable_id: StableFileId::from_u64(1),
            class: FileClass::VirtualContainer,
            extent_start: 0,
            extent_count: 1,
        }],
        extents: vec![ExtentRecord {
            logical_offset: 0,
            logical_length: ByteCount::from_u64(6),
            page_start: 0,
            page_count: 1,
        }],
        pages: vec![PageRecord {
            plaintext_hash: PageHash::from_bytes([0x22; 32]),
            logical_length: 6,
            remote_location: 0,
        }],
        remote_locations: vec![RemoteLocation {
            object: pack_object(4096),
            offset: 17,
            encoded_length: ByteCount::from_u64(6),
            codec: Codec::None,
        }],
    }
}

pub fn complex_manifest() -> RepositoryManifest {
    let tail = 333_u64;
    RepositoryManifest {
        format_version: MANIFEST_FORMAT_VERSION,
        repository_id: RepositoryId::from_bytes([0x33; 16]),
        generation_id: GenerationId::from_u64(9),
        page_size: ByteCount::from_u64(PAGE_SIZE),
        directories: vec![
            DirectoryRecord {
                parent: None,
                name: String::new(),
            },
            DirectoryRecord {
                parent: Some(0),
                name: "Assets".to_string(),
            },
        ],
        files: vec![
            FileRecord {
                parent_directory: 0,
                name: "settings.toml".to_string(),
                logical_size: ByteCount::from_u64(42),
                stable_id: StableFileId::from_u64(7),
                class: FileClass::NativeConfiguration,
                extent_start: 0,
                extent_count: 0,
            },
            FileRecord {
                parent_directory: 1,
                name: "世界.pak".to_string(),
                logical_size: ByteCount::from_u64(PAGE_SIZE + tail),
                stable_id: StableFileId::from_u64(8),
                class: FileClass::VirtualContainer,
                extent_start: 0,
                extent_count: 2,
            },
        ],
        extents: vec![
            ExtentRecord {
                logical_offset: 0,
                logical_length: ByteCount::from_u64(PAGE_SIZE),
                page_start: 0,
                page_count: 1,
            },
            ExtentRecord {
                logical_offset: PAGE_SIZE,
                logical_length: ByteCount::from_u64(tail),
                page_start: 1,
                page_count: 1,
            },
        ],
        pages: vec![
            PageRecord {
                plaintext_hash: PageHash::from_bytes([0x44; 32]),
                logical_length: u32::try_from(PAGE_SIZE).expect("page size fits u32"),
                remote_location: 0,
            },
            PageRecord {
                plaintext_hash: PageHash::from_bytes([0x55; 32]),
                logical_length: u32::try_from(tail).expect("tail fits u32"),
                remote_location: 1,
            },
        ],
        remote_locations: vec![
            RemoteLocation {
                object: pack_object(PAGE_SIZE + tail),
                offset: 0,
                encoded_length: ByteCount::from_u64(PAGE_SIZE),
                codec: Codec::None,
            },
            RemoteLocation {
                object: pack_object(PAGE_SIZE + tail),
                offset: PAGE_SIZE,
                encoded_length: ByteCount::from_u64(tail),
                codec: Codec::None,
            },
        ],
    }
}

pub struct TestSigner;

impl CommitSigner for TestSigner {
    fn algorithm(&self) -> SignatureAlgorithm {
        SignatureAlgorithm::TestOnlyBlake3Keyed
    }

    fn key_id(&self) -> [u8; 16] {
        [0x71; 16]
    }

    fn sign(&self, canonical_unsigned_body: &[u8]) -> Result<Vec<u8>, MirageError> {
        Ok(blake3::keyed_hash(&[0x92; 32], canonical_unsigned_body)
            .as_bytes()
            .to_vec())
    }
}

impl CommitVerifier for TestSigner {
    fn verify(
        &self,
        canonical_unsigned_body: &[u8],
        signature: &SignatureEnvelope,
    ) -> Result<(), MirageError> {
        let expected = self.sign(canonical_unsigned_body)?;
        let mut difference = u8::from(signature.algorithm != self.algorithm())
            | u8::from(signature.key_id != self.key_id())
            | u8::from(signature.signature.len() != expected.len());
        for (actual, expected) in signature.signature.iter().zip(expected) {
            difference |= actual ^ expected;
        }
        if difference == 0 {
            Ok(())
        } else {
            Err(MirageError::integrity_mismatch(
                "fixture commit signature verification failed",
            ))
        }
    }
}

pub fn signer() -> TestSigner {
    TestSigner
}

pub fn commit_body(
    repository_id: RepositoryId,
    sequence: u64,
    parent_commit: Option<CommitHash>,
    salt: u8,
) -> UnsignedCommitBody {
    let manifest_hash = ManifestHash::from_bytes([salt; 32]);
    UnsignedCommitBody {
        format_version: COMMIT_FORMAT_VERSION,
        repository_id,
        sequence,
        parent_commit,
        manifest_hash,
        manifest_object: RemoteObjectRef {
            backend_id: BackendId::new("memory").expect("valid backend ID"),
            provider_object_id: ProviderObjectId::new(format!("manifest-{sequence:04}"))
                .expect("valid provider ID"),
            immutable_revision: Some(
                ImmutableRevision::new(format!("revision-{sequence:04}")).expect("valid revision"),
            ),
            byte_length: ByteCount::from_u64(512),
            content_hash: ContentHash::from_bytes(*manifest_hash.as_bytes()),
            kind: ObjectKind::Manifest,
        },
        referenced_pack_set_hash: ContentHash::from_bytes([salt.wrapping_add(1); 32]),
        created_utc_ns: i128::from(sequence) * 1_000_000_000 + i128::from(salt),
        writer_device_id: DeviceId::from_bytes([0xD1; 16]),
        update_journal_id: None,
    }
}
