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

use crate::remote_cache::ByteLru;
use mirage_backend::{ObjectKind, UploadSource};
use mirage_crypto::aead::RepositoryKey;
use mirage_db::Database;
use mirage_db::payload_remote::UnpublishedPayload;
use mirage_pack::encrypted_frame::{EncryptedFrameAad, encode_encrypted_frame};
use mirage_pack::verified_sector::{
    ENCRYPTED_FRAME_PREFIX_BYTES, FrameProofCatalog, VERIFIED_SECTOR_BYTES, decode_verified_sector,
    learn_verified_frame,
};
use mirage_types::ContentHash;
use mirage_types::{MirageError, RepositoryId};
use tokio_util::sync::CancellationToken;

/// Plaintext bytes per encrypted frame inside a payload object.
pub const PAYLOAD_FRAME_BYTES: u64 = 4 * 1024 * 1024;

/// Concurrent payload uploads per publisher pass. Each Drive object costs ~5
/// round trips (dedup lookup, session start, upload, metadata + readback
/// verify), so small payloads are latency-bound and need many in flight.
pub const PUBLISH_PARALLELISM: usize = 16;
/// Floor of the adaptive in-flight byte budget for a publish batch; also
/// its starting value. Home uplinks are ~1-10 MB/s: with several hundred
/// MiB in flight each request's share of the link pushed uploads and
/// readbacks past the 5-minute transport timeout, and the resulting errors
/// backed the whole publisher off. On a slow link the sizer stays at
/// 64 MiB, keeping every request well inside that timeout.
pub const PUBLISH_BATCH_BYTES: u64 = 64 * 1024 * 1024;
/// Ceiling of the adaptive batch budget. Each in-flight upload buffers one
/// chunk, so this also bounds upload memory next to a running game.
const PUBLISH_BATCH_MAX_BYTES: u64 = 256 * 1024 * 1024;
/// Batch duration the sizer steers toward: at ~45 s per batch the 5-minute
/// transport-timeout rationale above still holds.
const BATCH_TARGET: Duration = Duration::from_secs(45);

/// Adaptive in-flight byte budget for publish batches. Starts at
/// PUBLISH_BATCH_BYTES — the previous fixed cap, which is right for slow
/// uplinks — and grows toward PUBLISH_BATCH_MAX_BYTES when the measured
/// batch rate would fill a larger batch inside BATCH_TARGET. Growth is at
/// most 2x the current cap per batch; a backend failure resets the cap to
/// the floor alongside the publisher's back-off.
struct BatchSizer {
    cap: u64,
}

impl BatchSizer {
    fn new() -> Self {
        Self {
            cap: PUBLISH_BATCH_BYTES,
        }
    }

    fn cap(&self) -> u64 {
        self.cap
    }

    /// Adapts the cap from one successful batch's observed plaintext rate.
    fn record_success(&mut self, bytes: u64, elapsed: Duration) {
        let rate = bytes as f64 / elapsed.as_secs_f64().max(0.001);
        let target = (rate * BATCH_TARGET.as_secs_f64()) as u64;
        let target = target.clamp(PUBLISH_BATCH_BYTES, PUBLISH_BATCH_MAX_BYTES);
        self.cap = target.min(self.cap.saturating_mul(2));
    }

    fn record_failure(&mut self) {
        self.cap = PUBLISH_BATCH_BYTES;
    }
}

/// Largest plaintext payload that may join a pack — bigger payloads publish
/// as their own remote object exactly as before.
const PACK_MEMBER_MAX_BYTES: u64 = 4 * 1024 * 1024;
/// Plaintext bytes per packed remote object (encoded members are simply
/// concatenated; each member's row records its `member_offset`).
const PACK_TARGET_BYTES: u64 = 16 * 1024 * 1024;
/// Most payloads in one pack.
const PACK_MAX_MEMBERS: usize = 1024;

/// One remote object to upload: a single payload (member_offset 0) or a pack
/// of small payloads — Drive charges ~5 round trips per object, so packing
/// turns the per-object latency floor into a per-pack one.
struct PublishUnit<'a> {
    members: Vec<&'a UnpublishedPayload>,
}

fn unit_bytes(unit: &PublishUnit<'_>) -> u64 {
    unit.members
        .iter()
        .map(|member| u64::try_from(member.bytes).unwrap_or(0))
        .fold(0u64, u64::saturating_add)
}

