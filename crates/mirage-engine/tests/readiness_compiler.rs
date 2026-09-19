use mirage_engine::{
    BackendCapability, CompileInput, EvictionGranularity, FilePlacementClass, HydrationGranularity,
    OriginEstimate, ReadinessIdentity, ReadinessVerdict, RequiredFile, ScopeSpec,
    compile_readiness,
};
use mirage_types::{
    GenerationId, ManifestHash, MirageErrorKind, PresentationBackend, QualificationVersions,
    ReadinessConstraint, ReadinessMode, RepositoryId, ScopeCompleteness, SpatialEnvelope,
};

const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * MIB;

fn identity() -> ReadinessIdentity {
    ReadinessIdentity {
        repository_id: RepositoryId::from_bytes([1; 16]),
        generation: GenerationId::ZERO,
        manifest_hash: ManifestHash::from_bytes([2; 32]),
        configuration_label: "1.0:en".into(),
        profile_schema_version: 1,
    }
}

fn qualification() -> QualificationVersions {
    QualificationVersions {
        backend: "winfsp-2.0".into(),
        os_build: "26100.1".into(),
        driver: "test".into(),
        runtime: "0.1.0".into(),
    }
}

fn file(
    file_index: u32,
    logical_size: u64,
    class: FilePlacementClass,
    required_units: u64,
    required_bytes: u64,
    resident_bytes: u64,
    max_unit_bytes: u64,
) -> RequiredFile {
    RequiredFile {
        file_index,
        logical_size,
        class,
        required_units,
        required_bytes,
        resident_bytes,
        max_unit_bytes,
    }
}

fn virtual_file(
    file_index: u32,
    logical_size: u64,
    required_units: u64,
    required_bytes: u64,
    resident_bytes: u64,
) -> RequiredFile {
    file(
        file_index,
        logical_size,
        FilePlacementClass::Virtual,
        required_units,
        required_bytes,
        resident_bytes,
        MIB,
    )
}

fn capability(
    backend: PresentationBackend,
    qualified: bool,
    hydration: HydrationGranularity,
    eviction: EvictionGranularity,
    whole_file_pin_only: bool,
    metadata_bytes_per_unit: u64,
) -> BackendCapability {
    BackendCapability {
        backend,
        qualified,
        hydration,
        eviction,
        whole_file_pin_only,
        metadata_bytes_per_unit,
    }
}

fn winfsp() -> BackendCapability {
    capability(
        PresentationBackend::WinFspProjection,
        true,
        HydrationGranularity::Page,
        EvictionGranularity::Page,
        false,
        0,
    )
}

fn empty_envelope() -> SpatialEnvelope {
    SpatialEnvelope {
        allocated: 0,
        reserved_new_allocation: 0,
        dirty_staging: 0,
        rollback_retention: 0,
        journal_and_metadata: 0,
        filesystem_slack: 0,
    }
}

fn origin(available: bool) -> OriginEstimate {
    OriginEstimate {
        available,
        queue_delay_ns: 0,
        source_ttfb_ns: 40_000_000,
        goodput_bytes_per_second: 6_250_000,
        decode_ns_per_unit: 0,
        verify_ns_per_unit: 0,
        placement_ns_per_unit: 0,
        safety_margin_ns: 0,
    }
}

fn input(
    completeness: ScopeCompleteness,
    files: Vec<RequiredFile>,
    lead_time_ns: Option<u64>,
    budget_bytes: u64,
    current: SpatialEnvelope,
    candidates: Vec<BackendCapability>,
    origin: OriginEstimate,
) -> CompileInput {
    CompileInput {
        identity: identity(),
        scope: ScopeSpec {
            scope_id: "boot-chapter-1".into(),
            completeness,
            files,
            lead_time_ns,
        },
        budget_bytes,
        current,
        candidates,
        origin,
        qualification: qualification(),
        pin_generation: 7,
        ram_credit_bytes: 0,
        max_in_flight: 4,
    }
}

fn ready(verdict: ReadinessVerdict) -> mirage_types::ReadinessRecord {
    match verdict {
        ReadinessVerdict::Ready { record, .. } => *record,
        other => panic!("expected Ready, got {other:?}"),
    }
}

