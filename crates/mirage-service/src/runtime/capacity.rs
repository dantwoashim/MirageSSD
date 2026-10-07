//! Drive capacity snapshots and advertised-space bookkeeping.

use super::fs_util::read_json_bounded;
use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct VolumeCapacitySnapshot {
    pub(super) format_version: u32,
    pub(super) repository_id: RepositoryId,
    provider: String,
    pub(super) limit_bytes: Option<u64>,
    pub(super) usage_bytes: u64,
    observed_at_ns: i64,
}

pub(super) fn save_drive_capacity(
    database: &Database,
    repository_id: RepositoryId,
    quota: DriveQuotaSnapshot,
) -> Result<(), MirageError> {
    write_json_atomic(
        &repository_state_root(database, repository_id)?.join("volume-capacity.json"),
        &VolumeCapacitySnapshot {
            format_version: 1,
            repository_id,
            provider: "google-drive".to_owned(),
            limit_bytes: quota.limit_bytes,
            usage_bytes: quota.usage_bytes,
            observed_at_ns: now_ns(),
        },
    )
}

/// Largest capacity advertised for an unlimited Drive plan (1 PiB).
const UNLIMITED_DRIVE_ADVERTISED_BYTES: u64 = 1 << 50;

/// Records the account's current Drive quota for a Drive-backed repository so
/// its mounted volume advertises cloud capacity rather than the local budget.
pub fn refresh_drive_capacity(
    database: &Database,
    repository_id: RepositoryId,
    access_token: &str,
) -> Result<(), MirageError> {
    let native = Arc::new(NativeHttpTransport::new()?);
    let retrying = RetryingHttpTransport::new(native, 3)?;
    let quota = futures_executor::block_on(mirage_backend_drive::quota::storage_quota(
        &retrying,
        access_token,
    ))
    .map_err(MirageError::from)?;
    let snapshot = DriveQuotaSnapshot {
        limit_bytes: quota.limit,
        usage_bytes: quota.usage,
    };
    snapshot.validate()?;
    save_drive_capacity(database, repository_id, snapshot)
}

/// `(total, free)` from the last recorded Drive quota, if one exists.
/// Unlimited plans advertise 1 PiB minus current usage.
pub fn drive_capacity_snapshot(
    database: &Database,
    repository_id: RepositoryId,
) -> Result<Option<(u64, u64)>, MirageError> {
    let path = repository_state_root(database, repository_id)?.join("volume-capacity.json");
    if !path.exists() {
        return Ok(None);
    }
    let snapshot: VolumeCapacitySnapshot =
        read_json_bounded(&path, 64 * 1024, "volume capacity snapshot")?;
    if snapshot.format_version != 1
        || snapshot.repository_id != repository_id
        || snapshot.provider != "google-drive"
    {
        return Ok(None);
    }
    let total = snapshot
        .limit_bytes
        .unwrap_or(UNLIMITED_DRIVE_ADVERTISED_BYTES)
        .max(1);
    Ok(Some((total, total.saturating_sub(snapshot.usage_bytes))))
}

pub fn volume_capacity(
    database: &Database,
    repository_id: RepositoryId,
    index_path: &Path,
) -> Result<(u64, u64), MirageError> {
    let path = repository_state_root(database, repository_id)?.join("volume-capacity.json");
    if path.exists() {
        let snapshot: VolumeCapacitySnapshot =
            read_json_bounded(&path, 64 * 1024, "volume capacity snapshot")?;
        if snapshot.format_version != 1
            || snapshot.repository_id != repository_id
            || snapshot.provider != "google-drive"
            || snapshot
                .limit_bytes
                .is_some_and(|limit| limit == 0 || snapshot.usage_bytes > limit)
        {
            return Err(MirageError::integrity_mismatch(
                "volume capacity snapshot is invalid",
            ));
        }
        if let Some(limit) = snapshot.limit_bytes {
            return Ok((limit, limit.saturating_sub(snapshot.usage_bytes)));
        }
    }
    let index = MountIndex::open(index_path)?;
    let mut logical_bytes = 0_u64;
    for ordinal in 0..index.file_count() {
        let ordinal = u32::try_from(ordinal)
            .map_err(|_| MirageError::unsupported_layout("mount index exceeds u32 files"))?;
        logical_bytes = logical_bytes
            .checked_add(index.file_by_index(ordinal)?.logical_size())
            .ok_or_else(|| MirageError::unsupported_layout("volume size overflows u64"))?;
    }
    Ok((logical_bytes.max(1), 0))
}
