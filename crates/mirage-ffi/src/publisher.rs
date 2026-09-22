//! Managed-volume payload publisher: uploads committed journal payloads as
//! immutable encrypted remote objects so the local copies can be evicted
//! under budget pressure and fetched back on demand.
//!
//! The publisher runs on its own thread and never blocks filesystem
//! callbacks: payload files are read outside the dirty ledger lock, uploads
//! go through the bounded backend interface, and only the durable record is
//! written through the single-writer channel.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use mirage_backend::{ObjectKind, UploadSource};
use mirage_crypto::aead::RepositoryKey;
use mirage_db::Database;
use mirage_db::payload_remote::UnpublishedPayload;
use mirage_pack::encrypted_frame::{EncryptedFrameAad, encode_encrypted_frame};
use mirage_types::ContentHash;
use mirage_types::{MirageError, RepositoryId};
use tokio_util::sync::CancellationToken;

/// Plaintext bytes per encrypted frame inside a payload object.
pub const PAYLOAD_FRAME_BYTES: u64 = 4 * 1024 * 1024;

/// Number of plaintext frames for a payload of `plaintext_len` bytes. A
/// non-empty payload always has at least one frame; an empty payload
/// encodes as a single empty frame so the object is never zero bytes.
#[must_use]
pub fn payload_frame_count(plaintext_len: u64) -> u64 {
    plaintext_len.div_ceil(PAYLOAD_FRAME_BYTES).max(1)
}

/// Per-frame wire overhead (magic + header + nonce + AEAD tag), measured
/// once from the real encoder rather than duplicated as a constant.
pub fn frame_overhead() -> u64 {
    static OVERHEAD: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *OVERHEAD.get_or_init(|| {
        let key = RepositoryKey::from_bytes([0x51; 32]);
        let aad = EncryptedFrameAad {
            repository: RepositoryId::from_bytes([0; 16]),
            pack_id: [0; 16],
            frame_index: 0,
            plaintext_hash: mirage_types::PageHash::from_bytes(*blake3::hash(b"x").as_bytes()),
            plaintext_length: 1,
        };
        let encoded =
            encode_encrypted_frame(&key, b"x", aad).expect("probe frame encodes") as Vec<u8>;
        encoded.len() as u64 - 1
    })
}

/// Byte range of `frame_index` inside the encoded payload object:
/// `(offset, encoded_length)`. `None` when the index is out of range.
#[must_use]
pub fn payload_frame_range(plaintext_len: u64, frame_index: u64) -> Option<(u64, u64)> {
    let count = payload_frame_count(plaintext_len);
    if frame_index >= count {
        return None;
    }
    let overhead = frame_overhead();
    let offset = frame_index * (overhead + PAYLOAD_FRAME_BYTES);
    let plaintext_here = if frame_index + 1 == count {
        plaintext_len - frame_index * PAYLOAD_FRAME_BYTES
    } else {
        PAYLOAD_FRAME_BYTES
    };
    Some((offset, overhead + plaintext_here))
}

/// Encodes one plaintext payload into its remote object bytes: a
/// concatenation of fixed-size encrypted frames. Returns the object bytes
/// and the per-frame plaintext hashes.
pub fn encode_payload_object(
    key: &RepositoryKey,
    repository: RepositoryId,
    payload_id: [u8; 16],
    plaintext: &[u8],
) -> Result<(Vec<u8>, Vec<[u8; 32]>), MirageError> {
    let count = payload_frame_count(plaintext.len() as u64);
    let mut output = Vec::new();
    let mut frame_hashes = Vec::with_capacity(count as usize);
    for index in 0..count {
        let start = (index * PAYLOAD_FRAME_BYTES) as usize;
        let end = ((index + 1) * PAYLOAD_FRAME_BYTES).min(plaintext.len() as u64) as usize;
        let chunk = &plaintext[start..end];
        let plaintext_hash = mirage_types::PageHash::from_bytes(*blake3::hash(chunk).as_bytes());
        frame_hashes.push(*plaintext_hash.as_bytes());
        let frame = encode_encrypted_frame(
            key,
            chunk,
            EncryptedFrameAad {
                repository,
                pack_id: payload_id,
                frame_index: index,
                plaintext_hash,
                plaintext_length: 0,
            },
        )?;
        output.extend_from_slice(&frame);
    }
    Ok((output, frame_hashes))
}

