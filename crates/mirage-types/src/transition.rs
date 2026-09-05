//! Centralized, side-effect-free lifecycle transitions.

use core::fmt;

use crate::state::{BackendHealthState, PageState, RepositoryState, SessionState, UpdateState};

macro_rules! event_enum {
    ($name:ident { $($variant:ident => $wire:literal),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum $name { $($variant),+ }

        impl $name {
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];

            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $wire),+ }
            }
        }
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StateMachine {
    Repository,
    Page,
    Session,
    Update,
    BackendHealth,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransitionError {
    pub machine: StateMachine,
    pub from: &'static str,
    pub event: &'static str,
}

impl fmt::Display for TransitionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid {:?} transition: {} + {}",
            self.machine, self.from, self.event
        )
    }
}

impl std::error::Error for TransitionError {}

event_enum! {
    RepositoryEvent {
        StartImport => "start_import",
        BaseImported => "base_imported",
        BaseUploaded => "base_uploaded",
        BaseVerified => "base_verified",
        MountRequested => "mount_requested",
        MountSucceeded => "mount_succeeded",
        UnmountRequested => "unmount_requested",
        BeginAdmission => "begin_admission",
        CapsuleSealed => "capsule_sealed",
        BalancedLaunchRequested => "balanced_launch_requested",
        SessionEnded => "session_ended",
        BeginUpdate => "begin_update",
        UpdateCommitted => "update_committed",
        RecoveryRequested => "recovery_requested",
        RecoverySucceededUnmounted => "recovery_succeeded_unmounted",
        RecoverySucceededMounted => "recovery_succeeded_mounted",
        BackendDegraded => "backend_degraded",
        ConflictDetected => "conflict_detected",
        FatalError => "fatal_error",
    }
}

pub fn transition_repository(
    from: RepositoryState,
    event: RepositoryEvent,
) -> Result<RepositoryState, TransitionError> {
    use RepositoryEvent as E;
    use RepositoryState as S;
    let next = match (from, event) {
        (S::Uninitialized, E::StartImport) => S::Importing,
        (S::Importing, E::BaseImported) => S::UploadingBase,
        (S::UploadingBase, E::BaseUploaded) => S::VerifyingBase,
        (S::VerifyingBase, E::BaseVerified) => S::ReadyUnmounted,
        (S::ReadyUnmounted, E::MountRequested) => S::Mounting,
        (S::Mounting, E::MountSucceeded) => S::ReadyMounted,
        (S::ReadyMounted, E::UnmountRequested) => S::ReadyUnmounted,
        (S::ReadyMounted, E::BeginAdmission) => S::AdmittingSession,
        (S::AdmittingSession, E::CapsuleSealed) => S::PlayingSealed,
        (S::ReadyMounted, E::BalancedLaunchRequested) => S::PlayingBalanced,
        (S::PlayingSealed | S::PlayingBalanced, E::SessionEnded) => S::ReadyMounted,
        (S::ReadyUnmounted | S::ReadyMounted, E::BeginUpdate) => S::Updating,
        (S::Updating, E::UpdateCommitted) => S::ReadyUnmounted,
        (
            S::Importing
            | S::UploadingBase
            | S::VerifyingBase
            | S::ReadyUnmounted
            | S::Mounting
            | S::ReadyMounted
            | S::AdmittingSession
            | S::PlayingSealed
            | S::PlayingBalanced
            | S::Updating
            | S::Degraded
            | S::Conflicted
            | S::Error,
            E::RecoveryRequested,
        ) => S::Recovering,
        (S::Recovering, E::RecoverySucceededUnmounted) => S::ReadyUnmounted,
        (S::Recovering, E::RecoverySucceededMounted) => S::ReadyMounted,
        (
            S::ReadyUnmounted
            | S::ReadyMounted
            | S::AdmittingSession
            | S::PlayingSealed
            | S::PlayingBalanced,
            E::BackendDegraded,
        ) => S::Degraded,
        (
            S::Importing
            | S::UploadingBase
            | S::VerifyingBase
            | S::ReadyUnmounted
            | S::ReadyMounted
            | S::Updating
            | S::Recovering
            | S::Degraded,
            E::ConflictDetected,
        ) => S::Conflicted,
        (_, E::FatalError) => S::Error,
        _ => {
            return Err(invalid(
                StateMachine::Repository,
                from.as_str(),
                event.as_str(),
            ));
        }
    };
    Ok(next)
}