/// Splits the pending list (device-seq order) into publish units: payloads
/// over PACK_MEMBER_MAX_BYTES become singles; the rest accumulate into packs
/// bounded by PACK_TARGET_BYTES and PACK_MAX_MEMBERS.
fn publish_units(pending: &[UnpublishedPayload]) -> Vec<PublishUnit<'_>> {
    let mut units = Vec::new();
    let mut pack: Vec<&UnpublishedPayload> = Vec::new();
    let mut pack_bytes = 0u64;
    for payload in pending {
        let bytes = u64::try_from(payload.bytes).unwrap_or(0);
        if bytes > PACK_MEMBER_MAX_BYTES {
            if !pack.is_empty() {
                units.push(PublishUnit {
                    members: std::mem::take(&mut pack),
                });
                pack_bytes = 0;
            }
            units.push(PublishUnit {
                members: vec![payload],
            });
            continue;
        }
        if !pack.is_empty()
            && (pack_bytes.saturating_add(bytes) > PACK_TARGET_BYTES
                || pack.len() >= PACK_MAX_MEMBERS)
        {
            units.push(PublishUnit {
                members: std::mem::take(&mut pack),
            });
            pack_bytes = 0;
        }
        pack.push(payload);
        pack_bytes = pack_bytes.saturating_add(bytes);
    }
    if !pack.is_empty() {
        units.push(PublishUnit { members: pack });
    }
    units
}

/// One batch starting at `start`: at most PUBLISH_PARALLELISM units and
/// `cap` total member plaintext (a single larger unit still forms its own
/// batch). Returns the end cursor.
fn next_batch(units: &[PublishUnit<'_>], start: usize, cap: u64) -> usize {
    let mut end = start;
    let mut bytes = 0_u64;
    while end < units.len() && end - start < PUBLISH_PARALLELISM {
        let next = unit_bytes(&units[end]);
        if end > start && bytes.saturating_add(next) > cap {
            break;
        }
        bytes = bytes.saturating_add(next);
        end += 1;
    }
    end
}

/// Splits the unit list into batches of at most PUBLISH_PARALLELISM units
/// and `cap` total member plaintext (a single larger unit still forms its
/// own batch). The publisher loop batches incrementally through next_batch
/// so each batch uses the sizer's current cap; this whole-list split backs
/// the batching tests.
#[cfg(test)]
fn publish_batches<'a>(units: &'a [PublishUnit<'a>], cap: u64) -> Vec<&'a [PublishUnit<'a>]> {
    let mut batches = Vec::new();
    let mut start = 0;
    while start < units.len() {
        let end = next_batch(units, start, cap);
        batches.push(&units[start..end]);
        start = end;
    }
    batches
}

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
    let mut sizer = BatchSizer::new();
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
        // Uploads run a few at a time: Drive's per-object latency (upload +
        // independent readback) dominates for small payloads, so sequential
        // publishing crawled at one object every several seconds. Packing
        // keeps that parallelism but multiplies each object's payload count,
        // turning the drain bandwidth-bound instead of round-trip-bound.
        // The durable record still goes through the single writer channel;
        // a backend failure anywhere in a batch ends the pass and backs off.
        // The in-flight budget adapts to the measured link rate: batches are
        // carved off a cursor so each one uses the budget current when it
        // starts.
        let units = publish_units(&pending);
        let mut any_error = false;
        let mut cursor = 0;
        while cursor < units.len() {
            if stop.load(Ordering::Acquire) {
                return;
            }
            let end = next_batch(&units, cursor, sizer.cap());
            let batch = &units[cursor..end];
            let batch_bytes = batch.iter().map(unit_bytes).fold(0u64, u64::saturating_add);
            let started = Instant::now();
            let results: Vec<Result<(), PublishFailure>> = std::thread::scope(|scope| {
                let workers: Vec<_> = batch
                    .iter()
                    .map(|unit| {
                        scope.spawn(move || {
                            publish_unit(db, *volume, journal_dir, provider, key, unit, stats)
                        })
                    })
                    .collect();
                workers
                    .into_iter()
                    .map(|worker| worker.join().unwrap_or(Err(PublishFailure::Backend)))
                    .collect()
            });
            if results
                .iter()
                .any(|result| matches!(result, Err(PublishFailure::Backend)))
            {
                sizer.record_failure();
                any_error = true;
                break;
            }
            sizer.record_success(batch_bytes, started.elapsed());
            cursor = end;
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

/// One encoded pack member and its byte offset inside the pack object.
struct EncodedMember<'a> {
    payload: &'a UnpublishedPayload,
    member_offset: u64,
    plaintext_length: u64,
    plaintext_hash: [u8; 32],
    frame_hashes: Vec<[u8; 32]>,
}

/// Reads, checksum-verifies, and encrypts one payload exactly as the single
/// publish path did. A member that fails returns `None` (counted in stats)
/// so one bad file cannot block the rest of its pack.
fn encode_member<'a>(
    volume: RepositoryId,
    journal_dir: &Path,
    key: &RepositoryKey,
    payload: &'a UnpublishedPayload,
    member_offset: u64,
    stats: &PublisherStats,
) -> Option<(EncodedMember<'a>, Vec<u8>)> {
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
            return None;
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
        return None;
    }
    let plaintext_hash = *blake3::hash(&plaintext).as_bytes();
    let (encoded, frame_hashes) =
        match encode_payload_object(key, volume, payload.payload_id, &plaintext) {
            Ok(encoded) => encoded,
            Err(_) => {
                stats.record_error("payload frame encode failed");
                return None;
            }
        };
    Some((
        EncodedMember {
            payload,
            member_offset,
            plaintext_length: plaintext.len() as u64,
            plaintext_hash,
            frame_hashes,
        },
        encoded,
    ))
}