/// Publisher lifecycle states reported through `mirage_engine_publication_stats`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PublisherState {
    Idle = 0,
    WaitingForToken = 1,
    Publishing = 2,
    Draining = 3,
}

/// Shared, readable publisher status.
pub struct PublisherStats {
    pub state: AtomicU8,
    pub integrity_refusals: AtomicU64,
    pub last_error_class: Mutex<[u8; 32]>,
}

impl PublisherStats {
    fn record_error(&self, message: &str) {
        if let Ok(mut slot) = self.last_error_class.lock() {
            let bytes = message.as_bytes();
            let take = bytes.len().min(31);
            slot.fill(0);
            slot[..take].copy_from_slice(&bytes[..take]);
        }
    }
}

/// Background publisher: woken on flush fences and a 5 s poll, drains best
/// effort on shutdown. Uploads are sequential — the single writer channel
/// would serialize the records anyway.
pub struct Publisher {
    wake: Arc<(Mutex<bool>, Condvar)>,
    stop: Arc<AtomicBool>,
    done: Mutex<std::sync::mpsc::Receiver<()>>,
    pub stats: Arc<PublisherStats>,
}

/// Thread context handed to the publisher loop.
struct PublisherCtx {
    db: Database,
    volume: RepositoryId,
    journal_dir: PathBuf,
    provider: Arc<crate::handles::ManagedProvider>,
    key: Arc<RepositoryKey>,
    stats: Arc<PublisherStats>,
}