event_enum! {
    PageEvent {
        FetchRequested => "fetch_requested",
        FetchVerified => "fetch_verified",
        SessionPinAdded => "session_pin_added",
        SessionPinReleased => "session_pin_released",
        EvictionStarted => "eviction_started",
        EvictionCompleted => "eviction_completed",
        DirtyWrite => "dirty_write",
        StagingStarted => "staging_started",
        StageVerified => "stage_verified",
        CommitActivated => "commit_activated",
        GenerationPromoted => "generation_promoted",
        VerificationFailed => "verification_failed",
        QuarantineReleased => "quarantine_released",
    }
}

pub fn transition_page(from: PageState, event: PageEvent) -> Result<PageState, TransitionError> {
    use PageEvent as E;
    use PageState as S;
    let next = match (from, event) {
        (S::Absent, E::FetchRequested) => S::Fetching,
        (S::Fetching, E::FetchVerified) => S::ResidentClean,
        (S::ResidentClean, E::SessionPinAdded) => S::SessionPinned,
        (S::SessionPinned, E::SessionPinReleased) => S::ResidentClean,
        (S::ResidentClean, E::EvictionStarted) => S::Evicting,
        (S::Evicting, E::EvictionCompleted) => S::Absent,
        (S::ResidentClean, E::DirtyWrite) => S::DirtyLocal,
        (S::DirtyLocal, E::StagingStarted) => S::Staging,
        (S::Staging, E::StageVerified) => S::StagedRemote,
        (S::StagedRemote, E::CommitActivated) => S::CommittedRemote,
        (S::CommittedRemote, E::GenerationPromoted) => S::ResidentClean,
        (
            S::Fetching
            | S::ResidentClean
            | S::SessionPinned
            | S::Evicting
            | S::DirtyLocal
            | S::Staging
            | S::StagedRemote
            | S::CommittedRemote,
            E::VerificationFailed,
        ) => S::Quarantined,
        (S::Quarantined, E::QuarantineReleased) => S::Absent,
        _ => return Err(invalid(StateMachine::Page, from.as_str(), event.as_str())),
    };
    Ok(next)
}

event_enum! {
    SessionEvent {
        ReservationStarted => "reservation_started",
        ReservationCompleted => "reservation_completed",
        MaterializationCompleted => "materialization_completed",
        VerificationPassed => "verification_passed",
        LaunchRequested => "launch_requested",
        LaunchObserved => "launch_observed",
        HelpersDraining => "helpers_draining",
        ProcessesExited => "processes_exited",
        SealViolated => "seal_violated",
        AbortRequested => "abort_requested",
    }
}

pub fn transition_session(
    from: SessionState,
    event: SessionEvent,
) -> Result<SessionState, TransitionError> {
    use SessionEvent as E;
    use SessionState as S;
    let next = match (from, event) {
        (S::Planned, E::ReservationStarted) => S::Reserving,
        (S::Reserving, E::ReservationCompleted) => S::Materializing,
        (S::Materializing, E::MaterializationCompleted) => S::Verifying,
        (S::Verifying, E::VerificationPassed) => S::SealedReady,
        (S::SealedReady, E::LaunchRequested) => S::Launching,
        (S::Launching, E::LaunchObserved) => S::Active,
        (S::Active, E::HelpersDraining) => S::DrainingHelpers,
        (S::Active | S::DrainingHelpers, E::ProcessesExited) => S::Completed,
        (S::SealedReady | S::Launching | S::Active | S::DrainingHelpers, E::SealViolated) => {
            S::Violated
        }
        (
            S::Planned
            | S::Reserving
            | S::Materializing
            | S::Verifying
            | S::SealedReady
            | S::Launching
            | S::Active
            | S::DrainingHelpers,
            E::AbortRequested,
        ) => S::Aborted,
        _ => {
            return Err(invalid(
                StateMachine::Session,
                from.as_str(),
                event.as_str(),
            ));
        }
    };
    Ok(next)
}

