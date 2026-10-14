//! Exclusive, journaled cleanup of owned chunks after the filesystem drains.
//! Cleanup is never authorized by launcher exit or a PID alone. A lifecycle
//! controller first stops its exact host, then this owner lock proves exclusion.
use super::*;
use std::collections::HashSet;
pub fn allocation_unit(root: &Path) -> Result<u64, MirageStatus> {
    cache::allocation_unit(root)
}

/// Data allocation rather than advertised length. Compressed/sparse files use
/// the filesystem's compressed-size query; ordinary files include allocation
/// unit rounding through FILE_STANDARD_INFO. Directory/MFT overhead is covered
/// by the separately admitted free-space floor, not fabricated as file bytes.
#[cfg(windows)]
pub fn allocated_bytes(path: &Path) -> Result<u64, MirageStatus> {
    use std::os::windows::{ffi::OsStrExt, fs::MetadataExt, io::AsRawHandle};
    use windows_sys::Win32::{
        Foundation::{GetLastError, SetLastError},
        Storage::FileSystem::{
            FILE_STANDARD_INFO, FileStandardInfo, GetCompressedFileSizeW,
            GetFileInformationByHandleEx,
        },
    };
    let file = File::open(path).map_err(|_| MirageStatus::IoError)?;
    let meta = file.metadata().map_err(|_| MirageStatus::IoError)?;
    if meta.file_attributes() & (0x800 | 0x200) != 0 {
        let wide: Vec<_> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut high = 0;
        // SAFETY: valid NUL path and writable output; last-error disambiguates
        // the valid low-word 0xffffffff result.
        let low = unsafe {
            SetLastError(0);
            GetCompressedFileSizeW(wide.as_ptr(), &mut high)
        };
        if low == u32::MAX && unsafe { GetLastError() } != 0 {
            return Err(MirageStatus::IoError);
        }
        return Ok((u64::from(high) << 32) | u64::from(low));
    }
    let mut info: FILE_STANDARD_INFO = unsafe { std::mem::zeroed() };
    // SAFETY: owned file handle, correctly sized writable standard-info buffer.
    if unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileStandardInfo,
            (&mut info as *mut FILE_STANDARD_INFO).cast(),
            std::mem::size_of::<FILE_STANDARD_INFO>() as u32,
        )
    } == 0
        || info.AllocationSize < 0
    {
        return Err(MirageStatus::IoError);
    }
    Ok(info.AllocationSize as u64)
}
#[cfg(not(windows))]
pub fn allocated_bytes(path: &Path) -> Result<u64, MirageStatus> {
    std::fs::metadata(path)
        .map(|meta| meta.len())
        .map_err(|_| MirageStatus::IoError)
}

#[derive(Serialize, Deserialize)]
pub struct OwnerRecord {
    pub pid: u32,
    pub creation_100ns: u64,
    pub executable: PathBuf,
    pub config: PathBuf,
}
pub(super) fn record_owner(root: &Path, config: &Path) -> Result<(), MirageStatus> {
    let creation_100ns;
    #[cfg(windows)]
    {
        use windows_sys::Win32::{
            Foundation::FILETIME,
            System::Threading::{GetCurrentProcess, GetProcessTimes},
        };
        let mut created: FILETIME = unsafe { std::mem::zeroed() };
        let mut exit = created;
        let mut kernel = created;
        let mut user = created;
        // SAFETY: pseudo-handle for this process and writable FILETIME outputs.
        if unsafe {
            GetProcessTimes(
                GetCurrentProcess(),
                &mut created,
                &mut exit,
                &mut kernel,
                &mut user,
            )
        } == 0
        {
            return Err(MirageStatus::IoError);
        }
        creation_100ns =
            (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime);
    }
    #[cfg(not(windows))]
    {
        creation_100ns = 0;
    }
    let owner = OwnerRecord {
        pid: std::process::id(),
        creation_100ns,
        executable: std::env::current_exe().map_err(|_| MirageStatus::IoError)?,
        config: config.to_path_buf(),
    };
    let temp = root.join("source-owner.tmp");
    let target = root.join("source-owner.json");
    cache::ordinary_path(&temp)?;
    cache::ordinary_path(&target)?;
    let mut file = File::create(&temp).map_err(|_| MirageStatus::IoError)?;
    file.write_all(&serde_json::to_vec(&owner).map_err(|_| MirageStatus::Internal)?)
        .and_then(|()| file.sync_all())
        .map_err(|_| MirageStatus::IoError)?;
    drop(file);
    std::fs::rename(temp, target).map_err(|_| MirageStatus::IoError)
}

