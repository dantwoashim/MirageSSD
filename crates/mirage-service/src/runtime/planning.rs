//! Capsule planning: ordinals and backend capability.

use super::config::ASSUMED_ORIGIN_GOODPUT_BYTES_PER_SECOND;
use super::config::ASSUMED_ORIGIN_TTFB_NS;
use super::drive::active_generation;
use super::profile::PROFILE_STARTUP_WINDOW_US;
use super::profile::held_out_violation_millionths;
use super::profile::load_profiles;
use super::profile::millionths;
use super::*;

/// The only presentation backend admitted today: the measured WinFsp baseline
/// per ADR 0008. No CFAPI/ProjFS descriptor exists because E1/E2 have not run.
const WINFSP_CAPABILITY: BackendCapability = BackendCapability {
    backend: PresentationBackend::WinFspProjection,
    qualified: true,
    hydration: HydrationGranularity::Page,
    eviction: EvictionGranularity::Page,
    whole_file_pin_only: false,
    metadata_bytes_per_unit: 64,
};

pub fn plan(
    database: &Database,
    repository_id: RepositoryId,
    full_volume: bool,
) -> Result<Value, MirageError> {
    let config = load_config(database, repository_id)?;
    let active = active_generation(database, repository_id)?;
    let index = MountIndex::open(active.mount_index_path.as_ref().ok_or_else(|| {
        MirageError::integrity_mismatch("active generation has no compiled mount index")
    })?)?;
    let profiles = load_profiles(
        database,
        repository_id,
        &ProfileBinding {
            repository_id,
            manifest_hash: active.manifest_hash,
            format_version: PROFILE_FORMAT_VERSION,
            label: format!("{}:{}", config.version_label, config.configuration_label),
        },
    )?;
    let hard = build_hard_set(
        &profiles,
        &HardSetPolicy {
            startup_window_us: PROFILE_STARTUP_WINDOW_US,
            minimum_session_count: 1,
            minimum_session_ratio_millionths: 1_000_000,
        },
        &BTreeSet::new(),
        &BTreeSet::new(),
        &BTreeSet::new(),
    )?;
    let mut all = RoaringBitmap::new();
    if full_volume {
        let page_count = u32::try_from(index.page_count())
            .map_err(|_| MirageError::unsupported_layout("mount index exceeds u32 pages"))?;
        all.insert_range(0..page_count);
    } else {
        for profile in &profiles {
            for observation in &profile.page_observations {
                all.insert(global_page_ordinal(
                    &index,
                    PageKey {
                        file_index: observation.file_index,
                        page_ordinal: observation.page_ordinal,
                    },
                )?);
            }
        }
        // Always pin the first and last page of every file: open-time probes
        // (AV signature scans, indexers, cache-manager sniffing) read file heads
        // and tails through the game's own handles and would otherwise record
        // seal violations on pages no profile observed.
        for ordinal in 0..index.file_count() {
            let file =
                index
                    .file_by_index(u32::try_from(ordinal).map_err(|_| {
                        MirageError::unsupported_layout("file ordinal exceeds u32")
                    })?)?;
            let size = file.logical_size();
            if size == 0 {
                continue;
            }
            let last = u32::try_from((size - 1) / index.header().page_size)
                .map_err(|_| MirageError::unsupported_layout("file page ordinal exceeds u32"))?;
            for page_ordinal in [0, last] {
                all.insert(global_page_ordinal(
                    &index,
                    PageKey {
                        file_index: file.ordinal(),
                        page_ordinal,
                    },
                )?);
            }
        }
    }
    let mut mandatory = RoaringBitmap::new();
    for page in hard {
        mandatory.insert(global_page_ordinal(&index, page.page)?);
    }
    if all.is_empty() {
        return Err(MirageError::invalid_argument(
            "profile set contains no resolvable pages",
        ));
    }
    let page_size = u32::try_from(index.header().page_size)
        .map_err(|_| MirageError::unsupported_layout("page size exceeds u32"))?;
    let resident_slots = database.load_resident_cache_slots()?;
    let resident_total_bytes = resident_slots
        .iter()
        .map(|slot| u64::from(slot.logical_length))
        .sum::<u64>();
    let resident_hashes: BTreeSet<PageHash> = resident_slots
        .iter()
        .filter_map(|slot| slot.page_hash)
        .collect();
    let mut files = Vec::new();
    for ordinal in 0..index.file_count() {
        let file = index.file_by_index(
            u32::try_from(ordinal)
                .map_err(|_| MirageError::unsupported_layout("file ordinal exceeds u32"))?,
        )?;
        let mut required_units = 0_u64;
        let mut required_bytes = 0_u64;
        let mut resident_bytes = 0_u64;
        let mut max_unit_bytes = 0_u64;
        for relative in 0..file.extent_count() {
            let extent = file.extent(relative)?;
            for within in 0..extent.page_count() {
                let global = extent
                    .page_start()
                    .checked_add(within)
                    .ok_or_else(|| MirageError::invalid_argument("page ordinal overflows"))?;
                if !all.contains(global) {
                    continue;
                }
                let page = index.page_by_ordinal(global)?;
                let length = u64::from(page.logical_length());
                required_units += 1;
                required_bytes += length;
                max_unit_bytes = max_unit_bytes.max(length);
                if resident_hashes.contains(&page.plaintext_hash()) {
                    resident_bytes += length;
                }
            }
        }
        if required_units == 0 {
            continue;
        }
        files.push(RequiredFile {
            file_index: file.ordinal(),
            logical_size: file.logical_size(),
            class: FilePlacementClass::Virtual,
            required_units,
            required_bytes,
            resident_bytes,
            max_unit_bytes,
        });
    }
    let violation = held_out_violation_millionths(&profiles);
    let dropped: u64 = profiles
        .iter()
        .map(|profile| profile.dropped_event_count)
        .sum();
    let observations: u64 = profiles
        .iter()
        .map(|profile| profile.page_observations.len() as u64)
        .sum();
    let data_loss = millionths(dropped, dropped.saturating_add(observations));
    let version_confidence = if profiles
        .windows(2)
        .all(|pair| pair[0].label == pair[1].label)
    {
        1_000_000
    } else {
        500_000
    };
    let mut reasons = vec![ClusterReason {
        cluster_id: 1,
        kind: ReasonKind::RecentUnion,
        pages: all.clone(),
        evidence_millionths: 1_000_000_u32.saturating_sub(violation),
    }];
    if !mandatory.is_empty() {
        reasons.push(ClusterReason {
            cluster_id: 0,
            kind: ReasonKind::HardSet,
            pages: mandatory.clone(),
            evidence_millionths: 1_000_000,
        });
    }
    let plan = CapsulePlan::new(CapsuleDraft {
        repository_id,
        generation: active.generation_id,
        profile_key: ProfileKey(format!(
            "{}:{}",
            config.version_label, config.configuration_label
        )),
        page_set: all,
        mandatory_set: mandatory,
        frontier_set: RoaringBitmap::new(),
        page_size,
        risk: RiskEstimate {
            held_out_violation_millionths: violation,
            unseen_branch_mass_millionths: violation,
            data_quality_millionths: 1_000_000_u32.saturating_sub(data_loss),
            version_transfer_confidence_millionths: version_confidence,
        },
        reasons,
    })?;
    let scope_resident_bytes = files.iter().map(|file| file.resident_bytes).sum::<u64>();
    let record = match compile_readiness(&CompileInput {
        identity: ReadinessIdentity {
            repository_id,
            generation: active.generation_id,
            manifest_hash: active.manifest_hash,
            configuration_label: format!("{}:{}", config.version_label, config.configuration_label),
            profile_schema_version: PROFILE_FORMAT_VERSION,
        },
        scope: ScopeSpec {
            scope_id: plan.capsule_id.to_string(),
            completeness: if full_volume {
                ScopeCompleteness::Complete
            } else {
                ScopeCompleteness::Empirical
            },
            files,
            lead_time_ns: if full_volume {
                None
            } else {
                Some(PROFILE_STARTUP_WINDOW_US * 1000)
            },
        },
        budget_bytes: config.cache_bytes,
        current: SpatialEnvelope {
            allocated: resident_total_bytes,
            reserved_new_allocation: 0,
            dirty_staging: 0,
            rollback_retention: 0,
            journal_and_metadata: 0,
            filesystem_slack: 0,
        },
        candidates: vec![WINFSP_CAPABILITY],
        origin: OriginEstimate {
            available: true,
            queue_delay_ns: 0,
            source_ttfb_ns: ASSUMED_ORIGIN_TTFB_NS,
            goodput_bytes_per_second: ASSUMED_ORIGIN_GOODPUT_BYTES_PER_SECOND,
            decode_ns_per_unit: 0,
            verify_ns_per_unit: 0,
            placement_ns_per_unit: 0,
            safety_margin_ns: 0,
        },
        qualification: QualificationVersions {
            backend: "winfsp".into(),
            os_build: "unrecorded".into(),
            driver: "unrecorded".into(),
            runtime: env!("CARGO_PKG_VERSION").into(),
        },
        pin_generation: active.generation_id.as_u64(),
        ram_credit_bytes: 0,
        max_in_flight: u32::try_from(mirage_engine::DEFAULT_FETCH_WORKERS).unwrap_or(u32::MAX),
    })? {
        ReadinessVerdict::Ready { record, .. } => *record,
        ReadinessVerdict::Unsupported(unsupported) => {
            let first = unsupported
                .rejections
                .first()
                .map(|rejection| rejection.detail.as_str())
                .unwrap_or("no rejection detail");
            let message = format!(
                "no qualified readiness plan: {} ({first}); cache budget {} bytes",
                unsupported.detail, config.cache_bytes
            );
            return Err(
                if unsupported.failed.contains(&ReadinessConstraint::Spatial) {
                    MirageError::cache_full(message)
                } else if unsupported
                    .failed
                    .contains(&ReadinessConstraint::Compatibility)
                {
                    MirageError::unsupported_layout(message)
                } else {
                    MirageError::backend_unavailable(message)
                },
            );
        }
    };
    let capsule_root = repository_state_root(database, repository_id)?.join("capsules");
    std::fs::create_dir_all(&capsule_root).map_err(MirageError::from)?;
    write_json_atomic(
        &capsule_root.join(format!("{}.json", plan.capsule_id)),
        &plan,
    )?;
    write_json_atomic(
        &capsule_root.join(format!("{}.readiness.json", plan.capsule_id)),
        &record,
    )?;
    Ok(json!({
        "repository_id": repository_id.to_string(),
        "capsule_id": plan.capsule_id.to_string(),
        "full_volume": full_volume,
        "generation": plan.generation.as_u64(),
        "page_count": plan.page_set.len(),
        "mandatory_pages": plan.mandatory_set.len(),
        "total_bytes": plan.total_bytes,
        "cache_budget_bytes": config.cache_bytes,
        "held_out_violation_millionths": plan.risk.held_out_violation_millionths,
        "data_quality_millionths": plan.risk.data_quality_millionths,
        "readiness": {
            "mode": record.mode.as_str(),
            "scope_completeness": record.scope_completeness.as_str(),
            "presentation": record.presentation.as_str(),
            "required_bytes": record.required_bytes,
            "required_units": record.required_units,
            "resident_bytes": scope_resident_bytes,
            "spatial_total_bytes": record.spatial.total()?,
            "budget_bytes": record.budget_bytes,
            "temporal_lead_ns": record.temporal_lead_ns,
            "invalidation_conditions": record.invalidation_conditions,
        }
    }))
}

