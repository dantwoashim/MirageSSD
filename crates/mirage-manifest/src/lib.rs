//! Validated logical manifests, canonical persistence, commit chains, and source inventory.

#![forbid(unsafe_code)]

pub mod builder;
pub mod canonical;
pub mod chain;
pub mod classify;
pub mod codec;
pub mod commit;
pub mod hash;
pub mod inventory;
pub mod model;
pub mod path;
pub mod signature;
pub mod validate;

pub use builder::{ManifestBuilder, ManifestFile, ManifestPage};
pub use chain::{ChainConflict, ChainValidationError, select_highest_valid_chain, validate_link};
pub use classify::{ClassificationRuleSet, ClassificationVerdict};
pub use codec::{DecodeLimits, decode_manifest_bounded, encode_manifest};
pub use commit::{
    COMMIT_FORMAT_VERSION, RepositoryCommit, UnsignedCommitBody, commit_hash,
    decode_commit_bounded, encode_commit, sign_commit, validate_commit,
};
pub use hash::manifest_hash;
pub use inventory::{
    Inventory, InventoryEntry, InventoryEntryKind, InventoryScanner, ReparsePolicy,
};
pub use model::{
    Codec, DirectoryRecord, ExtentRecord, FileClass, FileRecord, MANIFEST_FORMAT_VERSION,
    ManifestSummary, PageRecord, RemoteLocation, RepositoryManifest,
};
pub use path::{MAX_COMPONENT_BYTES, MAX_LOGICAL_PATH_BYTES, validate_component, windows_case_key};
#[cfg(feature = "test-signing")]
pub use signature::InMemoryTestSigner;
pub use signature::{CommitSigner, CommitVerifier, SignatureAlgorithm, SignatureEnvelope};
pub use validate::{
    MAX_DIRECTORIES, MAX_EXTENTS, MAX_FILE_BYTES, MAX_FILES, MAX_PAGES, MAX_REMOTE_LOCATIONS,
    validate_manifest,
};