#[test]
fn complete_scope_on_qualified_page_backend_is_verified_scope() {
    let mut current = empty_envelope();
    current.allocated = MIB; // the resident MiB is already allocated
    let verdict = compile_readiness(&input(
        ScopeCompleteness::Complete,
        vec![
            virtual_file(0, 4 * MIB, 2, 2 * MIB, MIB),
            virtual_file(1, 8 * MIB, 3, 3 * MIB, 0),
        ],
        None,
        64 * MIB,
        current,
        vec![capability(
            PresentationBackend::WinFspProjection,
            true,
            HydrationGranularity::Page,
            EvictionGranularity::Page,
            false,
            1024,
        )],
        origin(true),
    ))
    .expect("compile");
    let ReadinessVerdict::Ready { record, rejections } = verdict else {
        panic!("expected Ready, got {verdict:?}")
    };
    assert!(rejections.is_empty());
    assert_eq!(record.mode, ReadinessMode::VerifiedScope);
    assert_eq!(record.presentation, PresentationBackend::WinFspProjection);
    assert_eq!(record.spatial.reserved_new_allocation, 4 * MIB);
    assert_eq!(record.spatial.journal_and_metadata, 5 * 1024);
    assert_eq!(record.required_bytes, 5 * MIB);
    assert_eq!(record.required_units, 5);
    assert_eq!(record.native_file_count, 0);
    assert_eq!(record.invalidation_conditions.len(), 5);
}

#[test]
fn whole_file_pin_only_closure_priced_honestly_falls_back_to_page_backend() {
    let files = (0..4)
        .map(|index| virtual_file(index, 10 * GIB, 1, MIB, 0))
        .collect();
    let verdict = compile_readiness(&input(
        ScopeCompleteness::Complete,
        files,
        None,
        GIB,
        empty_envelope(),
        vec![
            capability(
                PresentationBackend::CloudFiles,
                true,
                HydrationGranularity::ProgressiveFile,
                EvictionGranularity::WholeFile,
                true,
                0,
            ),
            winfsp(),
        ],
        origin(true),
    ))
    .expect("compile");
    let ReadinessVerdict::Ready { record, rejections } = verdict else {
        panic!("expected Ready, got {verdict:?}")
    };
    assert_eq!(record.presentation, PresentationBackend::WinFspProjection);
    assert_eq!(record.mode, ReadinessMode::VerifiedScope);
    assert_eq!(rejections.len(), 1);
    assert_eq!(rejections[0].backend, PresentationBackend::CloudFiles);
    assert_eq!(rejections[0].constraint, ReadinessConstraint::Spatial);
}

#[test]
fn unqualified_candidate_only_is_unsupported_compatibility() {
    let verdict = compile_readiness(&input(
        ScopeCompleteness::Complete,
        vec![virtual_file(0, 4 * MIB, 4, 4 * MIB, 0)],
        None,
        64 * MIB,
        empty_envelope(),
        vec![capability(
            PresentationBackend::CloudFiles,
            false,
            HydrationGranularity::ProgressiveFile,
            EvictionGranularity::WholeFile,
            true,
            0,
        )],
        origin(true),
    ))
    .expect("compile");
    let ReadinessVerdict::Unsupported(plan) = verdict else {
        panic!("expected Unsupported, got {verdict:?}")
    };
    assert_eq!(plan.failed, vec![ReadinessConstraint::Compatibility]);
    assert_eq!(plan.rejections.len(), 1);
    assert_eq!(plan.rejections[0].backend, PresentationBackend::CloudFiles);
}

#[test]
fn empirical_scope_requires_a_temporal_admission_window() {
    let files = vec![virtual_file(0, 4 * MIB, 1, MIB, MIB)];
    let verdict = compile_readiness(&input(
        ScopeCompleteness::Empirical,
        files.clone(),
        Some(16_700_000),
        64 * MIB,
        empty_envelope(),
        vec![winfsp()],
        origin(true),
    ))
    .expect("compile");
    let ReadinessVerdict::Unsupported(plan) = verdict else {
        panic!("expected Unsupported, got {verdict:?}")
    };
    assert_eq!(plan.failed, vec![ReadinessConstraint::Temporal]);
    assert!(plan.rejections[0].detail.contains("207772160"));

    let record = ready(
        compile_readiness(&input(
            ScopeCompleteness::Empirical,
            files,
            Some(300_000_000),
            64 * MIB,
            empty_envelope(),
            vec![winfsp()],
            origin(true),
        ))
        .expect("compile"),
    );
    assert_eq!(record.mode, ReadinessMode::ProfiledAdaptive);
    assert_eq!(record.invalidation_conditions.len(), 6);
    assert!(
        record
            .invalidation_conditions
            .iter()
            .any(|condition| condition == "origin_unavailable")
    );
}

