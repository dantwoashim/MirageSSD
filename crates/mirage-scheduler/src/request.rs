use mirage_backend::RemoteObjectRef;
use mirage_cache::BudgetReservation;
use mirage_types::{CheckedRange, PageHash};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum FetchPriority {
    P0Blocking = 0,
    P1Mandatory = 1,
    P2Capsule = 2,
    P3Frontier = 3,
    P4ReadAhead = 4,
    P5IdleWarm = 5,
    P6Maintenance = 6,
}
impl FetchPriority {
    #[must_use]
    pub const fn speculative(self) -> bool {
        matches!(
            self,
            Self::P3Frontier | Self::P4ReadAhead | Self::P5IdleWarm | Self::P6Maintenance
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageRequest {
    pub hash: PageHash,
    pub encoded_range: CheckedRange,
    pub logical_length: u32,
}

pub struct FetchRequest {
    pub pages: Vec<PageRequest>,
    pub object: RemoteObjectRef,
    pub priority: FetchPriority,
    pub deadline_ns: u64,
    pub sequence: u64,
    pub cancellation: CancellationToken,
    pub budget: Option<BudgetReservation>,
}
