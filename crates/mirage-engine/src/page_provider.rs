use crate::PageLocationMap;
use mirage_backend::ObjectBackend;
use mirage_cache::{ArenaShard, ReservationLedger, ResidentIndex};
use mirage_db::Database;
use mirage_pack::PackReadEncryption;
use mirage_scheduler::FlightMap;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub struct FetchContext {
    pub priority: mirage_scheduler::FetchPriority,
    pub deadline_ns: u64,
    pub cancellation: CancellationToken,
}
pub struct PageProvider<B: ObjectBackend> {
    pub(crate) backend: Arc<B>,
    pub(crate) db: Database,
    pub(crate) shard: Arc<ArenaShard>,
    pub(crate) index: Arc<ResidentIndex>,
    pub(crate) locations: Arc<PageLocationMap>,
    pub(crate) flights: FlightMap,
    pub(crate) budget: ReservationLedger,
    pub(crate) max_window: u64,
    pub(crate) encryption: Option<PackReadEncryption>,
}
impl<B: ObjectBackend> PageProvider<B> {
    #[must_use]
    pub fn new(
        backend: Arc<B>,
        db: Database,
        shard: Arc<ArenaShard>,
        index: Arc<ResidentIndex>,
        locations: Arc<PageLocationMap>,
        budget: ReservationLedger,
        max_window: u64,
    ) -> Self {
        Self {
            backend,
            db,
            shard,
            index,
            locations,
            flights: FlightMap::default(),
            budget,
            max_window,
            encryption: None,
        }
    }

    #[must_use]
    pub fn with_encryption(mut self, encryption: PackReadEncryption) -> Self {
        self.encryption = Some(encryption);
        self
    }
}
