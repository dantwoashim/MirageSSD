use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use mirage_backend::{FetchClass, ObjectBackend, RemoteObjectRef};
use mirage_backend_drive::{DriveObjectBackend, NativeHttpTransport, RetryingHttpTransport};
use mirage_cache::{
    ArenaShard, CacheLayout, InsertOutcome, IntegrityClass, ResidentIndex, VerifyOutcome,
    insert_reserved_page, verify_page,
};
use mirage_crypto::repository_key_store::load_repository_key;
use mirage_db::{
    CacheShardSpec, CacheSlotRecord, Database, LeaseSpec, NewRepository, NewSealedSession,
    ReserveCacheSlotOutcome, VerifiedGeneration,
};
use mirage_engine::{
    AdmissionStore, BackendCapability, CapsulePageStore, CompileInput, EvictionGranularity,
    FilePlacementClass, HydrationGranularity, MaterializeProgress, OriginEstimate,
    ReadinessIdentity, ReadinessVerdict, RequiredFile, ScopeSpec, admit_sealed_session,
    compile_readiness, materialize_capsule,
};
use mirage_index::{MountIndex, NodeIndex, compile_to_bytes};
use mirage_ipc::DriveQuotaSnapshot;
use mirage_manifest::{
    Codec, DecodeLimits, RepositoryManifest, decode_manifest_bounded, manifest_hash,
};
use mirage_pack::{
    EncryptedFrameAad, PackReadEncryption, PackReader, decode_encrypted_frame,
    encrypted_frame_pack_id,
};
use mirage_predictor::binding::{PROFILE_FORMAT_VERSION, ProfileBinding, bind_profile};
use mirage_predictor::capsule::{
    CapsuleDraft, CapsulePlan, ClusterReason, ProfileKey, ReasonKind, RiskEstimate,
};
use mirage_predictor::hard_set::{HardSetPolicy, PageKey, build as build_hard_set};
use mirage_predictor::{
    GameProfile, ObservationClass, PageObservation, ProcessRecord, ProfileProcessRole,
    TraceBlockDecoder, TraceBlockEncoder, TraceEvent, TraceHeader, normalize_trace,
};
use mirage_simulator::{BaselineReplay, NetworkModel, ReplayConfig};
use mirage_types::{
    ByteCount, CheckedRange, CommitHash, GenerationId, MirageError, PageHash, PresentationBackend,
    QualificationVersions, ReadinessConstraint, RepositoryEvent, RepositoryId, RepositoryState,
    ScopeCompleteness, SessionEvent, SessionId, SessionState, SpatialEnvelope, StableFileId,
};
use roaring::RoaringBitmap;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

use crate::{
    LaunchMode, LaunchPolicy, LaunchReadiness, NativeLaunch, ProfileLaunch, launch_native,
    run_profile_session,
};

pub(crate) mod capacity;
pub(crate) mod config;
pub(crate) mod conversion;
pub(crate) mod drive;
pub(crate) mod fs_util;
pub(crate) mod lifecycle;
pub(crate) mod materialize;
pub(crate) mod mount;
pub(crate) mod planning;
pub(crate) mod profile;
#[cfg(test)]
mod tests;

pub(crate) use capacity::drive_capacity_snapshot;
pub(crate) use capacity::refresh_drive_capacity;
pub(crate) use capacity::volume_capacity;
pub(crate) use config::RegisterSpec;
pub(crate) use config::RuntimeConfig;
pub(crate) use config::RuntimeOrigin;
pub(crate) use config::load_config;
pub(crate) use config::prepare_journal_root;
pub(crate) use config::repository_cache_root;
pub(crate) use config::repository_state_root;
pub(crate) use config::resolve_cache_root_request;
pub(crate) use config::service_state_root;
pub(crate) use conversion::NativeBackupInfo;
pub(crate) use conversion::convert;
pub(crate) use conversion::directory_inventory;
pub(crate) use conversion::evict_verified_native_backup;
pub(crate) use conversion::native_backup_info;
pub(crate) use conversion::reconcile_native_backup_eviction;
pub(crate) use conversion::restore_native;
pub(crate) use drive::drive_backend;
pub(crate) use drive::load_drive_manifest;
pub(crate) use fs_util::bounded_read;
pub(crate) use fs_util::now_ns;
pub(crate) use fs_util::write_json_atomic;
pub(crate) use lifecycle::configure;
pub(crate) use lifecycle::register;
pub(crate) use lifecycle::set_drive_origin;
pub(crate) use lifecycle::set_volume_mode;
pub(crate) use materialize::RuntimeLaunch;
pub(crate) use materialize::admit;
pub(crate) use materialize::launch;
pub(crate) use materialize::materialize;
pub(crate) use materialize::open_cache;
pub(crate) use mount::clear_mount_record;
pub(crate) use mount::load_mount_record;
pub(crate) use mount::mount_target_exists;
pub(crate) use mount::save_mount_record;
pub(crate) use mount::validate_explorer_mount_ready;
pub(crate) use mount::validate_mount_ready;
pub(crate) use planning::load_plan;
pub(crate) use planning::plan;
pub(crate) use profile::capture;
pub(crate) use profile::simulate;
