//! Stable runtime states shared by the service, persistence, CLI, and UI.

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

macro_rules! state_enum {
    ($(#[$meta:meta])* $name:ident { $($variant:ident => $wire:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        #[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
        #[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
        pub enum $name { $($variant),+ }

        impl $name {
            /// Every state, used by exhaustive transition-contract tests.
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];

            /// Stable lowercase identifier for diagnostics and durable adapters.
            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $wire),+ }
            }
        }
    };
}

state_enum! {
    /// Repository lifecycle from first import through mount, play, update, and recovery.
    RepositoryState {
        Uninitialized => "uninitialized",
        Importing => "importing",
        UploadingBase => "uploading_base",
        VerifyingBase => "verifying_base",
        ReadyUnmounted => "ready_unmounted",
        Mounting => "mounting",
        ReadyMounted => "ready_mounted",
        AdmittingSession => "admitting_session",
        PlayingSealed => "playing_sealed",
        PlayingBalanced => "playing_balanced",
        Updating => "updating",
        Recovering => "recovering",
        Degraded => "degraded",
        Conflicted => "conflicted",
        Error => "error",
    }
}

impl RepositoryState {
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Conflicted | Self::Error)
    }

    #[must_use]
    pub const fn is_readable(self) -> bool {
        matches!(
            self,
            Self::ReadyMounted
                | Self::AdmittingSession
                | Self::PlayingSealed
                | Self::PlayingBalanced
                | Self::Updating
                | Self::Degraded
        )
    }

    #[must_use]
    pub const fn requires_journal(self) -> bool {
        matches!(self, Self::Updating | Self::Recovering | Self::Conflicted)
    }
}

state_enum! {
    /// Logical page lifecycle. Arena slot states are a later persistence projection.
    PageState {
        Absent => "absent",
        Fetching => "fetching",
        ResidentClean => "resident_clean",
        SessionPinned => "session_pinned",
        Evicting => "evicting",
        DirtyLocal => "dirty_local",
        Staging => "staging",
        StagedRemote => "staged_remote",
        CommittedRemote => "committed_remote",
        Quarantined => "quarantined",
    }
}

impl PageState {
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        false
    }

    /// Whether verified local bytes may satisfy a read immediately.
    #[must_use]
    pub const fn is_readable(self) -> bool {
        matches!(
            self,
            Self::ResidentClean | Self::SessionPinned | Self::DirtyLocal | Self::Staging
        )
    }

    /// Whether the page may release its local slot after lease checks.
    #[must_use]
    pub const fn is_evictable(self) -> bool {
        matches!(
            self,
            Self::ResidentClean | Self::StagedRemote | Self::CommittedRemote
        )
    }

    #[must_use]
    pub const fn requires_journal(self) -> bool {
        matches!(
            self,
            Self::DirtyLocal | Self::Staging | Self::StagedRemote | Self::CommittedRemote
        )
    }
}

state_enum! {
    /// Sealed-session admission and process-lifetime state.
    SessionState {
        Planned => "planned",
        Reserving => "reserving",
        Materializing => "materializing",
        Verifying => "verifying",
        SealedReady => "sealed_ready",
        Launching => "launching",
        Active => "active",
        DrainingHelpers => "draining_helpers",
        Completed => "completed",
        Violated => "violated",
        Aborted => "aborted",
    }
}

impl SessionState {
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Violated | Self::Aborted)
    }

    #[must_use]
    pub const fn is_readable(self) -> bool {
        matches!(
            self,
            Self::SealedReady | Self::Launching | Self::Active | Self::DrainingHelpers
        )
    }
}

state_enum! {
    /// Durable update-journal states from architecture section 16.8.
    UpdateState {
        Created => "created",
        NativeSnapshotInProgress => "native_snapshot_in_progress",
        Applying => "applying",
        DirtyPagesPresent => "dirty_pages_present",
        Staging => "staging",
        AllContentStaged => "all_content_staged",
        ManifestUploaded => "manifest_uploaded",
        CommitUploaded => "commit_uploaded",
        CommitVerified => "commit_verified",
        LocalActivationPending => "local_activation_pending",
        Committed => "committed",
        RollbackPending => "rollback_pending",
        RolledBack => "rolled_back",
        RecoveryRequired => "recovery_required",
    }
}

impl UpdateState {
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Committed | Self::RolledBack)
    }

    /// Every update state is durable evidence, including terminal audit records.
    #[must_use]
    pub const fn requires_journal(self) -> bool {
        true
    }
}

state_enum! {
    /// Backend availability as observed by bounded probes and real operations.
    BackendHealthState {
        Unknown => "unknown",
        Healthy => "healthy",
        Degraded => "degraded",
        RateLimited => "rate_limited",
        Unauthenticated => "unauthenticated",
        Offline => "offline",
        Unavailable => "unavailable",
    }
}

impl BackendHealthState {
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        false
    }

    #[must_use]
    pub const fn is_readable(self) -> bool {
        matches!(self, Self::Healthy | Self::Degraded | Self::RateLimited)
    }
}
