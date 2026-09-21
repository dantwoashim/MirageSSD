use mirage_types::{
    FetchFailureCause, GenerationId, ManifestHash, MirageError, PresentationBackend,
    QualificationVersions, READINESS_SCHEMA_VERSION, ReadinessConstraint, ReadinessMode,
    ReadinessRecord, RepositoryId, ScopeCompleteness, SpatialEnvelope, TemporalEstimate,
};

fn envelope() -> SpatialEnvelope {
    SpatialEnvelope {
        allocated: 100,
        reserved_new_allocation: 50,
        dirty_staging: 25,
        rollback_retention: 10,
        journal_and_metadata: 10,
        filesystem_slack: 5,
    }
}

fn estimate() -> TemporalEstimate {
    TemporalEstimate {
        queue_delay_ns: 0,
        source_ttfb_ns: 40_000_000,
        encoded_bytes: 1_048_576,
        goodput_bytes_per_second: 6_250_000,
        decode_ns: 0,
        verify_ns: 0,
        placement_ns: 0,
        safety_margin_ns: 0,
    }
}

fn record() -> ReadinessRecord {
    ReadinessRecord {
        schema_version: READINESS_SCHEMA_VERSION,
        repository_id: RepositoryId::from_bytes([1; 16]),
        generation: GenerationId::from_u64(3),
        manifest_hash: ManifestHash::from_bytes([2; 32]),
        configuration_label: "1.0:en".into(),
        profile_schema_version: 1,
        scope_id: "campaign-act-1".into(),
        scope_completeness: ScopeCompleteness::Complete,
        mode: ReadinessMode::VerifiedScope,
        required_units: 16,
        required_bytes: 1_048_576,
        native_file_count: 4,
        presentation: PresentationBackend::NativeFiles,
        spatial: envelope(),
        budget_bytes: 200,
        temporal_lead_ns: 500_000_000,
        ram_credit_bytes: 65_536,
        max_in_flight: 8,
        qualification: QualificationVersions {
            backend: "stub".into(),
            os_build: "26100".into(),
            driver: "winfsp-2.1".into(),
            runtime: "1".into(),
        },
        pin_generation: 3,
        invalidation_conditions: vec!["manifest hash change".into()],
    }
}

#[test]
fn wire_ids_round_trip_through_serde() {
    for mode in ReadinessMode::ALL {
        let json = serde_json::to_string(mode).unwrap();
        assert_eq!(json, format!("\"{}\"", mode.as_str()));
        assert_eq!(serde_json::from_str::<ReadinessMode>(&json).unwrap(), *mode);
    }
    for completeness in ScopeCompleteness::ALL {
        let json = serde_json::to_string(completeness).unwrap();
        assert_eq!(json, format!("\"{}\"", completeness.as_str()));
        assert_eq!(
            serde_json::from_str::<ScopeCompleteness>(&json).unwrap(),
            *completeness
        );
    }
    for backend in PresentationBackend::ALL {
        let json = serde_json::to_string(backend).unwrap();
        assert_eq!(json, format!("\"{}\"", backend.as_str()));
        assert_eq!(
            serde_json::from_str::<PresentationBackend>(&json).unwrap(),
            *backend
        );
    }
    for constraint in ReadinessConstraint::ALL {
        let json = serde_json::to_string(constraint).unwrap();
        assert_eq!(json, format!("\"{}\"", constraint.as_str()));
        assert_eq!(
            serde_json::from_str::<ReadinessConstraint>(&json).unwrap(),
            *constraint
        );
    }
    for cause in FetchFailureCause::ALL {
        let json = serde_json::to_string(cause).unwrap();
        assert_eq!(json, format!("\"{}\"", cause.as_str()));
        assert_eq!(
            serde_json::from_str::<FetchFailureCause>(&json).unwrap(),
            *cause
        );
    }
}

#[test]
fn spatial_envelope_admits_exactly_at_equality() {
    let envelope = envelope();
    assert_eq!(envelope.total().unwrap(), 200);
    assert!(envelope.admits(200).unwrap());
    assert!(!envelope.admits(199).unwrap());
    let reserved = envelope.with_additional_reservation(10).unwrap();
    assert_eq!(reserved.reserved_new_allocation, 60);
    assert_eq!(reserved.total().unwrap(), 210);
}