impl Publisher {
    /// Starts the publisher thread. `backend` is probed every pass so token
    /// arrival (provider install) flips the loop from idling to publishing
    /// without a restart.
    pub fn spawn(
        db: Database,
        volume: RepositoryId,
        journal_dir: PathBuf,
        provider: Arc<crate::handles::ManagedProvider>,
        key: Arc<RepositoryKey>,
    ) -> Self {
        let wake = Arc::new((Mutex::new(false), Condvar::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let stats = Arc::new(PublisherStats {
            state: AtomicU8::new(PublisherState::WaitingForToken as u8),
            integrity_refusals: AtomicU64::new(0),
            last_error_class: Mutex::new([0; 32]),
        });
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        {
            let ctx = PublisherCtx {
                db,
                volume,
                journal_dir,
                provider,
                key,
                stats: Arc::clone(&stats),
            };
            let wake = Arc::clone(&wake);
            let stop = Arc::clone(&stop);
            std::thread::Builder::new()
                .name("mirage-publisher".into())
                .spawn(move || {
                    publisher_loop(&ctx, wake, stop);
                    let _ = done_tx.send(());
                })
                .expect("publisher thread spawns");
        }
        Self {
            wake,
            stop,
            done: Mutex::new(done_rx),
            stats,
        }
    }

    /// Wakes the loop for a pass (flush fence, timer).
    pub fn notify(&self) {
        if let Ok(mut flag) = self.wake.0.lock() {
            *flag = true;
            self.wake.1.notify_one();
        }
    }

    /// Drain pass during quiesce: flips the loop into drain mode and wakes.
    pub fn drain(&self) {
        self.stats
            .state
            .store(PublisherState::Draining as u8, Ordering::Release);
        self.notify();
    }

    /// Signals shutdown and waits for the loop to exit, bounded. An
    /// in-flight upload finishes first — the durable record is idempotent,
    /// so a stopped-mid-upload payload simply retries next mount.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Release);
        self.notify();
        // Bound the join: a wedged transport must not hang host shutdown.
        let done = self
            .done
            .lock()
            .map(|guard| guard.recv_timeout(Duration::from_secs(60)).is_ok());
        if !done.unwrap_or(false) {
            eprintln!("payload publisher did not stop within 60s; uploads retry next mount");
        }
    }
}

fn publisher_loop(ctx: &PublisherCtx, wake: Arc<(Mutex<bool>, Condvar)>, stop: Arc<AtomicBool>) {
    let PublisherCtx {
        db,
        volume,
        journal_dir,
        provider,
        key,
        stats,
    } = ctx;
    let mut backoff = Duration::from_millis(0);
    loop {
        // Wait for work: wake flag, 5 s poll, or stop.
        if let Ok(mut flag) = wake.0.lock() {
            let deadline = Instant::now() + Duration::from_secs(5) + backoff;
            while !*flag && !stop.load(Ordering::Acquire) {
                let now = Instant::now();
                if now >= deadline {
                    break;
                }
                let (guard, _) = match wake.1.wait_timeout(flag, deadline - now) {
                    Ok(pair) => pair,
                    Err(_) => return,
                };
                flag = guard;
            }
            *flag = false;
        }
        if stop.load(Ordering::Acquire) {
            return;
        }
        let draining = stats.state.load(Ordering::Acquire) == PublisherState::Draining as u8;
        if !provider.installed.load(Ordering::Acquire) {
            if !draining {
                stats
                    .state
                    .store(PublisherState::WaitingForToken as u8, Ordering::Release);
            }
            continue;
        }
        if !draining {
            stats
                .state
                .store(PublisherState::Publishing as u8, Ordering::Release);
        }
        let Ok(pending) = db.unpublished_payloads(*volume) else {
            stats.record_error("unpublished_payloads query failed");
            continue;
        };
        if pending.is_empty() {
            if draining {
                return;
            }
            stats
                .state
                .store(PublisherState::Idle as u8, Ordering::Release);
            continue;
        }
        let mut any_error = false;
        for payload in &pending {
            if stop.load(Ordering::Acquire) {
                return;
            }
            match publish_one(db, *volume, journal_dir, provider, key, payload, stats) {
                Ok(()) => {}
                Err(PublishFailure::Backend) => {
                    any_error = true;
                    break;
                }
                Err(PublishFailure::Integrity) => {}
            }
        }
        backoff = if any_error {
            (backoff * 2)
                .max(Duration::from_secs(1))
                .min(Duration::from_secs(60))
        } else {
            Duration::from_millis(0)
        };
    }
}

enum PublishFailure {
    Backend,
    Integrity,
}

fn publish_one(
    db: &Database,
    volume: RepositoryId,
    journal_dir: &Path,
    provider: &crate::handles::ManagedProvider,
    key: &RepositoryKey,
    payload: &UnpublishedPayload,
    stats: &PublisherStats,
) -> Result<(), PublishFailure> {
    let path = journal_dir.join(&payload.path);
    let plaintext = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) => {
            // A vanished file is not publishable; superseded payloads lose
            // their extent reference and fall out of the pending set.
            stats.record_error("payload file unreadable");
            eprintln!(
                "payload publish skipped: {} unreadable: {error}",
                payload.path
            );
            return Err(PublishFailure::Integrity);
        }
    };
    if let Some(expected) = payload.checksum
        && blake3::hash(&plaintext).as_bytes() != &expected
    {
        stats.integrity_refusals.fetch_add(1, Ordering::Relaxed);
        stats.record_error("payload checksum mismatch");
        eprintln!(
            "payload publish refused: {} checksum mismatch",
            payload.path
        );
        return Err(PublishFailure::Integrity);
    }
    let (object, frame_hashes) =
        match encode_payload_object(key, volume, payload.payload_id, &plaintext) {
            Ok(encoded) => encoded,
            Err(_) => {
                stats.record_error("payload frame encode failed");
                return Err(PublishFailure::Integrity);
            }
        };
    let object_hash = *blake3::hash(&object).as_bytes();
    let object_length = object.len() as u64;
    let source = UploadSource::from_bytes(bytes::Bytes::from(object));
    let result = futures_executor::block_on(provider.backend().put_immutable(
        ObjectKind::Payload,
        source,
        ContentHash::from_bytes(object_hash),
        CancellationToken::new(),
    ));
    let reference = match result {
        Ok(reference) => reference,
        Err(error) => {
            let class = format!("{:?}", error.class);
            stats.record_error(&class);
            eprintln!("payload publish backend error: {class}");
            return Err(PublishFailure::Backend);
        }
    };
    if reference.content_hash.as_bytes() != &object_hash
        || reference.byte_length.as_u64() != object_length
        || reference.kind != ObjectKind::Payload
    {
        stats.record_error("published identity mismatch");
        stats.integrity_refusals.fetch_add(1, Ordering::Relaxed);
        return Err(PublishFailure::Integrity);
    }
    // Only independent readback makes a local payload eligible for eviction.
    // A successful upload response or echoed appProperties hash is insufficient.
    if let Err(error) = futures_executor::block_on(mirage_backend::verify_object_bytes(
        provider.backend().as_ref(),
        &reference,
        CancellationToken::new(),
    )) {
        stats.record_error("publication readback failed");
        if error.class == mirage_backend::BackendErrorClass::Integrity {
            stats.integrity_refusals.fetch_add(1, Ordering::Relaxed);
            return Err(PublishFailure::Integrity);
        }
        return Err(PublishFailure::Backend);
    }
    let record = mirage_db::payload_remote::PayloadRemoteObject {
        volume_id: volume,
        payload_id: payload.payload_id,
        provider_object_id: reference.provider_object_id.as_str().to_owned(),
        immutable_revision: reference
            .immutable_revision
            .map(|revision| revision.as_str().to_owned()),
        object_length: i64::try_from(object_length).unwrap_or(i64::MAX),
        object_hash,
        plaintext_length: i64::try_from(plaintext.len()).unwrap_or(i64::MAX),
        plaintext_hash: *blake3::hash(&plaintext).as_bytes(),
        frame_hashes,
        published_ns: now_ns_i64_pub(),
    };
    match db.writer().payload_published(record) {
        Ok(()) => Ok(()),
        Err(error) => {
            // Unreferenced payload: the upload is orphaned. Deletion needs a
            // signed proof we cannot mint here, so the object is left for
            // repository-side GC; the record failure is surfaced.
            stats.record_error("payload publication record failed");
            eprintln!("payload publication record failed: {error:?}");
            Err(PublishFailure::Integrity)
        }
    }
}