pub fn load_plan(
    database: &Database,
    repository_id: RepositoryId,
    capsule_id: mirage_types::CapsuleId,
) -> Result<CapsulePlan, MirageError> {
    let path = repository_state_root(database, repository_id)?
        .join("capsules")
        .join(format!("{capsule_id}.json"));
    let bytes = bounded_read(&path, 256 * 1024 * 1024)?;
    let plan: CapsulePlan = serde_json::from_slice(&bytes).map_err(|error| {
        MirageError::integrity_mismatch("capsule plan is malformed").with_source(error)
    })?;
    let validated = CapsulePlan::new(CapsuleDraft {
        repository_id: plan.repository_id,
        generation: plan.generation,
        profile_key: plan.profile_key.clone(),
        page_set: plan.page_set.clone(),
        mandatory_set: plan.mandatory_set.clone(),
        frontier_set: plan.frontier_set.clone(),
        page_size: plan.page_size,
        risk: plan.risk,
        reasons: plan.reasons.clone(),
    })?;
    if validated != plan || plan.repository_id != repository_id || plan.capsule_id != capsule_id {
        return Err(MirageError::integrity_mismatch(
            "capsule plan identity is invalid",
        ));
    }
    Ok(plan)
}

pub(super) fn file_ordinals(
    index: &MountIndex,
) -> Result<BTreeMap<StableFileId, u32>, MirageError> {
    let mut result = BTreeMap::new();
    for ordinal in 0..index.file_count() {
        let ordinal = u32::try_from(ordinal)
            .map_err(|_| MirageError::unsupported_layout("file count exceeds u32"))?;
        result.insert(index.file_by_index(ordinal)?.stable_id(), ordinal);
    }
    Ok(result)
}

