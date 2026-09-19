use mirage_predictor::binding::{
    BindingRejection, PROFILE_FORMAT_VERSION, ProfileBinding, bind_profile, check_binding,
};
use mirage_predictor::{GameProfile, ObservationClass, PageObservation};
use mirage_types::{ManifestHash, MirageErrorKind, RepositoryId};

fn profile() -> GameProfile {
    GameProfile {
        format_version: PROFILE_FORMAT_VERSION,
        repository_id: RepositoryId::from_bytes([1; 16]),
        manifest_hash: ManifestHash::from_bytes([2; 32]),
        label: "1.0:en".into(),
        page_observations: vec![PageObservation {
            file_index: 0,
            page_ordinal: 0,
            first_touch_delta_us: 0,
            class: ObservationClass::Demand,
        }],
        processes: vec![],
        dropped_event_count: 0,
    }
}

fn binding() -> ProfileBinding {
    let profile = profile();
    ProfileBinding {
        repository_id: profile.repository_id,
        manifest_hash: profile.manifest_hash,
        format_version: PROFILE_FORMAT_VERSION,
        label: profile.label,
    }
}

#[test]
fn matching_profile_binds() {
    let profile = profile();
    assert_eq!(check_binding(&profile, &binding()), Ok(()));
    bind_profile(&profile, &binding()).unwrap();
}

#[test]
fn unknown_schema_is_rejected_as_unsupported_layout() {
    let mut profile = profile();
    profile.format_version = 2;
    assert_eq!(
        check_binding(&profile, &binding()),
        Err(BindingRejection::SchemaMismatch)
    );
    let error = bind_profile(&profile, &binding()).unwrap_err();
    assert_eq!(error.kind, MirageErrorKind::UnsupportedLayout);
    assert!(error.message.contains("schema_mismatch"));
}

#[test]
fn wrong_repository_is_rejected() {
    let mut profile = profile();
    profile.repository_id = RepositoryId::from_bytes([9; 16]);
    assert_eq!(
        check_binding(&profile, &binding()),
        Err(BindingRejection::WrongRepository)
    );
}

#[test]
fn changed_content_at_same_path_is_wrong_manifest() {
    let mut profile = profile();
    profile.manifest_hash = ManifestHash::from_bytes([3; 32]);
    assert_eq!(
        check_binding(&profile, &binding()),
        Err(BindingRejection::WrongManifest)
    );
    let error = bind_profile(&profile, &binding()).unwrap_err();
    assert_eq!(error.kind, MirageErrorKind::RepositoryConflict);
    assert!(error.message.contains("wrong_manifest"));
}

#[test]
fn changed_language_is_configuration_mismatch() {
    let mut profile = profile();
    profile.label = "1.0:ja".into();
    assert_eq!(
        check_binding(&profile, &binding()),
        Err(BindingRejection::ConfigurationMismatch)
    );
}

#[test]
fn schema_mismatch_is_reported_first() {
    let mut profile = profile();
    profile.format_version = 2;
    profile.repository_id = RepositoryId::from_bytes([9; 16]);
    profile.manifest_hash = ManifestHash::from_bytes([9; 32]);
    profile.label = "9.9:zz".into();
    assert_eq!(
        check_binding(&profile, &binding()),
        Err(BindingRejection::SchemaMismatch)
    );
}