/// Publishes one unit — a single payload (packed to member_offset 0) or a
/// pack of concatenated encoded payloads — as one provider object: upload,
/// identity check, independent readback, then one durable record covering
/// every member.
fn publish_unit(
    db: &Database,
    volume: RepositoryId,
    journal_dir: &Path,
    provider: &crate::handles::ManagedProvider,
    key: &RepositoryKey,
    unit: &PublishUnit<'_>,
    stats: &PublisherStats,
) -> Result<(), PublishFailure> {
    let mut object = Vec::new();
    let mut members = Vec::new();
    for payload in &unit.members {
        let Some((member, encoded)) = encode_member(
            volume,
            journal_dir,
            key,
            payload,
            object.len() as u64,
            stats,
        ) else {
            continue;
        };
        object.extend_from_slice(&encoded);
        members.push(member);
    }
    if members.is_empty() {
        return Err(PublishFailure::Integrity);
    }
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
            eprintln!("payload publish backend error: {class}: {error}");
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
    let published_ns = now_ns_i64_pub();
    let provider_object_id = reference.provider_object_id.as_str().to_owned();
    let immutable_revision = reference
        .immutable_revision
        .map(|revision| revision.as_str().to_owned());
    let records: Vec<mirage_db::payload_remote::PayloadRemoteObject> = members
        .iter()
        .map(|member| mirage_db::payload_remote::PayloadRemoteObject {
            volume_id: volume,
            payload_id: member.payload.payload_id,
            provider_object_id: provider_object_id.clone(),
            immutable_revision: immutable_revision.clone(),
            object_length: i64::try_from(object_length).unwrap_or(i64::MAX),
            object_hash,
            plaintext_length: i64::try_from(member.plaintext_length).unwrap_or(i64::MAX),
            plaintext_hash: member.plaintext_hash,
            frame_hashes: member.frame_hashes.clone(),
            published_ns,
            member_offset: i64::try_from(member.member_offset).unwrap_or(i64::MAX),
        })
        .collect();
    match db.writer().payloads_published(records) {
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
#[allow(clippy::too_many_arguments)]
pub fn evict_published(
    db: &Database,
    dirty: &crate::handles::DirtyLedger,
    journal_dir: &std::path::Path,
    volume: RepositoryId,
    handles: &mirage_engine::handles::HandleTable,
    payload_files: &std::sync::Mutex<crate::handles::PayloadFileCache>,
    pins: &std::collections::HashSet<mirage_types::InodeId>,
    target: u64,
) -> Result<(u64, u64), MirageError> {
    let evictable = db.published_payloads_evictable(volume)?;
    if !evictable.is_empty() {
        // Deleting a payload is irreversible — the committed state that
        // justifies it (the remote publication record) must be power-loss
        // durable first. One FULL barrier commit covers every earlier
        // NORMAL commit.
        db.durability_barrier()?;
    }
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
        crate::handles::invalidate_payload_file(payload_files, &payload_id);
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
    /// Shared immutable full frames for bursts and dense reads.
    frames: FrameCache,
    flights: FrameFlights,
    sectors: Mutex<ByteLru<SectorKey, [u8]>>,
    sector_flights: Mutex<std::collections::BTreeMap<SectorKey, Arc<FrameFlight>>>,
    /// Verification knowledge outlives bulk-data eviction.
    proofs: Mutex<ByteLru<FrameKey, FrameProofCatalog>>,
    fetch_gate: FetchGate,
    /// Bounded publication-record cache: payload_id → Option<record>.
    publications: PublicationCache,
    /// Payloads currently being re-staged to disk; dedupes concurrent reads
    /// so one cold open does one fetch+write.
    restaging: Mutex<std::collections::BTreeSet<[u8; 16]>>,
}

#[path = "remote_read_impl.rs"]
mod remote_read_impl;

type FrameCache = Mutex<ByteLru<FrameKey, [u8]>>;
type FrameFlights = Mutex<std::collections::BTreeMap<FrameKey, Arc<FrameFlight>>>;
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct FrameKey {
    payload: [u8; 16],
    index: u64,
    root: [u8; 32],
    length: u32,
    representation: [u8; 32],
    member_offset: u64,
}
type SectorKey = (FrameKey, usize);

/// Cache content/proof budgets; bookkeeping and active readers are additional.
#[derive(Debug, Clone, Copy)]
pub struct RemoteReadBudgets {
    pub frame_bytes: usize,
    pub sector_bytes: usize,
    pub proof_bytes: usize,
}
impl Default for RemoteReadBudgets {
    fn default() -> Self {
        Self {
            frame_bytes: 32 << 20,
            sector_bytes: 8 << 20,
            proof_bytes: 8 << 20,
        }
    }
}
#[derive(Debug, Clone, Copy)]
pub struct RemoteReadResidency {
    pub frame_bytes: usize,
    pub sector_bytes: usize,
    pub proof_bytes: usize,
}
type PublicationCache = Mutex<
    std::collections::HashMap<
        [u8; 16],
        Option<Arc<mirage_db::payload_remote::PayloadRemoteObject>>,
    >,
>;

const PUBLICATION_CACHE_LIMIT: usize = 4096;
/// Frames of one `read` fetched concurrently — hides a backend round trip
/// per frame (Drive latency dominates a cold read), bounded so a burst of
/// readers cannot stampede the backend.
const FRAME_FETCH_PARALLELISM: usize = 8;
/// Sub-range size when one frame fetch is split across connections. Some
/// links have low *per-connection* throughput to the provider (measured:
/// ~0.6 MiB/s per request against ~19 MiB/s aggregate); fetching a ~4.3 MiB
/// encoded frame as one request leaves most of the link idle.
const SUBRANGE_BYTES: u64 = 1024 * 1024;
/// Sub-range fetches in flight per frame. Combined with the 8-frame window
/// a foreground read tops out at 32 sockets — acceptable next to a running
/// game and far inside what a consumer NAT table tolerates.
const SUBRANGE_PARALLELISM: usize = 4;

struct FrameFlight {
    done: Mutex<Option<Result<Arc<[u8]>, FetchFailure>>>,
    ready: Condvar,
}

#[derive(Clone, Copy)]
enum FetchFailure {
    Backend,
    Integrity,
}

/// Global across readers: at most eight whole-frame or sector miss owners.
struct FetchGate {
    active: Mutex<(usize, usize)>,
    ready: Condvar,
}
struct FetchPermit<'a>(&'a FetchGate, bool);
impl FetchGate {
    fn acquire(&self, speculative: bool) -> Result<FetchPermit<'_>, FetchFailure> {
        let mut active = self.active.lock().map_err(|_| FetchFailure::Backend)?;
        while active.0 >= FRAME_FETCH_PARALLELISM || (speculative && active.1 >= 2) {
            active = self.ready.wait(active).map_err(|_| FetchFailure::Backend)?;
        }
        active.0 += 1;
        if speculative {
            active.1 += 1;
        }
        Ok(FetchPermit(self, speculative))
    }
}
impl Drop for FetchPermit<'_> {
    fn drop(&mut self) {
        if let Ok(mut active) = self.0.active.lock() {
            active.0 -= 1;
            if self.1 {
                active.1 -= 1;
            }
            self.0.ready.notify_all();
        }
    }
}

