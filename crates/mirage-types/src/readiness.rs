//! Readiness terminology, admission envelopes, and the durable readiness record.

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use crate::error::{MirageError, MirageErrorKind};
use crate::hash::ManifestHash;
use crate::id::{GenerationId, RepositoryId};
use crate::state::state_enum;

/// Schema version of [`ReadinessRecord`]; only version 1 exists.
pub const READINESS_SCHEMA_VERSION: u32 = 1;

const MAX_SCOPE_ID_BYTES: usize = 1024;
const MAX_LABEL_BYTES: usize = 255;
const MAX_INVALIDATION_CONDITIONS: usize = 64;

state_enum! {
    /// Readiness modes from the continuum architecture: distinct promises, never blended.
    ReadinessMode {
        VerifiedScope => "verified_scope",
        ProfiledAdaptive => "profiled_adaptive",
        FullLocal => "full_local",
        Maintenance => "maintenance",
        Unsupported => "unsupported",
    }
}

state_enum! {
    /// Whether a declared scope is a complete dependency closure or an empirical observation set.
    ScopeCompleteness {
        Complete => "complete",
        Empirical => "empirical",
    }
}

state_enum! {
    /// Presentation technology chosen for a repository under a readiness plan.
    PresentationBackend {
        NativeFiles => "native_files",
        WinFspProjection => "win_fsp_projection",
        CloudFiles => "cloud_files",
        ProjFs => "proj_fs",
        AssetInterface => "asset_interface",
    }
}

state_enum! {
    /// Constraint families that can independently refuse a readiness claim.
    ReadinessConstraint {
        Spatial => "spatial",
        Temporal => "temporal",
        Compatibility => "compatibility",
        Source => "source",
        Trust => "trust",
        Authorization => "authorization",
    }
}

state_enum! {
    /// Distinct fetch-failure causes; never collapse them into zeros, false EOF, or fake success.
    FetchFailureCause {
        ChecksumMismatch => "checksum_mismatch",
        AuthorizationDenied => "authorization_denied",
        SourceMissing => "source_missing",
        Timeout => "timeout",
        BudgetExhausted => "budget_exhausted",
        CallerCancelled => "caller_cancelled",
        MalformedResponse => "malformed_response",
        Internal => "internal",
    }
}

impl FetchFailureCause {
    /// Classifies a [`MirageError`] into the fetch-failure taxonomy.
    #[must_use]
    pub fn from_error(error: &MirageError) -> Self {
        match error.kind {
            MirageErrorKind::IntegrityMismatch
            | MirageErrorKind::ManifestInvalid
            | MirageErrorKind::UnsupportedLayout => Self::ChecksumMismatch,
            MirageErrorKind::BackendUnauthenticated | MirageErrorKind::BackendPermissionDenied => {
                Self::AuthorizationDenied
            }
            MirageErrorKind::RemoteObjectMissing | MirageErrorKind::ProviderUnavailable => {
                Self::SourceMissing
            }
            MirageErrorKind::BackendUnavailable
            | MirageErrorKind::BackendRateLimited
            | MirageErrorKind::DeadlineExceeded => Self::Timeout,
            MirageErrorKind::CacheFull => Self::BudgetExhausted,
            MirageErrorKind::Cancelled => Self::CallerCancelled,
            MirageErrorKind::InvalidArgument => Self::MalformedResponse,
            _ => Self::Internal,
        }
    }
}

/// Physical storage admission envelope in bytes. Categories never double-count
/// the same reservation and never omit simultaneous copies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct SpatialEnvelope {
    pub allocated: u64,
    pub reserved_new_allocation: u64,
    pub dirty_staging: u64,
    pub rollback_retention: u64,
    pub journal_and_metadata: u64,
    pub filesystem_slack: u64,
}

impl SpatialEnvelope {
    /// Checked sum of all six categories.
    pub fn total(&self) -> Result<u64, MirageError> {
        [
            self.allocated,
            self.reserved_new_allocation,
            self.dirty_staging,
            self.rollback_retention,
            self.journal_and_metadata,
            self.filesystem_slack,
        ]
        .into_iter()
        .try_fold(0_u64, |acc, part| {
            acc.checked_add(part)
                .ok_or_else(|| MirageError::invalid_argument("spatial envelope total overflows"))
        })
    }

    /// Whether the envelope fits within the managed budget.
    pub fn admits(&self, budget_bytes: u64) -> Result<bool, MirageError> {
        Ok(self.total()? <= budget_bytes)
    }

    /// Returns a copy with `reserved_new_allocation` increased by `bytes`.
    pub fn with_additional_reservation(self, bytes: u64) -> Result<Self, MirageError> {
        Ok(Self {
            reserved_new_allocation: self
                .reserved_new_allocation
                .checked_add(bytes)
                .ok_or_else(|| MirageError::invalid_argument("spatial reservation overflows"))?,
            ..self
        })
    }
}

/// Latest-start delivery estimate in nanoseconds for one required unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct TemporalEstimate {
    pub queue_delay_ns: u64,
    pub source_ttfb_ns: u64,
    pub encoded_bytes: u64,
    pub goodput_bytes_per_second: u64,
    pub decode_ns: u64,
    pub verify_ns: u64,
    pub placement_ns: u64,
    pub safety_margin_ns: u64,
}