#[test]
fn source_and_lead_time_are_separate_constraint_failures() {
    let resident = vec![virtual_file(0, 4 * MIB, 1, MIB, MIB)];
    let missing = vec![virtual_file(0, 4 * MIB, 2, 2 * MIB, MIB)];

    // Empirical scope without a declared lead time cannot be temporally admitted.
    let verdict = compile_readiness(&input(
        ScopeCompleteness::Empirical,
        resident.clone(),
        None,
        64 * MIB,
        empty_envelope(),
        vec![winfsp()],
        origin(true),
    ))
    .expect("compile");
    let ReadinessVerdict::Unsupported(plan) = verdict else {
        panic!("expected Unsupported, got {verdict:?}")
    };
    assert_eq!(plan.failed, vec![ReadinessConstraint::Temporal]);

    // Adaptive play always needs an origin, even when observed bytes are resident.
    let verdict = compile_readiness(&input(
        ScopeCompleteness::Empirical,
        resident.clone(),
        Some(300_000_000),
        64 * MIB,
        empty_envelope(),
        vec![winfsp()],
        origin(false),
    ))
    .expect("compile");
    let ReadinessVerdict::Unsupported(plan) = verdict else {
        panic!("expected Unsupported, got {verdict:?}")
    };
    assert_eq!(plan.failed, vec![ReadinessConstraint::Source]);

    // A complete scope with missing bytes and no origin is a Source failure.
    let verdict = compile_readiness(&input(
        ScopeCompleteness::Complete,
        missing,
        None,
        64 * MIB,
        empty_envelope(),
        vec![winfsp()],
        origin(false),
    ))
    .expect("compile");
    let ReadinessVerdict::Unsupported(plan) = verdict else {
        panic!("expected Unsupported, got {verdict:?}")
    };
    assert_eq!(plan.failed, vec![ReadinessConstraint::Source]);

    // A complete scope that is fully resident keeps its offline promise.
    let record = ready(
        compile_readiness(&input(
            ScopeCompleteness::Complete,
            resident,
            None,
            64 * MIB,
            empty_envelope(),
            vec![winfsp()],
            origin(false),
        ))
        .expect("compile"),
    );
    assert_eq!(record.mode, ReadinessMode::VerifiedScope);
}

#[test]
fn native_files_presentation_prices_every_virtual_file_fully() {
    let native = capability(
        PresentationBackend::NativeFiles,
        true,
        HydrationGranularity::WholeFile,
        EvictionGranularity::None,
        false,
        0,
    );
    let files = vec![
        virtual_file(0, 4 * MIB, 4, MIB, 0),
        virtual_file(1, 8 * MIB, 8, MIB, 0),
    ];
    let record = ready(
        compile_readiness(&input(
            ScopeCompleteness::Complete,
            files.clone(),
            None,
            64 * MIB,
            empty_envelope(),
            vec![native.clone()],
            origin(true),
        ))
        .expect("compile"),
    );
    assert_eq!(record.mode, ReadinessMode::FullLocal);
    assert_eq!(record.presentation, PresentationBackend::NativeFiles);
    assert_eq!(record.required_bytes, 12 * MIB);

    let verdict = compile_readiness(&input(
        ScopeCompleteness::Complete,
        files,
        None,
        8 * MIB,
        empty_envelope(),
        vec![native],
        origin(false),
    ))
    .expect("compile");
    let ReadinessVerdict::Unsupported(plan) = verdict else {
        panic!("expected Unsupported, got {verdict:?}")
    };
    assert_eq!(plan.failed, vec![ReadinessConstraint::Spatial]);
    assert!(plan.rejections[0].detail.contains("exceeds budget"));
}

#[test]
fn resident_bytes_are_never_double_counted_in_the_envelope() {
    let mut current = empty_envelope();
    current.allocated = 3 * MIB;
    current.reserved_new_allocation = 2 * MIB;
    let record = ready(
        compile_readiness(&input(
            ScopeCompleteness::Complete,
            vec![virtual_file(0, 8 * MIB, 4, 4 * MIB, 3 * MIB)],
            None,
            64 * MIB,
            current,
            vec![winfsp()],
            origin(true),
        ))
        .expect("compile"),
    );
    assert_eq!(record.spatial.allocated, current.allocated);
    assert_eq!(
        record.spatial.reserved_new_allocation,
        current.reserved_new_allocation + MIB
    );
}

#[test]
fn invalid_inputs_fail_closed() {
    let error = compile_readiness(&input(
        ScopeCompleteness::Complete,
        vec![virtual_file(0, 4 * MIB, 1, MIB, 2 * MIB)],
        None,
        64 * MIB,
        empty_envelope(),
        vec![winfsp()],
        origin(true),
    ))
    .expect_err("resident above required must fail");
    assert_eq!(error.kind, MirageErrorKind::InvalidArgument);

    let error = compile_readiness(&input(
        ScopeCompleteness::Complete,
        vec![virtual_file(0, 4 * MIB, 1, MIB, 0)],
        None,
        64 * MIB,
        empty_envelope(),
        Vec::new(),
        origin(true),
    ))
    .expect_err("empty candidates must fail");
    assert_eq!(error.kind, MirageErrorKind::InvalidArgument);
}
