//! Capture and simulation of launch profiles and traces.

use super::config::ASSUMED_ORIGIN_GOODPUT_BYTES_PER_SECOND;
use super::config::ASSUMED_ORIGIN_TTFB_NS;
use super::drive::active_generation;
use super::fs_util::write_atomic;
use super::planning::file_ordinals;
use super::planning::relative_page_ordinal;
use super::*;

pub(super) const PROFILE_STARTUP_WINDOW_US: u64 = 30_000_000;

pub fn capture(
    database: &Database,
    repository_id: RepositoryId,
    maximum_duration_seconds: u32,
) -> Result<Value, MirageError> {
    if !(1..=21_600).contains(&maximum_duration_seconds) {
        return Err(MirageError::invalid_argument(
            "profile maximum duration must be between 1 second and 6 hours",
        ));
    }
    let config = load_config(database, repository_id)?;
    let active = active_generation(database, repository_id)?;
    let index_path = active.mount_index_path.as_ref().ok_or_else(|| {
        MirageError::integrity_mismatch("active generation has no compiled mount index")
    })?;
    let index = MountIndex::open(index_path)?;
    let launcher = config.native_root.join(&config.launcher_relative);
    let result = run_profile_session(&ProfileLaunch {
        game_root: config.native_root.clone(),
        launcher,
        arguments: config.arguments.clone(),
        maximum_runtime: Duration::from_secs(u64::from(maximum_duration_seconds)),
        drain_interval: Duration::from_millis(config.drain_ms),
        version_label: config.version_label.clone(),
        configuration_label: config.configuration_label.clone(),
    })?;

    let mut trace_events = Vec::new();
    let profile_root = config.native_root.join(&config.mount_subtree);
    let filter = mirage_etw::filter::RootFilter::new(&profile_root).map_err(MirageError::from)?;
    let first_timestamp = result
        .events
        .iter()
        .filter(|event| !event.write)
        .map(|event| event.timestamp_100ns)
        .min()
        .unwrap_or(0);
    let mut matching_process_reads = 0_u64;
    let mut named_process_reads = 0_u64;
    let mut in_root_reads = 0_u64;
    for event in result
        .events
        .iter()
        .filter(|event| event.process_id == result.process_id && !event.write && event.size != 0)
    {
        matching_process_reads = matching_process_reads.saturating_add(1);
        if event.path.is_some() {
            named_process_reads = named_process_reads.saturating_add(1);
        }
        let Some(path) = event.path.as_deref().and_then(|path| filter.include(path)) else {
            continue;
        };
        in_root_reads = in_root_reads.saturating_add(1);
        let path = path.to_string_lossy().replace('/', "\\");
        let Some(NodeIndex::File(file_index)) = index.lookup_path(&path)? else {
            continue;
        };
        let file = index.file_by_index(file_index)?;
        trace_events.push(TraceEvent {
            timestamp_ns: event
                .timestamp_100ns
                .saturating_sub(first_timestamp)
                .saturating_mul(100),
            stable_file_id: file.stable_id(),
            offset: event.offset,
            length: event.size,
            flags: 0,
        });
    }
    trace_events.sort_by_key(|event| event.timestamp_ns);
    if trace_events.is_empty() {
        return Err(MirageError::provider_unavailable(format!(
            "ETW captured no manifest-backed reads: total_events={}, matching_process_reads={matching_process_reads}, named_process_reads={named_process_reads}, in_root_reads={in_root_reads}, unknown_paths={}, dropped_events={}, events_lost={}, buffers_lost={}, timed_out={}, exit_code={:?}",
            result.events.len(),
            result.unknown_path_events,
            result.dropped_capture_events,
            result.etw_events_lost,
            result.etw_buffers_lost,
            result.timed_out,
            result.exit_code,
        )));
    }
    let dropped = u64::from(result.etw_events_lost)
        .saturating_add(u64::from(result.etw_buffers_lost))
        .saturating_add(result.dropped_capture_events);
    let normalized = normalize_trace(&index, &trace_events, None)?;
    if normalized.touches.is_empty() {
        return Err(MirageError::provider_unavailable(
            "captured file I/O did not resolve to any virtual manifest page",
        ));
    }
    let file_ordinals = file_ordinals(&index)?;
    let observations = normalized
        .touches
        .iter()
        .map(|touch| {
            let file_index = *file_ordinals.get(&touch.stable_file_id).ok_or_else(|| {
                MirageError::internal_invariant("normalized trace file disappeared")
            })?;
            let page_ordinal = relative_page_ordinal(
                index.file_by_index(file_index)?,
                touch.page_ordinal.as_u32(),
            )?;
            Ok(PageObservation {
                file_index,
                page_ordinal,
                first_touch_delta_us: touch.timestamp_ns / 1_000,
                class: ObservationClass::Demand,
            })
        })
        .collect::<Result<Vec<_>, MirageError>>()?;
    let label = format!("{}:{}", config.version_label, config.configuration_label);
    let profile = GameProfile {
        format_version: 1,
        repository_id,
        manifest_hash: active.manifest_hash,
        label,
        page_observations: observations,
        processes: vec![ProcessRecord {
            stable_index: result.process_id,
            role: ProfileProcessRole::Game,
            redacted_image_path: config
                .launcher_relative
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("launcher")
                .to_owned(),
        }],
        dropped_event_count: dropped,
    };
    profile.validate()?;
    let stamp = now_ns();
    let profile_root = repository_state_root(database, repository_id)?.join("profiles");
    std::fs::create_dir_all(&profile_root).map_err(MirageError::from)?;
    let base = format!("{stamp}");
    write_json_atomic(&profile_root.join(format!("{base}.profile.json")), &profile)?;
    let trace_path = profile_root.join(format!("{base}.trace"));
    write_trace(
        &trace_path,
        repository_id,
        active.generation_id,
        u32::try_from(index.header().page_size)
            .map_err(|_| MirageError::unsupported_layout("page size exceeds u32"))?,
        dropped,
        &trace_events,
    )?;
    let summary = profile.summary()?;
    Ok(json!({
        "repository_id": repository_id.to_string(),
        "profile_id": base,
        "process_id": result.process_id,
        "exit_code": result.exit_code,
        "timed_out": result.timed_out,
        "elapsed_ms": result.elapsed.as_millis(),
        "observations": summary.observation_count,
        "unique_pages": summary.unique_page_count,
        "dropped_events": dropped,
        "unknown_path_events": result.unknown_path_events,
        "trace_path": trace_path
    }))
}