pub(super) fn relative_page_ordinal(
    file: mirage_index::FileView<'_>,
    global: u32,
) -> Result<u32, MirageError> {
    let mut relative = 0_u32;
    for extent_index in 0..file.extent_count() {
        let extent = file.extent(extent_index)?;
        if global >= extent.page_start()
            && global < extent.page_start().saturating_add(extent.page_count())
        {
            return relative
                .checked_add(global - extent.page_start())
                .ok_or_else(|| MirageError::manifest_invalid("relative page ordinal overflows"));
        }
        relative = relative
            .checked_add(extent.page_count())
            .ok_or_else(|| MirageError::manifest_invalid("relative page count overflows"))?;
    }
    Err(MirageError::manifest_invalid(
        "resolved page does not belong to file",
    ))
}

fn global_page_ordinal(index: &MountIndex, key: PageKey) -> Result<u32, MirageError> {
    let file = index.file_by_index(key.file_index)?;
    let mut remaining = key.page_ordinal;
    for extent_index in 0..file.extent_count() {
        let extent = file.extent(extent_index)?;
        if remaining < extent.page_count() {
            return extent
                .page_start()
                .checked_add(remaining)
                .ok_or_else(|| MirageError::manifest_invalid("global page ordinal overflows"));
        }
        remaining -= extent.page_count();
    }
    Err(MirageError::integrity_mismatch(
        "profile page ordinal exceeds its file",
    ))
}
