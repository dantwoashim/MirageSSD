//! Platform-neutral file-open and exact-read contracts for MirageSSD.

#![forbid(unsafe_code)]

pub mod admission;
pub mod allocator;
pub mod api;
pub mod cache_policy;
pub mod capsule_materialize;
pub mod copy;
pub mod extent_map;
pub mod file_handle;
pub mod gc;
pub mod generation;
pub mod get_or_fetch;
pub mod handles;
pub mod journal;
pub mod mark_set;
pub mod mount;
pub mod native_restore;
pub mod object_plan;
pub mod observe;
pub mod outcome;
pub mod page_location;
pub mod page_provider;
pub mod publication_session;
pub mod read;
pub mod read_request;
pub mod readiness;
pub mod reconcile;
pub mod repair;
pub mod repository_extract;
pub mod repository_reader;
pub mod repository_writer;
pub mod restore;
pub mod seal;
pub mod space_lease;
pub mod update;
pub mod volume;
pub mod workspace;

pub use admission::{AdmissionStore, AdmittedSession, admit_sealed_session};
pub use api::{EngineResult, ReadEngine};
pub use capsule_materialize::{CapsulePageStore, MaterializeProgress, materialize_capsule};
pub use file_handle::{AccessMask, FileHandleContext};
pub use generation::MountGeneration;
pub use get_or_fetch::ProviderPage;
pub use observe::{ObservationEvent, SessionObserver, TelemetryMetrics};
pub use outcome::{CacheTier, ReadOutcome, SealViolation};
pub use page_location::{PageLocation, PageLocationMap};
pub use page_provider::{DEFAULT_FETCH_WORKERS, FetchContext, PageProvider};

/// Synchronous page-provider hook owned by the volume coordinator: admitted
/// fetch with a bounded transient fallback, typed errors, no zero-fills.
pub type ProviderHook =
    dyn Fn(mirage_types::PageHash) -> Result<ProviderPage, mirage_types::MirageError> + Send + Sync;
pub use read_request::{
    AccessPattern, BufferCacheMode, BufferingHint, ProcessRole, ReadContext, ReadPriority,
};
pub use readiness::{
    BackendCapability, CandidateRejection, CompileInput, EvictionGranularity, FilePlacementClass,
    HydrationGranularity, OriginEstimate, ReadinessIdentity, ReadinessVerdict, RequiredFile,
    ScopeSpec, UnsupportedPlan, compile_readiness,
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