event_enum! {
    UpdateEvent {
        NativeSnapshotStarted => "native_snapshot_started",
        ApplyStarted => "apply_started",
        DirtyPagesDetected => "dirty_pages_detected",
        StagingStarted => "staging_started",
        ContentStaged => "content_staged",
        ManifestUploaded => "manifest_uploaded",
        CommitUploaded => "commit_uploaded",
        CommitVerified => "commit_verified",
        LocalActivationRequested => "local_activation_requested",
        ActivationCommitted => "activation_committed",
        RollbackRequested => "rollback_requested",
        RollbackCompleted => "rollback_completed",
        RecoveryNeeded => "recovery_needed",
    }
}

pub fn transition_update(
    from: UpdateState,
    event: UpdateEvent,
) -> Result<UpdateState, TransitionError> {
    use UpdateEvent as E;
    use UpdateState as S;
    let next = match (from, event) {
        (S::Created | S::NativeSnapshotInProgress, E::NativeSnapshotStarted) => {
            S::NativeSnapshotInProgress
        }
        (S::NativeSnapshotInProgress | S::Applying, E::ApplyStarted) => S::Applying,
        (S::Applying | S::DirtyPagesPresent, E::DirtyPagesDetected) => S::DirtyPagesPresent,
        (S::Applying | S::DirtyPagesPresent | S::Staging, E::StagingStarted) => S::Staging,
        (S::Staging | S::AllContentStaged, E::ContentStaged) => S::AllContentStaged,
        (S::AllContentStaged | S::ManifestUploaded, E::ManifestUploaded) => S::ManifestUploaded,
        (S::ManifestUploaded | S::CommitUploaded, E::CommitUploaded) => S::CommitUploaded,
        (S::CommitUploaded | S::CommitVerified, E::CommitVerified) => S::CommitVerified,
        (S::CommitVerified | S::LocalActivationPending, E::LocalActivationRequested) => {
            S::LocalActivationPending
        }
        (S::LocalActivationPending | S::Committed, E::ActivationCommitted) => S::Committed,
        (
            S::Created
            | S::NativeSnapshotInProgress
            | S::Applying
            | S::DirtyPagesPresent
            | S::Staging
            | S::AllContentStaged
            | S::ManifestUploaded
            | S::CommitUploaded
            | S::CommitVerified
            | S::LocalActivationPending
            | S::RecoveryRequired
            | S::RollbackPending,
            E::RollbackRequested,
        ) => S::RollbackPending,
        (S::RollbackPending | S::RolledBack, E::RollbackCompleted) => S::RolledBack,
        (
            S::Created
            | S::NativeSnapshotInProgress
            | S::Applying
            | S::DirtyPagesPresent
            | S::Staging
            | S::AllContentStaged
            | S::ManifestUploaded
            | S::CommitUploaded
            | S::CommitVerified
            | S::LocalActivationPending
            | S::RollbackPending
            | S::RecoveryRequired,
            E::RecoveryNeeded,
        ) => S::RecoveryRequired,
        _ => return Err(invalid(StateMachine::Update, from.as_str(), event.as_str())),
    };
    Ok(next)
}

event_enum! {
    BackendHealthEvent {
        ProbeSucceeded => "probe_succeeded",
        ProbeDegraded => "probe_degraded",
        RateLimitObserved => "rate_limit_observed",
        AuthenticationFailed => "authentication_failed",
        NetworkOffline => "network_offline",
        ProviderUnavailable => "provider_unavailable",
        ProbeRequested => "probe_requested",
    }
}

pub fn transition_backend_health(
    _from: BackendHealthState,
    event: BackendHealthEvent,
) -> Result<BackendHealthState, TransitionError> {
    use BackendHealthEvent as E;
    Ok(match event {
        E::ProbeSucceeded => BackendHealthState::Healthy,
        E::ProbeDegraded => BackendHealthState::Degraded,
        E::RateLimitObserved => BackendHealthState::RateLimited,
        E::AuthenticationFailed => BackendHealthState::Unauthenticated,
        E::NetworkOffline => BackendHealthState::Offline,
        E::ProviderUnavailable => BackendHealthState::Unavailable,
        E::ProbeRequested => BackendHealthState::Unknown,
    })
}

const fn invalid(
    machine: StateMachine,
    from: &'static str,
    event: &'static str,
) -> TransitionError {
    TransitionError {
        machine,
        from,
        event,
    }
}