fn now_ns_i64_pub() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|span| i64::try_from(span.as_nanos()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// True when `inode` is pinned itself or sits under a pinned directory;
/// ancestors resolve live through `dirents` so a pin on a directory covers
/// children created after the pin.
pub fn pin_held(
    db: &Database,
    volume: RepositoryId,
    inode: mirage_types::InodeId,
    pins: &std::collections::HashSet<mirage_types::InodeId>,
) -> bool {
    let mut current = inode;
    let mut seen = std::collections::HashSet::new();
    for _ in 0..4096 {
        if pins.contains(&current) {
            return true;
        }
        if !seen.insert(current) {
            return true; // A malformed ancestry is not evidence of evictability.
        }
        match db.namespace_entry(volume, current) {
            Ok(Some((parent, _))) => current = parent,
            Ok(None) => {
                return db
                    .namespace_root(volume)
                    .map_or(true, |root| root != Some(current));
            }
            Err(_) => return true,
        }
    }
    true
}

/// Evicts published payload files until `target` plaintext bytes are freed.
/// Returns `(freed, pinned_blocked)`: payloads skipped solely because a pin
/// covers one of their inodes accumulate into `pinned_blocked`. Each payload
/// is also skipped when an inode referencing it has an open handle. The
/// extent is marked dead inside the writer transaction and the file is
/// deleted only after the ledger commit — a crash leaves either a live file
/// or a dead extent with a fetchable remote object.
pub fn evict_published(
    db: &Database,
    dirty: &crate::handles::DirtyLedger,
    journal_dir: &std::path::Path,
    volume: RepositoryId,
    handles: &mirage_engine::handles::HandleTable,
    pins: &std::collections::HashSet<mirage_types::InodeId>,
    target: u64,
) -> Result<(u64, u64), MirageError> {
    let evictable = db.published_payloads_evictable(volume)?;
    let mut freed = 0_u64;
    let mut pinned_blocked = 0_u64;
    for (payload_id, plaintext_length, _published_ns) in evictable {
        if freed >= target {
            break;
        }
        let inodes = db.payload_inodes(volume, &payload_id)?;
        if inodes
            .iter()
            .any(|inode| pin_held(db, volume, *inode, pins))
        {
            pinned_blocked += u64::try_from(plaintext_length).unwrap_or(0);
            continue;
        }
        if inodes.iter().any(|inode| handles.is_open(*inode)) {
            continue;
        }
        let mut name = String::with_capacity(40);
        for byte in payload_id {
            name.push_str(&format!("{byte:02x}"));
        }
        name.push_str(".payload");
        // Never release the physical charge before deletion succeeds. If a
        // later ledger update fails, the missing, remotely recoverable file
        // stays conservatively charged until another eviction pass finishes.
        match std::fs::remove_file(journal_dir.join(name)) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(MirageError::from(error)),
        }
        db.writer()
            .physical_mark_extent_dead(payload_id, now_ns_i64_pub())?;
        let length = u64::try_from(plaintext_length).unwrap_or(0);
        let _ = dirty
            .used
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                Some(used.saturating_sub(length))
            });
        freed += length;
    }
    Ok((freed, pinned_blocked))
}