impl RemotePayloadStore {
    #[must_use]
    pub fn new(
        backend: Arc<dyn mirage_backend::ObjectBackend>,
        installed: Arc<AtomicBool>,
        key: Arc<RepositoryKey>,
        repository: RepositoryId,
    ) -> Self {
        Self::with_budgets(
            backend,
            installed,
            key,
            repository,
            RemoteReadBudgets::default(),
        )
    }

    #[must_use]
    pub fn with_budgets(
        backend: Arc<dyn mirage_backend::ObjectBackend>,
        installed: Arc<AtomicBool>,
        key: Arc<RepositoryKey>,
        repository: RepositoryId,
        budgets: RemoteReadBudgets,
    ) -> Self {
        Self {
            backend,
            installed,
            key,
            repository,
            frames: Mutex::new(ByteLru::new(budgets.frame_bytes)),
            flights: Mutex::new(std::collections::BTreeMap::new()),
            sectors: Mutex::new(ByteLru::new(budgets.sector_bytes)),
            sector_flights: Mutex::new(std::collections::BTreeMap::new()),
            proofs: Mutex::new(ByteLru::new(budgets.proof_bytes)),
            fetch_gate: FetchGate {
                active: Mutex::new((0, 0)),
                ready: Condvar::new(),
            },
            publications: Mutex::new(std::collections::HashMap::new()),
            restaging: Mutex::new(std::collections::BTreeSet::new()),
        }
    }

