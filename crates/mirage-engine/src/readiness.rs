//! Readiness compiler: binds a declared scope, a managed budget, and an origin
//! estimate into a versioned readiness record, or an explained unsupported plan.

use serde::{Deserialize, Serialize};

use mirage_types::{
    GenerationId, ManifestHash, MirageError, PresentationBackend, QualificationVersions,
    READINESS_SCHEMA_VERSION, ReadinessConstraint, ReadinessMode, ReadinessRecord, RepositoryId,
    ScopeCompleteness, SpatialEnvelope, TemporalEstimate,
};

const MAX_SCOPE_ID_BYTES: usize = 1024;

/// Worst-case allocation closure a presentation backend can request per file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HydrationGranularity {
    /// Required bytes only.
    Page,
    /// The file's full logical size.
    WholeFile,
    /// The file's full logical size, streamed progressively.
    ProgressiveFile,
}

/// Smallest unit a presentation backend can evict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvictionGranularity {
    Page,
    WholeFile,
    None,
}

/// What a presentation backend can honestly do for this title and version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendCapability {
    pub backend: PresentationBackend,
    /// E0-E2 gates passed for this title/version; false fails Compatibility.
    pub qualified: bool,
    pub hydration: HydrationGranularity,
    pub eviction: EvictionGranularity,
    /// Pins are file-level intent (CFAPI); Complete scopes then reserve full files.
    pub whole_file_pin_only: bool,
    /// Index/outboard/journal overhead charged per required unit.
    pub metadata_bytes_per_unit: u64,
}

/// `Native` files (executables, anti-cheat, mutable state) always occupy full
/// native bytes; `Virtual` files may be projected by a presentation backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilePlacementClass {
    Native,
    Virtual,
}

/// One file's requirement inside a scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequiredFile {
    pub file_index: u32,
    pub logical_size: u64,
    pub class: FilePlacementClass,
    pub required_units: u64,
    /// Required bytes within the scope.
    pub required_bytes: u64,
    /// Verified-resident bytes; never counted twice.
    pub resident_bytes: u64,
    /// Largest single verification/transfer unit in the file.
    pub max_unit_bytes: u64,
}

/// The scope a readiness claim is bound to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeSpec {
    pub scope_id: String,
    pub completeness: ScopeCompleteness,
    pub files: Vec<RequiredFile>,
    pub lead_time_ns: Option<u64>,
}

/// Measured origin delivery facts used for temporal admission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OriginEstimate {
    pub available: bool,
    pub queue_delay_ns: u64,
    pub source_ttfb_ns: u64,
    pub goodput_bytes_per_second: u64,
    pub decode_ns_per_unit: u64,
    pub verify_ns_per_unit: u64,
    pub placement_ns_per_unit: u64,
    pub safety_margin_ns: u64,
}

/// Identities a readiness record is bound to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadinessIdentity {
    pub repository_id: RepositoryId,
    pub generation: GenerationId,
    pub manifest_hash: ManifestHash,
    pub configuration_label: String,
    pub profile_schema_version: u32,
}

/// Pure compiler input; nothing is mutated or reserved by compiling.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompileInput {
    pub identity: ReadinessIdentity,
    pub scope: ScopeSpec,
    pub budget_bytes: u64,
    /// Measured now; `allocated` already includes resident bytes.
    pub current: SpatialEnvelope,
    /// Backend candidates in preference order.
    pub candidates: Vec<BackendCapability>,
    pub origin: OriginEstimate,
    pub qualification: QualificationVersions,
    pub pin_generation: u64,
    pub ram_credit_bytes: u64,
    pub max_in_flight: u32,
}

/// One candidate's refusal, naming the constraint family that rejected it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateRejection {
    pub backend: PresentationBackend,
    pub constraint: ReadinessConstraint,
    pub detail: String,
}

/// Every constraint that refused a plan, with per-candidate explanations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnsupportedPlan {
    /// Deduped, in `ReadinessConstraint::ALL` order.
    pub failed: Vec<ReadinessConstraint>,
    pub rejections: Vec<CandidateRejection>,
    pub detail: String,
}

/// The compiler verdict: exactly one honest readiness mode, or the failure
/// detail needed to explain why no plan is claimable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadinessVerdict {
    Ready {
        record: Box<ReadinessRecord>,
        /// Candidates rejected before the winning one, for diagnostics.
        rejections: Vec<CandidateRejection>,
    },
    Unsupported(UnsupportedPlan),
}

fn invalid_argument(message: impl Into<String>) -> MirageError {
    MirageError::invalid_argument(message)
}

