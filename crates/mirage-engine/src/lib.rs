//! Platform-neutral file-open and exact-read contracts for MirageSSD.

#![forbid(unsafe_code)]

pub mod admission;
pub mod api;
pub mod capsule_materialize;
pub mod copy;
pub mod file_handle;
pub mod gc;
pub mod generation;
pub mod get_or_fetch;
pub mod mark_set;
pub mod mount;
pub mod object_plan;
pub mod observe;
pub mod outcome;
pub mod page_location;
pub mod page_provider;
pub mod read;
pub mod read_request;
pub mod repair;
pub mod repository_extract;
pub mod repository_reader;
pub mod repository_writer;
pub mod restore;
pub mod seal;
pub mod space_lease;
pub mod update;

pub use admission::{AdmissionStore, AdmittedSession, admit_sealed_session};
pub use api::{EngineResult, ReadEngine};
pub use capsule_materialize::{CapsulePageStore, MaterializeProgress, materialize_capsule};
pub use file_handle::{AccessMask, FileHandleContext};
pub use generation::MountGeneration;
pub use observe::{ObservationEvent, SessionObserver, TelemetryMetrics};
pub use outcome::{CacheTier, ReadOutcome, SealViolation};
pub use page_location::{PageLocation, PageLocationMap};
pub use page_provider::{FetchContext, PageProvider};
pub use read_request::{
    AccessPattern, BufferCacheMode, BufferingHint, ProcessRole, ReadContext, ReadPriority,
};
pub use repository_extract::{
    ExtractReport, extract_virtual_files, extract_virtual_files_with_encryption,
};
pub use repository_reader::{
    RecoveredRepository, RecoveryHints, recover_repository, recover_repository_with_hints,
};
pub use repository_writer::{
    PublishedGeneration, publish_base_generation, publish_successor_generation,
};
pub use restore::{FreshRestore, restore_fresh};