pub fn simulate(database: &Database, repository_id: RepositoryId) -> Result<Value, MirageError> {
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
    let latest = latest_trace_path(database, repository_id)?;
    let trace = read_trace(&latest, repository_id, active.generation_id)?;
    let normalized = normalize_trace(&index, &trace, None)?;
    let page_size = index.header().page_size;
    let cache_pages = usize::try_from((config.cache_bytes / page_size).max(1))
        .unwrap_or(usize::MAX)
        .min(
            usize::try_from(index.page_count())
                .unwrap_or(usize::MAX)
                .max(1),
        );
    let mut replay = BaselineReplay::new(ReplayConfig {
        cache_pages,
        page_bytes: page_size,
        pinned_pages: BTreeSet::new(),
        network: NetworkModel {
            base_latency_ns: ASSUMED_ORIGIN_TTFB_NS,
            jitter_ns: 5_000_000,
            jitter_seed: 0x4d49_5241_4745,
            bandwidth_bytes_per_second: ASSUMED_ORIGIN_GOODPUT_BYTES_PER_SECOND,
            max_concurrency: 1,
            fail_fetches: BTreeSet::new(),
        },
    })?;
    for touch in normalized.touches {
        replay.process(touch)?;
    }
    let metrics = replay.finish();
    let report = mirage_predictor::analyze::analyze(
        &profiles,
        page_size,
        PROFILE_STARTUP_WINDOW_US,
        index.page_count(),
    )?;
    let value = json!({
        "repository_id": repository_id.to_string(),
        "profile_sessions": report.session_count,
        "unique_pages": report.unique_pages,
        "union_bytes": report.union_bytes,
        "intersection_pages": report.intersection_pages,
        "startup_pages": report.startup_pages,
        "broad_scan_detected": report.broad_scan_detected,
        "dropped_events": report.dropped_events,
        "simulation": {
            "cache_pages": cache_pages,
            "network_mbps": 100,
            "latency_ms": 50,
            "accesses": metrics.accesses,
            "hits": metrics.hits,
            "misses": metrics.misses,
            "blocking_ns": metrics.blocking_ns,
            "p95_stall_ns": metrics.p95_stall_ns,
            "p99_stall_ns": metrics.p99_stall_ns,
            "remote_bytes": metrics.remote_bytes,
            "peak_resident_pages": metrics.peak_resident_pages
        }
    });
    write_json_atomic(
        &repository_state_root(database, repository_id)?.join("simulation.json"),
        &value,
    )?;
    Ok(value)
}