fn checked_add(left: u64, right: u64, what: &str) -> Result<u64, MirageError> {
    left.checked_add(right)
        .ok_or_else(|| invalid_argument(format!("{what} overflows u64")))
}

fn validate_input(input: &CompileInput) -> Result<(), MirageError> {
    if input.scope.scope_id.is_empty() || input.scope.scope_id.len() > MAX_SCOPE_ID_BYTES {
        return Err(invalid_argument(
            "readiness scope id is empty or exceeds 1024 bytes",
        ));
    }
    if input.scope.files.is_empty() {
        return Err(invalid_argument("readiness scope declares no files"));
    }
    if input.candidates.is_empty() {
        return Err(invalid_argument(
            "readiness compile requires at least one backend candidate",
        ));
    }
    for file in &input.scope.files {
        if file.required_bytes > file.logical_size {
            return Err(invalid_argument(
                "required bytes exceed the file's logical size",
            ));
        }
        if file.resident_bytes > file.required_bytes {
            return Err(invalid_argument(
                "resident bytes exceed the file's required bytes",
            ));
        }
        if file.max_unit_bytes > file.logical_size {
            return Err(invalid_argument(
                "maximum unit bytes exceed the file's logical size",
            ));
        }
    }
    Ok(())
}

/// Compiles a readiness verdict. Pure: it never mutates storage, never
/// reserves budget, and never returns a `Ready` record with `Unsupported` mode.
///
/// Order per candidate: compatibility, spatial, source, then temporal
/// (empirical scopes only). The first refusing constraint rejects the
/// candidate; every candidate's rejection is retained in the verdict.
pub fn compile_readiness(input: &CompileInput) -> Result<ReadinessVerdict, MirageError> {
    validate_input(input)?;

    let mut native_missing = 0_u64;
    let mut native_required = 0_u64;
    let mut native_file_count = 0_u64;
    let mut units = 0_u64;
    let mut max_unit_bytes = 0_u64;
    let mut virtual_file_count = 0_u64;
    for file in &input.scope.files {
        units = checked_add(units, file.required_units, "required units")?;
        max_unit_bytes = max_unit_bytes.max(file.max_unit_bytes);
        match file.class {
            FilePlacementClass::Native => {
                native_file_count = checked_add(native_file_count, 1, "native file count")?;
                native_required =
                    checked_add(native_required, file.logical_size, "native required bytes")?;
                let missing = file
                    .logical_size
                    .checked_sub(file.resident_bytes)
                    .ok_or_else(|| {
                        invalid_argument("native resident bytes exceed the file's logical size")
                    })?;
                native_missing = checked_add(native_missing, missing, "native missing bytes")?;
            }
            FilePlacementClass::Virtual => {
                virtual_file_count = checked_add(virtual_file_count, 1, "virtual file count")?;
            }
        }
    }

    let mut rejections = Vec::new();
    for candidate in &input.candidates {
        if !candidate.qualified {
            rejections.push(CandidateRejection {
                backend: candidate.backend,
                constraint: ReadinessConstraint::Compatibility,
                detail: "backend not qualified for this title/version".into(),
            });
            continue;
        }
        let mut virtual_missing = 0_u64;
        let mut virtual_required = 0_u64;
        let mut all_virtual_full = true;
        for file in input
            .scope
            .files
            .iter()
            .filter(|file| file.class == FilePlacementClass::Virtual)
        {
            let closure = if candidate.hydration == HydrationGranularity::Page
                && !(candidate.whole_file_pin_only
                    && input.scope.completeness == ScopeCompleteness::Complete)
            {
                file.required_bytes
            } else {
                file.logical_size
            };
            virtual_missing = checked_add(
                virtual_missing,
                closure.saturating_sub(file.resident_bytes),
                "virtual missing bytes",
            )?;
            virtual_required = checked_add(virtual_required, closure, "virtual required bytes")?;
            all_virtual_full &= closure == file.logical_size;
        }
        let missing = checked_add(native_missing, virtual_missing, "missing bytes")?;
        let metadata = units
            .checked_mul(candidate.metadata_bytes_per_unit)
            .ok_or_else(|| invalid_argument("unit metadata bytes overflow u64"))?;
        let mut envelope = input.current.with_additional_reservation(missing)?;
        envelope.journal_and_metadata = envelope
            .journal_and_metadata
            .checked_add(metadata)
            .ok_or_else(|| invalid_argument("journal and metadata bytes overflow u64"))?;
        if !envelope.admits(input.budget_bytes)? {
            rejections.push(CandidateRejection {
                backend: candidate.backend,
                constraint: ReadinessConstraint::Spatial,
                detail: format!(
                    "envelope {} exceeds budget {}",
                    envelope.total()?,
                    input.budget_bytes
                ),
            });
            continue;
        }
        // Empirical scopes always require an origin: adaptive play may need
        // unobserved bytes even when the observed set is fully resident.
        let needs_origin = missing > 0 || input.scope.completeness == ScopeCompleteness::Empirical;
        if needs_origin && !input.origin.available {
            rejections.push(CandidateRejection {
                backend: candidate.backend,
                constraint: ReadinessConstraint::Source,
                detail: "required bytes are not resident and no authorized origin is available"
                    .into(),
            });
            continue;
        }
        if input.scope.completeness == ScopeCompleteness::Empirical {
            let lead = match input.scope.lead_time_ns {
                Some(lead) => lead,
                None => {
                    rejections.push(CandidateRejection {
                        backend: candidate.backend,
                        constraint: ReadinessConstraint::Temporal,
                        detail: "adaptive scope declares no lead time".into(),
                    });
                    continue;
                }
            };
            let estimate = TemporalEstimate {
                queue_delay_ns: input.origin.queue_delay_ns,
                source_ttfb_ns: input.origin.source_ttfb_ns,
                encoded_bytes: max_unit_bytes,
                goodput_bytes_per_second: input.origin.goodput_bytes_per_second,
                decode_ns: input.origin.decode_ns_per_unit,
                verify_ns: input.origin.verify_ns_per_unit,
                placement_ns: input.origin.placement_ns_per_unit,
                safety_margin_ns: input.origin.safety_margin_ns,
            };
            if !estimate.admits(0, lead)? {
                rejections.push(CandidateRejection {
                    backend: candidate.backend,
                    constraint: ReadinessConstraint::Temporal,
                    detail: format!(
                        "verified availability {} exceeds lead time {}",
                        estimate.verified_available_ns(0)?,
                        lead
                    ),
                });
                continue;
            }
        }

        let presentation = if virtual_file_count == 0 {
            PresentationBackend::NativeFiles
        } else {
            candidate.backend
        };
        let mode = match input.scope.completeness {
            ScopeCompleteness::Complete
                if presentation == PresentationBackend::NativeFiles && all_virtual_full =>
            {
                ReadinessMode::FullLocal
            }
            ScopeCompleteness::Complete => ReadinessMode::VerifiedScope,
            ScopeCompleteness::Empirical => ReadinessMode::ProfiledAdaptive,
        };
        let mut invalidation_conditions = vec![
            "manifest_hash_changed".to_string(),
            "configuration_label_changed".to_string(),
            "backend_qualification_changed".to_string(),
            "pin_generation_advanced".to_string(),
            "budget_reduced_below_envelope".to_string(),
        ];
        if mode == ReadinessMode::ProfiledAdaptive {
            invalidation_conditions.push("origin_unavailable".to_string());
        }
        let record = ReadinessRecord {
            schema_version: READINESS_SCHEMA_VERSION,
            repository_id: input.identity.repository_id,
            generation: input.identity.generation,
            manifest_hash: input.identity.manifest_hash,
            configuration_label: input.identity.configuration_label.clone(),
            profile_schema_version: input.identity.profile_schema_version,
            scope_id: input.scope.scope_id.clone(),
            scope_completeness: input.scope.completeness,
            mode,
            required_units: units,
            required_bytes: checked_add(
                native_required,
                virtual_required,
                "record required bytes",
            )?,
            native_file_count,
            presentation,
            spatial: envelope,
            budget_bytes: input.budget_bytes,
            temporal_lead_ns: input.scope.lead_time_ns.unwrap_or(0),
            ram_credit_bytes: input.ram_credit_bytes,
            max_in_flight: input.max_in_flight,
            qualification: input.qualification.clone(),
            pin_generation: input.pin_generation,
            invalidation_conditions,
        };
        record.validate()?;
        return Ok(ReadinessVerdict::Ready {
            record: Box::new(record),
            rejections,
        });
    }

    let failed = ReadinessConstraint::ALL
        .iter()
        .copied()
        .filter(|constraint| {
            rejections
                .iter()
                .any(|rejection| rejection.constraint == *constraint)
        })
        .collect();
    Ok(ReadinessVerdict::Unsupported(UnsupportedPlan {
        failed,
        rejections,
        detail: "no qualified plan meets compatibility, space, source and delivery constraints under this budget".into(),
    }))
}