impl TemporalEstimate {
    /// Ceiling of `encoded_bytes / goodput_bytes_per_second` in nanoseconds.
    pub fn transfer_ns(&self) -> Result<u64, MirageError> {
        if self.goodput_bytes_per_second == 0 {
            return Err(MirageError::invalid_argument(
                "temporal estimate has zero goodput",
            ));
        }
        let numerator = u128::from(self.encoded_bytes) * 1_000_000_000_u128;
        let value = numerator.div_ceil(u128::from(self.goodput_bytes_per_second));
        u64::try_from(value)
            .map_err(|_| MirageError::invalid_argument("transfer time exceeds u64 nanoseconds"))
    }

    /// Earliest verified availability instant starting the fetch at `start_ns`.
    pub fn verified_available_ns(&self, start_ns: u64) -> Result<u64, MirageError> {
        [
            start_ns,
            self.queue_delay_ns,
            self.source_ttfb_ns,
            self.transfer_ns()?,
            self.decode_ns,
            self.verify_ns,
            self.placement_ns,
            self.safety_margin_ns,
        ]
        .into_iter()
        .try_fold(0_u64, |acc, part| {
            acc.checked_add(part).ok_or_else(|| {
                MirageError::invalid_argument("verified availability overflows u64 nanoseconds")
            })
        })
    }

    /// Whether the unit can be verified available by `required_ns`.
    pub fn admits(&self, start_ns: u64, required_ns: u64) -> Result<bool, MirageError> {
        Ok(self.verified_available_ns(start_ns)? <= required_ns)
    }
}

/// Qualification versions under which a readiness record was issued.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct QualificationVersions {
    pub backend: String,
    pub os_build: String,
    pub driver: String,
    pub runtime: String,
}

/// Durable readiness record binding identity, scope, reservations, and
/// invalidation conditions. A signed record authenticates its issuer and
/// contents; it does not prove a model predicts every future read.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct ReadinessRecord {
    pub schema_version: u32,
    pub repository_id: RepositoryId,
    pub generation: GenerationId,
    pub manifest_hash: ManifestHash,
    pub configuration_label: String,
    pub profile_schema_version: u32,
    pub scope_id: String,
    pub scope_completeness: ScopeCompleteness,
    pub mode: ReadinessMode,
    pub required_units: u64,
    pub required_bytes: u64,
    pub native_file_count: u64,
    pub presentation: PresentationBackend,
    pub spatial: SpatialEnvelope,
    pub budget_bytes: u64,
    pub temporal_lead_ns: u64,
    pub ram_credit_bytes: u64,
    pub max_in_flight: u32,
    pub qualification: QualificationVersions,
    pub pin_generation: u64,
    pub invalidation_conditions: Vec<String>,
}

impl ReadinessRecord {
    /// Enforces the readiness-v1 invariants. A record is only issued for a
    /// supported plan, so `Unsupported` mode is rejected here.
    pub fn validate(&self) -> Result<(), MirageError> {
        if self.schema_version != READINESS_SCHEMA_VERSION {
            return Err(MirageError::invalid_argument(
                "readiness record schema version is not 1",
            ));
        }
        if self.scope_id.is_empty() || self.scope_id.len() > MAX_SCOPE_ID_BYTES {
            return Err(MirageError::invalid_argument(
                "readiness scope id is empty or exceeds 1024 bytes",
            ));
        }
        if self.configuration_label.is_empty() || self.configuration_label.len() > MAX_LABEL_BYTES {
            return Err(MirageError::invalid_argument(
                "readiness configuration label is empty or exceeds 255 bytes",
            ));
        }
        match self.mode {
            ReadinessMode::Unsupported => {
                return Err(MirageError::invalid_argument(
                    "readiness record cannot claim unsupported mode",
                ));
            }
            ReadinessMode::VerifiedScope
                if self.scope_completeness != ScopeCompleteness::Complete =>
            {
                return Err(MirageError::invalid_argument(
                    "verified scope requires complete dependency closure",
                ));
            }
            ReadinessMode::ProfiledAdaptive
                if self.scope_completeness != ScopeCompleteness::Empirical =>
            {
                return Err(MirageError::invalid_argument(
                    "profiled adaptive mode requires empirical scope completeness",
                ));
            }
            _ => {}
        }
        if !self.spatial.admits(self.budget_bytes)? {
            return Err(MirageError::invalid_argument(
                "spatial envelope exceeds the managed budget",
            ));
        }
        if self.invalidation_conditions.len() > MAX_INVALIDATION_CONDITIONS
            || self
                .invalidation_conditions
                .iter()
                .any(|condition| condition.len() > MAX_LABEL_BYTES)
        {
            return Err(MirageError::invalid_argument(
                "invalidation conditions exceed readiness-v1 bounds",
            ));
        }
        if self.max_in_flight == 0 {
            return Err(MirageError::invalid_argument(
                "readiness record requires a non-zero in-flight limit",
            ));
        }
        Ok(())
    }
}