    pub fn residency(&self) -> Result<RemoteReadResidency, MirageError> {
        Ok(RemoteReadResidency {
            frame_bytes: self
                .frames
                .lock()
                .map_err(|_| MirageError::internal_invariant("remote cache lock poisoned"))?
                .used(),
            sector_bytes: self
                .sectors
                .lock()
                .map_err(|_| MirageError::internal_invariant("remote cache lock poisoned"))?
                .used(),
            proof_bytes: self
                .proofs
                .lock()
                .map_err(|_| MirageError::internal_invariant("remote cache lock poisoned"))?
                .used(),
        })
    }

    pub fn published_length(
        &self,
        db: &Database,
        volume: RepositoryId,
        payload_id: &[u8; 16],
    ) -> Result<Option<u64>, MirageError> {
        self.publication(db, volume, payload_id)?
            .map(|record| {
                u64::try_from(record.plaintext_length).map_err(|_| {
                    MirageError::integrity_mismatch("published payload length is negative")
                })
            })
            .transpose()
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
        self.read_inner(db, volume, payload_id, offset, length, None)
    }

    /// `read` for a background worker: `cancel` is polled between frame
    /// batches so engine shutdown can cut a whole-payload fetch short.
    pub fn read_cancelable(
        &self,
        db: &Database,
        volume: RepositoryId,
        payload_id: &[u8; 16],
        offset: u64,
        length: usize,
        cancel: &AtomicBool,
    ) -> Result<Option<Vec<u8>>, MirageError> {
        self.read_inner(db, volume, payload_id, offset, length, Some(cancel))
    }

