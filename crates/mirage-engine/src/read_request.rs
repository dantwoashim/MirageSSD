use std::time::Instant;

use tokio_util::sync::CancellationToken;

/// End-to-end scheduler priority. Lower numeric rank is more urgent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReadPriority {
    Blocking,
    MandatoryAdmission,
    CapsuleAdmission,
    LiveFrontier,
    ReadAhead,
    IdleWarm,
    Maintenance,
}

impl ReadPriority {
    #[must_use]
    pub const fn rank(self) -> u8 {
        match self {
            Self::Blocking => 0,
            Self::MandatoryAdmission => 1,
            Self::CapsuleAdmission => 2,
            Self::LiveFrontier => 3,
            Self::ReadAhead => 4,
            Self::IdleWarm => 5,
            Self::Maintenance => 6,
        }
    }
}

/// Process classification used for policy and telemetry, never for path authorization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProcessRole {
    Game,
    Launcher,
    AntiCheat,
    Helper,
    Updater,
    System,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BufferCacheMode {
    Buffered,
    Unbuffered { alignment: u32 },
    MemoryMapped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AccessPattern {
    Unspecified,
    Sequential,
    Random,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BufferingHint {
    pub cache_mode: BufferCacheMode,
    pub access_pattern: AccessPattern,
}

impl Default for BufferingHint {
    fn default() -> Self {
        Self {
            cache_mode: BufferCacheMode::Buffered,
            access_pattern: AccessPattern::Unspecified,
        }
    }
}

/// Per-read policy supplied by the adapter or internal caller.
#[derive(Debug, Clone)]
pub struct ReadContext {
    pub priority: ReadPriority,
    pub deadline: Instant,
    pub cancellation: CancellationToken,
    pub process_role: ProcessRole,
    pub buffering: BufferingHint,
}

impl ReadContext {
    #[must_use]
    pub fn is_expired(&self) -> bool {
        Instant::now() >= self.deadline
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancellation.is_cancelled()
    }
}