/// Remote fetch support for evicted payloads: a bounded in-memory cache of
/// decrypted frames plus per-frame single-flight so concurrent readers share
/// one backend fetch.
pub struct RemotePayloadStore {
    backend: Arc<dyn mirage_backend::ObjectBackend>,
    /// Publication is live only after the provider install (first token).
    installed: Arc<AtomicBool>,
    key: Arc<RepositoryKey>,
    repository: RepositoryId,
    /// (payload_id, frame_index) → decrypted plaintext chunk. Bounded by
    /// bytes; evicted wholesale when the budget is exceeded (LRU precision
    /// is not worth the bookkeeping for correctness-only data).
    frames: FrameCache,
    flights: FrameFlights,
    /// Bounded publication-record cache: payload_id → Option<record>.
    publications: PublicationCache,
    /// Payloads currently being re-staged to disk; dedupes concurrent reads
    /// so one cold open does one fetch+write.
    restaging: Mutex<std::collections::BTreeSet<[u8; 16]>>,
}

type FrameCache = Mutex<(std::collections::HashMap<([u8; 16], u64), Vec<u8>>, u64)>;
type FrameFlights = Mutex<std::collections::BTreeMap<([u8; 16], u64), Arc<FrameFlight>>>;
type PublicationCache = Mutex<
    std::collections::HashMap<
        [u8; 16],
        Option<Arc<mirage_db::payload_remote::PayloadRemoteObject>>,
    >,
>;

const FRAME_CACHE_BUDGET: u64 = 64 * 1024 * 1024;
const PUBLICATION_CACHE_LIMIT: usize = 4096;

struct FrameFlight {
    done: Mutex<Option<Result<Vec<u8>, FetchFailure>>>,
    ready: Condvar,
}

#[derive(Clone, Copy)]
enum FetchFailure {
    Backend,
    Integrity,
}

impl RemotePayloadStore {
    #[must_use]
    pub fn new(
        backend: Arc<dyn mirage_backend::ObjectBackend>,
        installed: Arc<AtomicBool>,
        key: Arc<RepositoryKey>,
        repository: RepositoryId,
    ) -> Self {
        Self {
            backend,
            installed,
            key,
            repository,
            frames: Mutex::new((std::collections::HashMap::new(), 0)),
            flights: Mutex::new(std::collections::BTreeMap::new()),
            publications: Mutex::new(std::collections::HashMap::new()),
            restaging: Mutex::new(std::collections::BTreeSet::new()),
        }
    }

    /// The publication record for a payload, cached (bounded).
    fn publication(
        &self,
        db: &Database,
        volume: RepositoryId,
        payload_id: &[u8; 16],
    ) -> Result<Option<Arc<mirage_db::payload_remote::PayloadRemoteObject>>, MirageError> {
        if let Ok(cache) = self.publications.lock()
            && let Some(hit) = cache.get(payload_id)
        {
            return Ok(hit.clone());
        }
        let record = db.payload_publication(volume, payload_id)?.map(Arc::new);
        if let Ok(mut cache) = self.publications.lock() {
            if cache.len() >= PUBLICATION_CACHE_LIMIT {
                cache.clear();
            }
            cache.insert(*payload_id, record.clone());
        }
        Ok(record)
    }

    /// Decrypted plaintext for one frame: cache → single-flight fetch →
    /// verify → cache. Errors are typed, never partial.
    fn frame(
        &self,
        record: &mirage_db::payload_remote::PayloadRemoteObject,
        frame_index: u64,
    ) -> Result<Vec<u8>, FetchFailure> {
        let flight_key = (record.payload_id, frame_index);
        if let Ok(guard) = self.frames.lock()
            && let Some(hit) = guard.0.get(&flight_key)
        {
            return Ok(hit.clone());
        }
        // Single-flight: the creator fetches, others wait on the condvar.
        let flight = {
            let mut flights = self.flights.lock().map_err(|_| FetchFailure::Backend)?;
            match flights.get(&flight_key) {
                Some(existing) => Arc::clone(existing),
                None => {
                    let created = Arc::new(FrameFlight {
                        done: Mutex::new(None),
                        ready: Condvar::new(),
                    });
                    flights.insert(flight_key, Arc::clone(&created));
                    drop(flights);
                    let result = self.fetch_frame(record, frame_index);
                    {
                        let mut done = flight_done(&created);
                        *done = Some(result);
                    }
                    created.ready.notify_all();
                    if let Ok(mut flights) = self.flights.lock() {
                        flights.remove(&flight_key);
                    }
                    return match flight_result(&created) {
                        Some(result) => result,
                        None => Err(FetchFailure::Backend),
                    };
                }
            }
        };
        let mut done = flight.done.lock().map_err(|_| FetchFailure::Backend)?;
        while done.is_none() {
            done = flight.ready.wait(done).map_err(|_| FetchFailure::Backend)?;
        }
        done.clone().unwrap()
    }

