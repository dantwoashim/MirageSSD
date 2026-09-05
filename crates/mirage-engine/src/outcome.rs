use mirage_types::{ByteCount, PageOrdinal};

/// Highest-cost tier involved in satisfying a read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CacheTier {
    Memory,
    SsdArena,
    RemoteVerified,
    Mixed,
}

/// Exact page that escaped a sealed capsule during a blocking read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SealViolation {
    pub file_index: u32,
    pub page: PageOrdinal,
}

/// Successful read facts. A success always describes the exact transferred bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadOutcome {
    pub bytes_transferred: ByteCount,
    pub cache_tier: CacheTier,
    pub pages_touched: Vec<PageOrdinal>,
    pub seal_violation: Option<SealViolation>,
}

impl ReadOutcome {
    #[must_use]
    pub fn empty(cache_tier: CacheTier) -> Self {
        Self {
            bytes_transferred: ByteCount::ZERO,
            cache_tier,
            pages_touched: Vec::new(),
            seal_violation: None,
        }
    }
}