    /// Full verified plaintext of an evicted payload plus its record, for
    /// re-staging the local file. `None` when no publication row exists.
    /// `cancel`, when set, cuts the fetch between frame batches.
    pub fn fetch_whole(
        &self,
        db: &Database,
        volume: RepositoryId,
        payload_id: &[u8; 16],
        cancel: Option<&AtomicBool>,
    ) -> Result<Option<WholePayload>, MirageError> {
        let Some(record) = self.publication(db, volume, payload_id)? else {
            return Ok(None);
        };
        let length = usize::try_from(record.plaintext_length)
            .map_err(|_| MirageError::integrity_mismatch("published payload length overflows"))?;
        let bytes = self
            .read_inner(db, volume, payload_id, 0, length, cancel)?
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

/// Bounded background re-stage pool: at most `RESTAGE_WORKERS`
/// whole-payload fetches run per engine, so a cold-read burst keeps its
/// latency win without competing head-on with the foreground reader or the
/// publisher. Queued work past `RESTAGE_QUEUE_LIMIT` drops silently — the
/// next cold read schedules it again. Best-effort by contract: failures
/// live and die with the task itself.
pub struct RestagePool {
    inner: Mutex<RestagePoolState>,
    wake: Condvar,
    /// Engine shutdown: workers exit at the next job boundary and an
    /// in-flight fetch polls the flag between frame batches.
    stop: Arc<AtomicBool>,
}

struct RestagePoolState {
    queue: std::collections::VecDeque<RestageJob>,
    /// Queued-but-not-started payload ids; the restage slot dedupes running
    /// work, this set keeps the queue itself bounded and unique.
    queued: std::collections::BTreeSet<[u8; 16]>,
    running: usize,
}

struct RestageJob {
    payload_id: [u8; 16],
    task: Box<dyn FnOnce(&AtomicBool) + Send>,
}

const RESTAGE_WORKERS: usize = 2;
const RESTAGE_QUEUE_LIMIT: usize = 64;

impl Default for RestagePool {
    fn default() -> Self {
        Self::new()
    }
}

impl RestagePool {
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(RestagePoolState {
                queue: std::collections::VecDeque::new(),
                queued: std::collections::BTreeSet::new(),
                running: 0,
            }),
            wake: Condvar::new(),
            stop: Arc::new(AtomicBool::new(false)),
        }
    }

    /// The shared cancellation flag for tasks that poll mid-fetch.
    pub fn stop_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.stop)
    }

    /// Queues `task` for `payload_id`; `false` when the payload is already
    /// queued, the queue is full, or the pool is stopping — callers that do
    /// not care may ignore it (a later read reschedules).
    pub fn submit(
        self: &Arc<Self>,
        payload_id: [u8; 16],
        task: impl FnOnce(&AtomicBool) + Send + 'static,
    ) -> bool {
        if self.stop.load(Ordering::Acquire) {
            return false;
        }
        let mut state = match self.inner.lock() {
            Ok(state) => state,
            Err(_) => return false,
        };
        if !state.queued.insert(payload_id) {
            return false;
        }
        if state.queue.len() >= RESTAGE_QUEUE_LIMIT {
            state.queued.remove(&payload_id);
            return false;
        }
        state.queue.push_back(RestageJob {
            payload_id,
            task: Box::new(task),
        });
        if state.running < RESTAGE_WORKERS {
            // Reserve the slot under the lock so a failed spawn can hand it
            // back deterministically.
            state.running += 1;
            let pool = Arc::clone(self);
            drop(state);
            match std::thread::Builder::new()
                .name("mirage-restage".to_owned())
                .spawn(move || pool.worker())
            {
                Ok(_) => {}
                Err(_) => {
                    if let Ok(mut state) = self.inner.lock() {
                        state.running = state.running.saturating_sub(1);
                    }
                }
            }
        } else {
            drop(state);
            self.wake.notify_one();
        }
        true
    }

    /// Stop scheduling and wake workers. Workers are detached: everything a
    /// job touches is independently owned (Arc/Database), so engine destroy
    /// never blocks on a fetch already in flight — it only lets the pool
    /// wind down.
    pub fn shutdown(&self) {
        self.stop.store(true, Ordering::Release);
        self.wake.notify_all();
    }

    fn worker(self: &Arc<Self>) {
        loop {
            let job = {
                let mut state = match self.inner.lock() {
                    Ok(state) => state,
                    Err(_) => return,
                };
                loop {
                    if let Some(job) = state.queue.pop_front() {
                        state.queued.remove(&job.payload_id);
                        break job;
                    }
                    if self.stop.load(Ordering::Acquire) {
                        state.running = state.running.saturating_sub(1);
                        return;
                    }
                    state = match self.wake.wait(state) {
                        Ok(state) => state,
                        Err(_) => return,
                    };
                }
            };
            let cancel = Arc::clone(&self.stop);
            (job.task)(&cancel);
        }
    }
}