    fn fetch_frame(
        &self,
        record: &mirage_db::payload_remote::PayloadRemoteObject,
        frame_index: u64,
    ) -> Result<Vec<u8>, FetchFailure> {
        let plaintext_len = u64::try_from(record.plaintext_length).unwrap_or(0);
        let Some((offset, length)) = payload_frame_range(plaintext_len, frame_index) else {
            return Err(FetchFailure::Integrity);
        };
        let range =
            mirage_types::CheckedRange::new(offset, length).map_err(|_| FetchFailure::Integrity)?;
        let reference = mirage_backend::RemoteObjectRef {
            backend_id: mirage_backend::BackendId::new("drive")
                .map_err(|_| FetchFailure::Backend)?,
            provider_object_id: mirage_backend::ProviderObjectId::new(
                record.provider_object_id.clone(),
            )
            .map_err(|_| FetchFailure::Backend)?,
            immutable_revision: record
                .immutable_revision
                .clone()
                .and_then(|r| mirage_backend::ImmutableRevision::new(r).ok()),
            byte_length: mirage_types::ByteCount::from_u64(
                u64::try_from(record.object_length).unwrap_or(0),
            ),
            content_hash: ContentHash::from_bytes(record.object_hash),
            kind: ObjectKind::Payload,
        };
        let read = futures_executor::block_on(self.backend.read_range(
            &reference,
            range,
            mirage_backend::FetchClass::BlockingRead,
            CancellationToken::new(),
        ))
        .map_err(|_| FetchFailure::Backend)?;
        let bytes = futures_executor::block_on(collect_stream(read.stream))
            .map_err(|_| FetchFailure::Backend)?;
        if bytes.len() as u64 != length {
            return Err(FetchFailure::Integrity);
        }
        let plaintext_length = usize::try_from(
            plaintext_len
                .saturating_sub(frame_index * PAYLOAD_FRAME_BYTES)
                .min(PAYLOAD_FRAME_BYTES),
        )
        .map_err(|_| FetchFailure::Integrity)?;
        let expected = record
            .frame_hashes
            .get(frame_index as usize)
            .copied()
            .ok_or(FetchFailure::Integrity)?;
        let plaintext = mirage_pack::encrypted_frame::decode_encrypted_frame(
            &self.key,
            &bytes,
            EncryptedFrameAad {
                repository: self.repository,
                pack_id: record.payload_id,
                frame_index,
                plaintext_hash: mirage_types::PageHash::from_bytes(expected),
                plaintext_length: u32::try_from(plaintext_length)
                    .map_err(|_| FetchFailure::Integrity)?,
            },
        )
        .map_err(|_| FetchFailure::Integrity)?;
        if plaintext.len() != plaintext_length {
            return Err(FetchFailure::Integrity);
        }
        // Admit to the bounded frame cache.
        if let Ok(mut guard) = self.frames.lock() {
            let (frames, used) = &mut *guard;
            let len = plaintext.len() as u64;
            if *used + len > FRAME_CACHE_BUDGET {
                frames.clear();
                *used = 0;
            }
            frames.insert(flight_key_of(record, frame_index), plaintext.clone());
            *used += len;
        }
        Ok(plaintext)
    }

