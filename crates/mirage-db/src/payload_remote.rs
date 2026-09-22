//! Publication ledger for managed-volume journal payloads: each committed
//! payload file may be uploaded as one immutable encrypted remote object.
//! The row records the remote identity plus the verification material
//! (encrypted-object hash, plaintext hash, per-frame hashes) needed to fetch
//! and authenticate the payload after local eviction.

use mirage_types::{MirageError, RepositoryId};
use rusqlite::{Connection, OptionalExtension, params};

use crate::Database;
use crate::error::sqlite;

/// Physical-ledger file id for a managed volume's journal payloads — shared
/// between the host (which registers the file) and the service (status).
pub const MANAGED_JOURNAL_FILE_ID: [u8; 16] = [0x4a; 16];

/// One payload's remote publication identity and verification material.
#[derive(Debug, Clone)]
pub struct PayloadRemoteObject {
    pub volume_id: RepositoryId,
    pub payload_id: [u8; 16],
    /// Backend object name (e.g. Drive file id).
    pub provider_object_id: String,
    pub immutable_revision: Option<String>,
    /// Encrypted object byte length.
    pub object_length: i64,
    /// blake3 of the encrypted object bytes.
    pub object_hash: [u8; 32],
    /// Plaintext payload byte length.
    pub plaintext_length: i64,
    /// blake3 of the plaintext payload.
    pub plaintext_hash: [u8; 32],
    /// Concatenated blake3 hashes, one per 4 MiB plaintext frame.
    pub frame_hashes: Vec<[u8; 32]>,
    pub published_ns: i64,
}

/// A committed, still-referenced payload that has no remote object yet.
#[derive(Debug, Clone)]
pub struct UnpublishedPayload {
    pub payload_id: [u8; 16],
    /// Journal-relative payload file name (`<hex>.payload`).
    pub path: String,
    pub bytes: i64,
    /// blake3 checksum recorded when the payload was staged; publish refuses
    /// to upload bytes that disagree with it.
    pub checksum: Option<[u8; 32]>,
    /// Device order of the owning operation — publish order follows it.
    pub device_seq: i64,
}

fn blob16(bytes: &[u8]) -> Result<[u8; 16], MirageError> {
    bytes
        .try_into()
        .map_err(|_| MirageError::integrity_mismatch("identifier blob is not 16 bytes"))
}

fn blob32(bytes: &[u8]) -> Result<[u8; 32], MirageError> {
    bytes
        .try_into()
        .map_err(|_| MirageError::integrity_mismatch("hash blob is not 32 bytes"))
}

fn decode_record(row: &rusqlite::Row<'_>) -> Result<PayloadRemoteObject, rusqlite::Error> {
    let volume: Vec<u8> = row.get(0)?;
    let payload: Vec<u8> = row.get(1)?;
    let object_hash: Vec<u8> = row.get(5)?;
    let plaintext_hash: Vec<u8> = row.get(7)?;
    let frames: Vec<u8> = row.get(8)?;
    if !frames.len().is_multiple_of(32) {
        return Err(rusqlite::Error::IntegralValueOutOfRange(
            8,
            frames.len() as i64,
        ));
    }
    Ok(PayloadRemoteObject {
        volume_id: RepositoryId::from_bytes(
            volume
                .as_slice()
                .try_into()
                .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, 16))?,
        ),
        payload_id: payload
            .as_slice()
            .try_into()
            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(1, 16))?,
        provider_object_id: row.get(2)?,
        immutable_revision: row.get(3)?,
        object_length: row.get(4)?,
        object_hash: blob32(&object_hash)
            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(5, 32))?,
        plaintext_length: row.get(6)?,
        plaintext_hash: blob32(&plaintext_hash)
            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(7, 32))?,
        frame_hashes: frames
            .chunks_exact(32)
            .map(|chunk| chunk.try_into().expect("chunk is 32 bytes"))
            .collect(),
        published_ns: row.get(9)?,
    })
}

const RECORD_COLUMNS: &str =
    "volume_id, payload_id, provider_object_id, immutable_revision, object_length,
     object_hash, plaintext_length, plaintext_hash, frame_hashes, published_ns";