/// Fetches one backend range. Ranges larger than `SUBRANGE_BYTES` are split
/// into sub-ranges fetched `SUBRANGE_PARALLELISM` at a time so a
/// per-connection-limited link overlaps them; parts are concatenated in
/// order and the total length must match exactly.
fn fetch_remote_range(
    backend: &Arc<dyn mirage_backend::ObjectBackend>,
    reference: &mirage_backend::RemoteObjectRef,
    range: mirage_types::CheckedRange,
    class: mirage_backend::FetchClass,
) -> Result<Vec<u8>, FetchFailure> {
    if range.len() <= SUBRANGE_BYTES {
        return fetch_remote_part(backend, reference, range, class);
    }
    // Balance one frame over at most four requests. A 4 MiB frame's 68-byte
    // envelope must not become a fifth, serialized round trip.
    let part_count = range
        .len()
        .div_ceil(SUBRANGE_BYTES)
        .min(SUBRANGE_PARALLELISM as u64);
    let part_bytes = range.len().div_ceil(part_count);
    let mut parts: Vec<mirage_types::CheckedRange> = Vec::new();
    let mut cursor = range.start();
    let end = range.start() + range.len();
    while cursor < end {
        let length = part_bytes.min(end - cursor);
        let Some(part) = mirage_types::CheckedRange::try_new(cursor, length) else {
            return Err(FetchFailure::Integrity);
        };
        parts.push(part);
        cursor += length;
    }
    let mut pending = parts.iter();
    let mut bytes = Vec::with_capacity(range.len() as usize);
    loop {
        let batch: Vec<mirage_types::CheckedRange> = pending
            .by_ref()
            .take(SUBRANGE_PARALLELISM)
            .copied()
            .collect();
        if batch.is_empty() {
            break;
        }
        let results: Vec<Result<Vec<u8>, FetchFailure>> = std::thread::scope(|scope| {
            let spawned: Vec<_> = batch
                .iter()
                .map(|part| {
                    scope.spawn(move || fetch_remote_part(backend, reference, *part, class))
                })
                .collect();
            spawned
                .into_iter()
                .map(|thread| thread.join().unwrap_or(Err(FetchFailure::Backend)))
                .collect()
        });
        for result in results {
            bytes.extend_from_slice(&result?);
        }
    }
    if bytes.len() as u64 != range.len() {
        return Err(FetchFailure::Integrity);
    }
    Ok(bytes)
}

/// One `read_range` + stream collect; typed backend failure only — callers
/// compare the length.
fn fetch_remote_part(
    backend: &Arc<dyn mirage_backend::ObjectBackend>,
    reference: &mirage_backend::RemoteObjectRef,
    range: mirage_types::CheckedRange,
    class: mirage_backend::FetchClass,
) -> Result<Vec<u8>, FetchFailure> {
    let read = futures_executor::block_on(backend.read_range(
        reference,
        range,
        class,
        CancellationToken::new(),
    ))
    .map_err(|_| FetchFailure::Backend)?;
    if read.requested_range != range
        || read.received_length.as_u64() != range.len()
        || (reference.immutable_revision.is_some()
            && read.metadata.observed_revision.is_some()
            && reference.immutable_revision != read.metadata.observed_revision)
    {
        return Err(FetchFailure::Integrity);
    }
    futures_executor::block_on(collect_stream(read.stream, range.len()))
}

async fn collect_stream(
    mut stream: mirage_backend::BackendByteStream,
    expected: u64,
) -> Result<Vec<u8>, FetchFailure> {
    use futures_util::StreamExt;
    let mut out = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| FetchFailure::Backend)?;
        if (out.len() as u64).saturating_add(chunk.len() as u64) > expected {
            return Err(FetchFailure::Integrity);
        }
        out.extend_from_slice(&chunk);
    }
    if out.len() as u64 != expected {
        return Err(FetchFailure::Integrity);
    }
    Ok(out)
}

#[cfg(test)]
mod batch_tests {
    use super::*;

    fn payload(bytes: i64) -> UnpublishedPayload {
        UnpublishedPayload {
            payload_id: [0; 16],
            path: String::new(),
            bytes,
            checksum: None,
            device_seq: 0,
        }
    }