#[derive(Default, Serialize, Deserialize)]
pub struct Cleanup {
    pub pending: bool,
    pub reclaimed_bytes: u64,
    pub retained_bytes: u64,
    retain: Vec<Key>,
}

fn save(root: &Path, journal: &Cleanup) -> Result<(), MirageStatus> {
    let temp = root.join("cleanup.tmp");
    let target = root.join("cleanup.json");
    cache::ordinary_path(&temp)?;
    cache::ordinary_path(&target)?;
    let bytes = serde_json::to_vec(journal).map_err(|_| MirageStatus::Internal)?;
    let mut file = File::create(&temp).map_err(|_| MirageStatus::IoError)?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|_| MirageStatus::IoError)?;
    drop(file);
    std::fs::rename(temp, target).map_err(|_| MirageStatus::IoError)
}

pub(super) fn recover(root: &Path, disk: &mut cache::DiskCache) -> Result<(), MirageStatus> {
    let path = root.join("cleanup.json");
    cache::ordinary_path(&path)?;
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err(MirageStatus::IoError),
    };
    let mut bytes = Vec::new();
    file.take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| MirageStatus::IoError)?;
    if bytes.len() > 1024 * 1024 {
        return Err(MirageStatus::IntegrityFailure);
    }
    let mut journal: Cleanup =
        serde_json::from_slice(&bytes).map_err(|_| MirageStatus::IntegrityFailure)?;
    if journal.retain.len() > 8192 {
        return Err(MirageStatus::IntegrityFailure);
    }
    if !journal.pending {
        return Ok(());
    }
    let keep: HashSet<_> = journal.retain.iter().copied().collect();
    let before = disk.used();
    let result = disk.clear_except(&keep);
    journal.reclaimed_bytes = journal.reclaimed_bytes.saturating_add(before - disk.used());
    journal.retained_bytes = disk.used();
    journal.pending = result.is_err();
    save(root, &journal)?;
    result.map(|_| ())
}

/// Retain the explicit prepared chunk identities (Balanced) or none
/// (Disposable). Returns Conflict while any source owns the cache. Reparse
/// paths, unknown files and read-only seeds are never deleted.
pub fn cleanup(cache_root: &Path, retain: Vec<[u8; 32]>) -> Result<Cleanup, MirageStatus> {
    if retain.len() > 8192 {
        return Err(MirageStatus::InvalidArgument);
    }
    cache::ordinary_path(cache_root)?;
    cache::ordinary_path(&cache_root.join("owner.lock"))?;
    let lock = OpenOptions::new()
        .create(false)
        .read(true)
        .write(true)
        .open(cache_root.join("owner.lock"))
        .map_err(|_| MirageStatus::IoError)?;
    lock.try_lock().map_err(|_| MirageStatus::Conflict)?;
    let mut disk = cache::DiskCache::open(cache_root, u64::MAX)?;
    // Complete previous cleanup before accepting a new policy.
    recover(cache_root, &mut disk)?;
    let mut journal = Cleanup {
        pending: true,
        retain,
        ..Cleanup::default()
    };
    save(cache_root, &journal)?;
    let before = disk.used();
    let keep: HashSet<_> = journal.retain.iter().copied().collect();
    let result = disk.clear_except(&keep);
    journal.reclaimed_bytes = before - disk.used();
    journal.retained_bytes = disk.used();
    journal.pending = result.is_err();
    save(cache_root, &journal)?;
    // A locked chunk leaves an inspectable pending journal and is retried on
    // reopen. Return the partial measured result, never report it as success.
    Ok(journal)
}