pub(super) fn load_profiles(
    database: &Database,
    repository_id: RepositoryId,
    binding: &ProfileBinding,
) -> Result<Vec<GameProfile>, MirageError> {
    let root = repository_state_root(database, repository_id)?.join("profiles");
    let mut paths = std::fs::read_dir(&root)
        .map_err(MirageError::from)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".profile.json"))
        })
        .collect::<Vec<_>>();
    paths.sort();
    if paths.is_empty() {
        return Err(MirageError::invalid_argument(
            "repository has no captured profiles",
        ));
    }
    paths
        .into_iter()
        .map(|path| {
            let bytes = bounded_read(&path, 256 * 1024 * 1024)?;
            let profile: GameProfile = serde_json::from_slice(&bytes).map_err(|error| {
                MirageError::integrity_mismatch("stored game profile is malformed")
                    .with_source(error)
            })?;
            profile.validate()?;
            bind_profile(&profile, binding)?;
            Ok(profile)
        })
        .collect()
}

fn latest_trace_path(
    database: &Database,
    repository_id: RepositoryId,
) -> Result<PathBuf, MirageError> {
    let root = repository_state_root(database, repository_id)?.join("profiles");
    let mut paths = std::fs::read_dir(root)
        .map_err(MirageError::from)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("trace"))
        .collect::<Vec<_>>();
    paths.sort();
    paths
        .pop()
        .ok_or_else(|| MirageError::invalid_argument("repository has no captured trace"))
}

fn write_trace(
    path: &Path,
    repository_id: RepositoryId,
    generation: GenerationId,
    page_size: u32,
    dropped: u64,
    events: &[TraceEvent],
) -> Result<(), MirageError> {
    let mut bytes = Vec::new();
    let mut encoder = TraceBlockEncoder::new(
        &mut bytes,
        &TraceHeader {
            schema_version: 1,
            repository_id,
            manifest_generation: generation,
            page_size,
            machine_profile: std::env::var("COMPUTERNAME").unwrap_or_else(|_| "windows".into()),
            dropped_event_count: dropped,
        },
    )?;
    for block in events.chunks(65_536) {
        encoder.write_block(block)?;
    }
    let _ = encoder.finish();
    write_atomic(path, &bytes)
}

fn read_trace(
    path: &Path,
    repository_id: RepositoryId,
    generation: GenerationId,
) -> Result<Vec<TraceEvent>, MirageError> {
    let file = File::open(path).map_err(MirageError::from)?;
    let mut decoder = TraceBlockDecoder::new(file)?;
    if decoder.header.repository_id != repository_id
        || decoder.header.manifest_generation != generation
    {
        return Err(MirageError::repository_conflict(
            "stored trace targets a different repository generation",
        ));
    }
    let mut events = Vec::new();
    while let Some(block) = decoder.read_block()? {
        events.extend(block);
    }
    Ok(events)
}

pub(super) fn held_out_violation_millionths(profiles: &[GameProfile]) -> u32 {
    if profiles.len() < 2 {
        return 1_000_000;
    }
    profiles
        .iter()
        .enumerate()
        .map(|(held_out, profile)| {
            let expected = profile
                .page_observations
                .iter()
                .map(|page| (page.file_index, page.page_ordinal))
                .collect::<BTreeSet<_>>();
            let trained = profiles
                .iter()
                .enumerate()
                .filter(|(index, _)| *index != held_out)
                .flat_map(|(_, profile)| {
                    profile
                        .page_observations
                        .iter()
                        .map(|page| (page.file_index, page.page_ordinal))
                })
                .collect::<BTreeSet<_>>();
            let missing = expected.difference(&trained).count() as u64;
            millionths(missing, expected.len() as u64)
        })
        .max()
        .unwrap_or(1_000_000)
}

pub(super) fn millionths(numerator: u64, denominator: u64) -> u32 {
    if denominator == 0 {
        return 0;
    }
    u32::try_from(
        (u128::from(numerator).saturating_mul(1_000_000) / u128::from(denominator)).min(1_000_000),
    )
    .unwrap_or(1_000_000)
}
