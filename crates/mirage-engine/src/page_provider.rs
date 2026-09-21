use crate::PageLocationMap;
use mirage_backend::ObjectBackend;
use mirage_cache::{ArenaShard, ReservationLedger, ReservationSnapshot, ResidentIndex};
use mirage_db::Database;
use mirage_pack::PackReadEncryption;
use mirage_scheduler::{FetchPool, FetchPoolConfig, FlightMap};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Default fetch-pool worker count; the service reports it as the readiness
/// record's `max_in_flight`.
pub const DEFAULT_FETCH_WORKERS: usize = 4;

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
    pub(crate) pool: Arc<FetchPool>,
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
            pool: FetchPool::new(FetchPoolConfig {
                workers: DEFAULT_FETCH_WORKERS,
                queue_depth: 256,
                speculative_queue_depth: 64,
                max_in_flight_bytes: 0,
            })
            .expect("fixed fetch pool configuration is valid"),
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

    #[must_use]
    pub fn with_fetch_pool(mut self, pool: Arc<FetchPool>) -> Self {
        self.pool = pool;
        self
    }

    /// The backend as a trait object for non-provider consumers (payload
    /// publication, evicted-payload fetch).
    #[must_use]
    pub fn backend_arc(&self) -> Arc<dyn ObjectBackend>
    where
        B: 'static,
    {
        Arc::clone(&self.backend) as Arc<dyn ObjectBackend>
    }

    /// In-flight flights, queued pool jobs, and running pool jobs.
    #[must_use]
    pub fn flight_metrics(&self) -> (usize, usize, usize) {
        (self.flights.len(), self.pool.queued(), self.pool.running())
    }

    /// Current budget ledger counters: committed, reserved, dirty-reserved, and
    /// peak envelope bytes.
    pub fn budget_snapshot(&self) -> Result<ReservationSnapshot, mirage_types::MirageError> {
        self.budget.snapshot()
    }
}