#[test]
fn spatial_envelope_overflow_is_an_error() {
    let overflowing = SpatialEnvelope {
        allocated: u64::MAX,
        reserved_new_allocation: 1,
        dirty_staging: 0,
        rollback_retention: 0,
        journal_and_metadata: 0,
        filesystem_slack: 0,
    };
    assert!(overflowing.total().is_err());
    assert!(overflowing.admits(u64::MAX).is_err());
    assert!(envelope().with_additional_reservation(u64::MAX).is_err());
}

#[test]
fn temporal_estimate_matches_published_bounds() {
    let mib = estimate();
    assert_eq!(mib.transfer_ns().unwrap(), 167_772_160);
    assert_eq!(mib.verified_available_ns(0).unwrap(), 207_772_160);
    let kib = TemporalEstimate {
        encoded_bytes: 65_536,
        ..mib
    };
    assert_eq!(kib.verified_available_ns(0).unwrap(), 50_485_760);
    assert!(mib.admits(0, 207_772_160).unwrap());
    assert!(!mib.admits(0, 207_772_159).unwrap());
    let zero_goodput = TemporalEstimate {
        goodput_bytes_per_second: 0,
        ..mib
    };
    assert!(zero_goodput.transfer_ns().is_err());
}

#[test]
fn fetch_failure_cause_maps_every_listed_kind() {
    let cases: [(MirageError, FetchFailureCause); 15] = [
        (
            MirageError::integrity_mismatch("x"),
            FetchFailureCause::ChecksumMismatch,
        ),
        (
            MirageError::manifest_invalid("x"),
            FetchFailureCause::ChecksumMismatch,
        ),
        (
            MirageError::unsupported_layout("x"),
            FetchFailureCause::ChecksumMismatch,
        ),
        (
            MirageError::backend_unauthenticated("x"),
            FetchFailureCause::AuthorizationDenied,
        ),
        (
            MirageError::backend_permission_denied("x"),
            FetchFailureCause::AuthorizationDenied,
        ),
        (
            MirageError::remote_object_missing("x"),
            FetchFailureCause::SourceMissing,
        ),
        (
            MirageError::provider_unavailable("x"),
            FetchFailureCause::SourceMissing,
        ),
        (
            MirageError::backend_unavailable("x"),
            FetchFailureCause::Timeout,
        ),
        (
            MirageError::backend_rate_limited("x", None),
            FetchFailureCause::Timeout,
        ),
        (
            MirageError::deadline_exceeded("x"),
            FetchFailureCause::DeadlineExceeded,
        ),
        (
            MirageError::cache_full("x"),
            FetchFailureCause::BudgetExceeded,
        ),
        (
            MirageError::cancelled("x"),
            FetchFailureCause::CallerCancelled,
        ),
        (
            MirageError::invalid_argument("x"),
            FetchFailureCause::MalformedResponse,
        ),
        (
            MirageError::repository_conflict("x"),
            FetchFailureCause::Internal,
        ),
        (
            MirageError::internal_invariant("x"),
            FetchFailureCause::Internal,
        ),
    ];
    for (error, expected) in cases {
        assert_eq!(FetchFailureCause::from_error(&error), expected);
    }
}

#[test]
fn readiness_record_validate_enforces_mode_and_budget() {
    record().validate().unwrap();

    let unsupported = ReadinessRecord {
        mode: ReadinessMode::Unsupported,
        ..record()
    };
    assert!(unsupported.validate().is_err());

    let empirical_verified = ReadinessRecord {
        scope_completeness: ScopeCompleteness::Empirical,
        ..record()
    };
    assert!(empirical_verified.validate().is_err());

    let over_budget = ReadinessRecord {
        budget_bytes: 199,
        ..record()
    };
    assert!(over_budget.validate().is_err());

    let adaptive = ReadinessRecord {
        mode: ReadinessMode::ProfiledAdaptive,
        scope_completeness: ScopeCompleteness::Empirical,
        ..record()
    };
    adaptive.validate().unwrap();

    let no_in_flight = ReadinessRecord {
        max_in_flight: 0,
        ..record()
    };
    assert!(no_in_flight.validate().is_err());
}