    fn unit(payload: &UnpublishedPayload) -> PublishUnit<'_> {
        PublishUnit {
            members: vec![payload],
        }
    }

    #[test]
    fn units_pack_small_members_and_isolate_large() {
        // 40 x 1 MiB → 16 MiB packs → 16-member units then a partial tail.
        let small: Vec<_> = (0..40).map(|_| payload(1 << 20)).collect();
        let sizes: Vec<usize> = publish_units(&small)
            .iter()
            .map(|u| u.members.len())
            .collect();
        assert_eq!(sizes, vec![16, 16, 8]);
        // A >4 MiB payload stays a single in pending order.
        let mut mixed: Vec<_> = (0..4).map(|_| payload(1 << 20)).collect();
        mixed.push(payload(8 << 20));
        mixed.extend((0..4).map(|_| payload(1 << 20)));
        let sizes: Vec<usize> = publish_units(&mixed)
            .iter()
            .map(|u| u.members.len())
            .collect();
        assert_eq!(sizes, vec![4, 1, 4]);
    }

    #[test]
    fn batches_bound_count_and_bytes() {
        // The previous fixed cap is today's floor: the MIN-cap behaviour must
        // be identical to before.
        let small: Vec<_> = (0..40).map(|_| payload(1 << 20)).collect();
        let units: Vec<_> = small.iter().map(unit).collect();
        let sizes: Vec<usize> = publish_batches(&units, PUBLISH_BATCH_BYTES)
            .iter()
            .map(|b| b.len())
            .collect();
        assert_eq!(sizes, vec![16, 16, 8]);
        let large: Vec<_> = (0..20).map(|_| payload(32 << 20)).collect();
        let units: Vec<_> = large.iter().map(unit).collect();
        let sizes: Vec<usize> = publish_batches(&units, PUBLISH_BATCH_BYTES)
            .iter()
            .map(|b| b.len())
            .collect();
        assert_eq!(sizes, vec![2; 10]);
        let huge = [payload(1 << 30)];
        let units: Vec<_> = huge.iter().map(unit).collect();
        assert_eq!(
            publish_batches(&units, PUBLISH_BATCH_BYTES).len(),
            1,
            "an oversized payload still publishes"
        );
        // A pack counts its members' bytes against the batch budget.
        let members: Vec<_> = (0..4).map(|_| payload(16 << 20)).collect();
        let packed = PublishUnit {
            members: members.iter().collect(),
        };
        assert_eq!(unit_bytes(&packed), 64 << 20);
    }

    #[test]
    fn batch_sizer_holds_min_on_slow_link_grows_2x_to_max_and_resets() {
        let mut sizer = BatchSizer::new();
        assert_eq!(sizer.cap(), PUBLISH_BATCH_BYTES);
        // A slow link: 64 MiB in 45 s lands exactly on the target, so the
        // cap never leaves the floor.
        for _ in 0..8 {
            sizer.record_success(PUBLISH_BATCH_BYTES, BATCH_TARGET);
        }
        assert_eq!(sizer.cap(), PUBLISH_BATCH_BYTES);
        // A fast link: the target saturates at MAX but growth is 2x a step.
        let mut expected = PUBLISH_BATCH_BYTES;
        for _ in 0..5 {
            sizer.record_success(PUBLISH_BATCH_BYTES, Duration::from_secs(1));
            expected = expected.saturating_mul(2).min(PUBLISH_BATCH_MAX_BYTES);
            assert_eq!(sizer.cap(), expected);
        }
        assert_eq!(sizer.cap(), PUBLISH_BATCH_MAX_BYTES);
        sizer.record_failure();
        assert_eq!(sizer.cap(), PUBLISH_BATCH_BYTES);
    }

    #[test]
    fn dynamic_batching_respects_unit_and_byte_caps() {
        // Parallelism still bounds units per batch whatever the byte cap.
        let small: Vec<_> = (0..17).map(|_| payload(1 << 20)).collect();
        let units: Vec<_> = small.iter().map(unit).collect();
        let sizes: Vec<usize> = publish_batches(&units, PUBLISH_BATCH_MAX_BYTES)
            .iter()
            .map(|b| b.len())
            .collect();
        assert_eq!(sizes, vec![16, 1]);
        // A grown cap batches more bytes: 32 MiB units go 3-up under 96 MiB
        // where the floor paired them.
        let large: Vec<_> = (0..8).map(|_| payload(32 << 20)).collect();
        let units: Vec<_> = large.iter().map(unit).collect();
        let sizes: Vec<usize> = publish_batches(&units, 96 << 20)
            .iter()
            .map(|b| b.len())
            .collect();
        assert_eq!(sizes, vec![3, 3, 2]);
        // Oversized units still form their own batch at any cap.
        let huge = [payload(1 << 30)];
        let units: Vec<_> = huge.iter().map(unit).collect();
        assert_eq!(next_batch(&units, 0, PUBLISH_BATCH_BYTES), 1);
    }
}
