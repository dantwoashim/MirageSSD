use mirage_service::{LaunchMode, LaunchPolicy, LaunchReadiness};
use mirage_types::{CapsuleId, GenerationId};
fn ready() -> LaunchReadiness {
    let capsule = CapsuleId::from_bytes([1; 16]);
    LaunchReadiness {
        mounted_generation: GenerationId(2),
        requested_generation: GenerationId(2),
        admitted_capsule: Some(capsule),
        requested_capsule: Some(capsule),
        capsule_complete: true,
        hard_set_complete: true,
        update_or_recovery_active: false,
        provider_healthy: true,
        risk_millionths: 10,
    }
}
#[test]
fn mode_preconditions_fail_closed() {
    assert!(LaunchPolicy::validate(LaunchMode::Sealed, &ready()).is_ok());
    let mut r = ready();
    r.requested_generation = GenerationId(3);
    assert!(LaunchPolicy::validate(LaunchMode::Sealed, &r).is_err());
    let mut r = ready();
    r.capsule_complete = false;
    assert!(LaunchPolicy::validate(LaunchMode::Sealed, &r).is_err());
    let mut r = ready();
    r.provider_healthy = false;
    assert!(LaunchPolicy::validate(LaunchMode::Balanced, &r).is_err());
    let mut r = ready();
    r.update_or_recovery_active = true;
    assert!(LaunchPolicy::validate(LaunchMode::Sealed, &r).is_err());
}