/// Records a payload's remote publication. Fails when the payload is no
/// longer referenced by any byte extent — the uploaded object is then an
/// orphan the caller should delete best-effort.
pub fn record_payload_publication(
    connection: &mut Connection,
    record: &PayloadRemoteObject,
) -> Result<(), MirageError> {
    let transaction = connection
        .transaction()
        .map_err(|e| sqlite(e, "payload publication begin failed"))?;
    let referenced: Option<i64> = transaction
        .query_row(
            "SELECT 1 FROM byte_extents
             WHERE volume_id = ?1 AND payload_id = ?2 LIMIT 1",
            params![
                record.volume_id.as_bytes().as_slice(),
                record.payload_id.as_slice()
            ],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| sqlite(e, "payload publication reference check failed"))?;
    if referenced.is_none() {
        return Err(MirageError::repository_conflict(
            "published payload is no longer referenced",
        ));
    }
    let mut frame_blob = Vec::with_capacity(record.frame_hashes.len() * 32);
    for hash in &record.frame_hashes {
        frame_blob.extend_from_slice(hash);
    }
    transaction
        .execute(
            "INSERT INTO payload_remote_objects
             (volume_id, payload_id, provider_object_id, immutable_revision,
              object_length, object_hash, plaintext_length, plaintext_hash,
              frame_hashes, published_ns)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(payload_id) DO UPDATE SET
                provider_object_id = excluded.provider_object_id,
                immutable_revision = excluded.immutable_revision,
                object_length = excluded.object_length,
                object_hash = excluded.object_hash,
                plaintext_length = excluded.plaintext_length,
                plaintext_hash = excluded.plaintext_hash,
                frame_hashes = excluded.frame_hashes,
                published_ns = excluded.published_ns",
            params![
                record.volume_id.as_bytes().as_slice(),
                record.payload_id.as_slice(),
                record.provider_object_id,
                record.immutable_revision,
                record.object_length,
                record.object_hash.as_slice(),
                record.plaintext_length,
                record.plaintext_hash.as_slice(),
                frame_blob,
                record.published_ns,
            ],
        )
        .map_err(|e| sqlite(e, "payload publication insert failed"))?;
    transaction
        .commit()
        .map_err(|e| sqlite(e, "payload publication commit failed"))
}

/// The remote identity of one published payload, when it exists.
pub fn payload_publication(
    connection: &Connection,
    volume_id: RepositoryId,
    payload_id: &[u8; 16],
) -> Result<Option<PayloadRemoteObject>, MirageError> {
    connection
        .query_row(
            &format!(
                "SELECT {RECORD_COLUMNS} FROM payload_remote_objects
             WHERE volume_id = ?1 AND payload_id = ?2"
            ),
            params![volume_id.as_bytes().as_slice(), payload_id.as_slice()],
            decode_record,
        )
        .optional()
        .map_err(|e| sqlite(e, "payload publication lookup failed"))
}

/// Committed payloads still referenced by extents but never published, in
/// device order.
pub fn unpublished_payloads(
    connection: &Connection,
    volume_id: RepositoryId,
) -> Result<Vec<UnpublishedPayload>, MirageError> {
    let mut statement = connection
        .prepare(
            "SELECT p.payload_id, p.path, p.bytes, p.checksum, o.device_seq
             FROM operation_payloads p
             JOIN local_operations o ON o.operation_id = p.operation_id
             WHERE o.volume_id = ?1
               AND o.status IN ('committed', 'flushed', 'published')
               AND p.flushed_ns IS NOT NULL
               AND EXISTS (SELECT 1 FROM byte_extents e
                            WHERE e.volume_id = o.volume_id
                              AND e.payload_id = p.payload_id)
               AND NOT EXISTS (SELECT 1 FROM payload_remote_objects r
                               WHERE r.payload_id = p.payload_id)
               -- A dead physical extent means the journal file was
               -- intentionally released (delete/supersede/compaction):
               -- there is nothing left to upload, and retrying it forever
               -- starves real publications.
               AND NOT EXISTS (SELECT 1 FROM physical_extents x
                               WHERE x.extent_id = p.payload_id
                                 AND x.state = 'dead')
             ORDER BY o.device_seq",
        )
        .map_err(|e| sqlite(e, "unpublished payload scan prepare failed"))?;
    let rows = statement
        .query_map([volume_id.as_bytes().as_slice()], |row| {
            Ok((
                row.get::<_, Vec<u8>>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Option<Vec<u8>>>(3)?,
                row.get::<_, i64>(4)?,
            ))
        })
        .map_err(|e| sqlite(e, "unpublished payload scan failed"))?;
    let mut out = Vec::new();
    for row in rows {
        let (payload, path, bytes, checksum, device_seq) =
            row.map_err(|e| sqlite(e, "unpublished payload row decode failed"))?;
        out.push(UnpublishedPayload {
            payload_id: blob16(&payload)?,
            path,
            bytes,
            checksum: checksum.map(|bytes| blob32(&bytes)).transpose()?,
            device_seq,
        });
    }
    Ok(out)
}