    /// Reads `length` payload bytes starting at `offset`; `None` when the
    /// payload has no remote object. `Err` on backend/integrity failure.
    pub fn read(
        &self,
        db: &Database,
        volume: RepositoryId,
        payload_id: &[u8; 16],
        offset: u64,
        length: usize,
    ) -> Result<Option<Vec<u8>>, MirageError> {
        if !self.installed.load(Ordering::Acquire) {
            return Err(MirageError::backend_unavailable(
                "payload fetch requires an installed backend credential",
            ));
        }
        let Some(record) = self.publication(db, volume, payload_id)? else {
            return Ok(None);
        };
        let end = offset
            .checked_add(length as u64)
            .ok_or_else(|| MirageError::invalid_argument("payload read range overflows"))?;
        if end > record.plaintext_length as u64 {
            return Err(MirageError::integrity_mismatch(
                "payload read exceeds the published length",
            ));
        }
        let first = offset / PAYLOAD_FRAME_BYTES;
        let last = (end.saturating_sub(1)) / PAYLOAD_FRAME_BYTES;
        let mut out = Vec::with_capacity(length);
        for frame_index in first..=last {
            let plaintext = self
                .frame(&record, frame_index)
                .map_err(|failure| match failure {
                    FetchFailure::Backend => {
                        MirageError::backend_unavailable("evicted payload fetch failed")
                    }
                    FetchFailure::Integrity => {
                        MirageError::integrity_mismatch("evicted payload failed verification")
                    }
                })?;
            let frame_start = frame_index * PAYLOAD_FRAME_BYTES;
            let take_start = offset.saturating_sub(frame_start) as usize;
            let take_end = ((end - frame_start) as usize).min(plaintext.len());
            out.extend_from_slice(&plaintext[take_start..take_end]);
        }
        Ok(Some(out))
    }

    /// Full verified plaintext of an evicted payload plus its record, for
    /// re-staging the local file. `None` when no publication row exists.
    pub fn fetch_whole(
        &self,
        db: &Database,
        volume: RepositoryId,
        payload_id: &[u8; 16],
    ) -> Result<Option<WholePayload>, MirageError> {
        let Some(record) = self.publication(db, volume, payload_id)? else {
            return Ok(None);
        };
        let length = usize::try_from(record.plaintext_length)
            .map_err(|_| MirageError::integrity_mismatch("published payload length overflows"))?;
        let bytes = self
            .read(db, volume, payload_id, 0, length)?
            .ok_or_else(|| MirageError::integrity_mismatch("publication vanished mid-read"))?;
        if bytes.len() != length {
            return Err(MirageError::integrity_mismatch(
                "published payload fetched short",
            ));
        }
        if blake3::hash(&bytes).as_bytes() != &record.plaintext_hash {
            return Err(MirageError::integrity_mismatch(
                "restaged payload failed whole-object verification",
            ));
        }
        Ok(Some(WholePayload { bytes, record }))
    }

    /// Claims the re-stage slot for a payload; `None` while another reader
    /// is already re-staging it.
    pub fn begin_restage(&self, payload_id: &[u8; 16]) -> Option<RestageGuard<'_>> {
        let mut guard = self.restaging.lock().ok()?;
        if !guard.insert(*payload_id) {
            return None;
        }
        Some(RestageGuard {
            store: self,
            payload_id: *payload_id,
        })
    }
}

/// Decrypted, hash-verified bytes of an evicted payload plus its
/// publication record, for atomic re-staging into the journal.
pub struct WholePayload {
    pub bytes: Vec<u8>,
    pub record: Arc<mirage_db::payload_remote::PayloadRemoteObject>,
}

/// RAII release for a claimed re-stage slot.
pub struct RestageGuard<'a> {
    store: &'a RemotePayloadStore,
    payload_id: [u8; 16],
}
impl Drop for RestageGuard<'_> {
    fn drop(&mut self) {
        if let Ok(mut set) = self.store.restaging.lock() {
            set.remove(&self.payload_id);
        }
    }
}

fn flight_key_of(
    record: &mirage_db::payload_remote::PayloadRemoteObject,
    frame_index: u64,
) -> ([u8; 16], u64) {
    (record.payload_id, frame_index)
}

fn flight_done(
    flight: &FrameFlight,
) -> std::sync::MutexGuard<'_, Option<Result<Vec<u8>, FetchFailure>>> {
    flight
        .done
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
}

fn flight_result(flight: &FrameFlight) -> Option<Result<Vec<u8>, FetchFailure>> {
    flight.done.lock().ok().and_then(|done| done.clone())
}

async fn collect_stream(
    mut stream: mirage_backend::BackendByteStream,
) -> Result<Vec<u8>, mirage_backend::BackendError> {
    use futures_util::StreamExt;
    let mut out = Vec::new();
    while let Some(chunk) = stream.next().await {
        out.extend_from_slice(&chunk?);
    }
    Ok(out)
}