/// Published payloads whose local copy may be evicted, oldest publication
/// first. Only payloads still referenced by extents are listed — anything
/// superseded is already dead.
pub fn published_payloads_evictable(
    connection: &Connection,
    volume_id: RepositoryId,
) -> Result<Vec<([u8; 16], i64, i64)>, MirageError> {
    let mut statement = connection
        .prepare(
            "SELECT r.payload_id, r.plaintext_length, r.published_ns
             FROM payload_remote_objects r
             JOIN physical_extents x ON x.extent_id = r.payload_id
             WHERE r.volume_id = ?1 AND x.state = 'alive' AND x.pin_count = 0
               AND EXISTS (SELECT 1 FROM byte_extents e
                            WHERE e.volume_id = r.volume_id
                              AND e.payload_id = r.payload_id)
             ORDER BY r.published_ns",
        )
        .map_err(|e| sqlite(e, "evictable payload scan prepare failed"))?;
    let rows = statement
        .query_map([volume_id.as_bytes().as_slice()], |row| {
            Ok((
                row.get::<_, Vec<u8>>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .map_err(|e| sqlite(e, "evictable payload scan failed"))?;
    let mut out = Vec::new();
    for row in rows {
        let (payload, length, published) =
            row.map_err(|e| sqlite(e, "evictable payload row decode failed"))?;
        out.push((blob16(&payload)?, length, published));
    }
    Ok(out)
}

/// Aggregate publication counters for status reporting. Counts payloads
/// still referenced by extents only — superseded payloads are excluded.
#[derive(Debug, Default, Clone, Copy)]
pub struct PayloadPublicationStats {
    pub pending_payloads: u64,
    pub pending_bytes: u64,
    pub published_payloads: u64,
    /// Plaintext bytes whose local copy is published and still resident.
    pub published_bytes: u64,
    /// Published payloads whose local copy was evicted.
    pub evicted_payloads: u64,
    /// Journal-file id used to join the physical extent ledger.
    /// Filled by the caller; `journal_file_id` identifies payload extents.
    pub _marker: (),
}

/// Sums over payloads: `pending` = referenced, unpublished; `published` =
/// referenced, published; `evicted` = published and its physical extent is
/// dead (local copy released).
pub fn payload_publication_stats(
    connection: &Connection,
    volume_id: RepositoryId,
    journal_file_id: &[u8; 16],
) -> Result<PayloadPublicationStats, MirageError> {
    let pending: (i64, Option<i64>) = connection
        .query_row(
            "SELECT count(*), sum(p.bytes)
             FROM operation_payloads p
             JOIN local_operations o ON o.operation_id = p.operation_id
             WHERE o.volume_id = ?1
               AND o.status IN ('committed', 'flushed', 'published')
               AND p.flushed_ns IS NOT NULL
               AND EXISTS (SELECT 1 FROM byte_extents e
                            WHERE e.volume_id = o.volume_id
                              AND e.payload_id = p.payload_id)
               AND NOT EXISTS (SELECT 1 FROM payload_remote_objects r
                               WHERE r.payload_id = p.payload_id)
               AND NOT EXISTS (SELECT 1 FROM physical_extents x
                               WHERE x.extent_id = p.payload_id
                                 AND x.state = 'dead')",
            [volume_id.as_bytes().as_slice()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|e| sqlite(e, "pending payload stats failed"))?;
    let published: (i64, Option<i64>) = connection
        .query_row(
            "SELECT count(*), sum(r.plaintext_length)
             FROM payload_remote_objects r
             WHERE r.volume_id = ?1
               AND EXISTS (SELECT 1 FROM byte_extents e
                            WHERE e.volume_id = r.volume_id
                              AND e.payload_id = r.payload_id)",
            [volume_id.as_bytes().as_slice()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|e| sqlite(e, "published payload stats failed"))?;
    let evicted: i64 = connection
        .query_row(
            "SELECT count(*)
             FROM payload_remote_objects r
             JOIN physical_extents x ON x.extent_id = r.payload_id
             WHERE r.volume_id = ?1 AND x.file_id = ?2 AND x.state = 'dead'
               AND EXISTS (SELECT 1 FROM byte_extents e
                            WHERE e.volume_id = r.volume_id
                              AND e.payload_id = r.payload_id)",
            params![volume_id.as_bytes().as_slice(), journal_file_id.as_slice()],
            |row| row.get(0),
        )
        .map_err(|e| sqlite(e, "evicted payload stats failed"))?;
    Ok(PayloadPublicationStats {
        pending_payloads: pending.0.max(0) as u64,
        pending_bytes: pending.1.unwrap_or(0).max(0) as u64,
        published_payloads: published.0.max(0) as u64,
        published_bytes: published.1.unwrap_or(0).max(0) as u64,
        evicted_payloads: evicted.max(0) as u64,
        _marker: (),
    })
}

/// Inodes whose current extent set references the payload (eviction skips
/// payloads an open handle may be reading).
pub fn payload_inodes(
    connection: &Connection,
    volume_id: RepositoryId,
    payload_id: &[u8; 16],
) -> Result<Vec<mirage_types::InodeId>, MirageError> {
    let mut statement = connection
        .prepare(
            "SELECT DISTINCT inode FROM byte_extents
             WHERE volume_id = ?1 AND payload_id = ?2",
        )
        .map_err(|e| sqlite(e, "payload inode scan prepare failed"))?;
    let rows = statement
        .query_map(
            params![volume_id.as_bytes().as_slice(), payload_id.as_slice()],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .map_err(|e| sqlite(e, "payload inode scan failed"))?;
    let mut out = Vec::new();
    for row in rows {
        let inode = row.map_err(|e| sqlite(e, "payload inode row decode failed"))?;
        out.push(mirage_types::InodeId::from_bytes(blob16(&inode)?));
    }
    Ok(out)
}

impl Database {
    /// Remote identity of one published payload.
    pub fn payload_publication(
        &self,
        volume_id: RepositoryId,
        payload_id: &[u8; 16],
    ) -> Result<Option<PayloadRemoteObject>, MirageError> {
        self.reads()
            .with_connection(|connection| payload_publication(connection, volume_id, payload_id))
    }

    /// Committed payloads still referenced by extents but never published.
    pub fn unpublished_payloads(
        &self,
        volume_id: RepositoryId,
    ) -> Result<Vec<UnpublishedPayload>, MirageError> {
        self.reads()
            .with_connection(|connection| unpublished_payloads(connection, volume_id))
    }

    /// Published payloads evictable oldest-first.
    pub fn published_payloads_evictable(
        &self,
        volume_id: RepositoryId,
    ) -> Result<Vec<([u8; 16], i64, i64)>, MirageError> {
        self.reads()
            .with_connection(|connection| published_payloads_evictable(connection, volume_id))
    }

    /// Inodes referencing a payload in their current extent sets.
    pub fn payload_inodes(
        &self,
        volume_id: RepositoryId,
        payload_id: &[u8; 16],
    ) -> Result<Vec<mirage_types::InodeId>, MirageError> {
        self.reads()
            .with_connection(|connection| payload_inodes(connection, volume_id, payload_id))
    }

    /// Publication counters for status reporting.
    pub fn payload_publication_stats(
        &self,
        volume_id: RepositoryId,
        journal_file_id: &[u8; 16],
    ) -> Result<PayloadPublicationStats, MirageError> {
        self.reads().with_connection(|connection| {
            payload_publication_stats(connection, volume_id, journal_file_id)
        })
    }
}
