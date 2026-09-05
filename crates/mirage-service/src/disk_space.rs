use std::path::Path;

use mirage_types::MirageError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VolumeSpace {
    pub volume_id: String,
    pub available_bytes: u64,
    pub total_bytes: u64,
    pub total_free_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TreeAllocation {
    pub physical_bytes: u64,
    pub latest_write_sequence: u64,
}

pub(crate) fn allocated_tree(root: &Path) -> Result<TreeAllocation, MirageError> {
    const MAX_ENTRIES: usize = 2_000_000;
    let root_metadata = std::fs::symlink_metadata(root).map_err(MirageError::from)?;
    if !root_metadata.is_dir() || is_reparse(&root_metadata) {
        return Err(MirageError::invalid_argument(
            "physical tree accounting requires a regular directory",
        ));
    }
    let mut pending = vec![root.to_path_buf()];
    let mut entries_seen = 0_usize;
    let mut physical_bytes = 0_u64;
    let mut latest_write_sequence = modified_sequence(&root_metadata);
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).map_err(MirageError::from)? {
            let entry = entry.map_err(MirageError::from)?;
            entries_seen += 1;
            if entries_seen > MAX_ENTRIES {
                return Err(MirageError::unsupported_layout(
                    "physical tree accounting exceeds the file-count bound",
                ));
            }
            let path = entry.path();
            let metadata = std::fs::symlink_metadata(&path).map_err(MirageError::from)?;
            if is_reparse(&metadata) {
                return Err(MirageError::invalid_argument(
                    "physical tree accounting refuses reparse points",
                ));
            }
            latest_write_sequence = latest_write_sequence.max(modified_sequence(&metadata));
            if metadata.is_dir() {
                pending.push(path);
            } else if metadata.is_file() {
                physical_bytes = physical_bytes
                    .checked_add(allocated_file_bytes(&path)?)
                    .ok_or_else(|| {
                        MirageError::unsupported_layout("physical tree allocation overflows")
                    })?;
            } else {
                return Err(MirageError::unsupported_layout(
                    "physical tree contains a non-file entry",
                ));
            }
        }
    }
    Ok(TreeAllocation {
        physical_bytes,
        latest_write_sequence,
    })
}

fn modified_sequence(metadata: &std::fs::Metadata) -> u64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |duration| {
            u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
        })
}

#[cfg(windows)]
fn is_reparse(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    metadata.file_attributes() & 0x400 != 0
}

#[cfg(not(windows))]
fn is_reparse(metadata: &std::fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

#[cfg(windows)]
pub(crate) fn allocated_file_bytes(path: &Path) -> Result<u64, MirageError> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{ERROR_SUCCESS, GetLastError};
    use windows_sys::Win32::Storage::FileSystem::GetCompressedFileSizeW;

    let wide = path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let mut high = 0_u32;
    // SAFETY: the input is NUL-terminated and remains live; high is a valid output pointer.
    let low = unsafe { GetCompressedFileSizeW(wide.as_ptr(), &mut high) };
    if low == u32::MAX {
        // SAFETY: GetLastError takes no pointers and observes this thread's last error.
        let error = unsafe { GetLastError() };
        if error != ERROR_SUCCESS {
            return Err(MirageError::from(std::io::Error::from_raw_os_error(
                error as i32,
            )));
        }
    }
    Ok((u64::from(high) << 32) | u64::from(low))
}

#[cfg(not(windows))]
pub(crate) fn allocated_file_bytes(_path: &Path) -> Result<u64, MirageError> {
    Err(MirageError::provider_unavailable(
        "physical file allocation accounting is currently available only on Windows NTFS",
    ))
}

#[cfg(windows)]
pub(crate) fn query(path: &Path) -> Result<VolumeSpace, MirageError> {
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::Storage::FileSystem::{
        GetDiskFreeSpaceExW, GetVolumeNameForVolumeMountPointW, GetVolumePathNameW,
    };

    fn wide(value: &std::ffi::OsStr) -> Vec<u16> {
        value.encode_wide().chain(Some(0)).collect()
    }

    let path = path.canonicalize().map_err(MirageError::from)?;
    let path_wide = wide(path.as_os_str());
    let mut volume_path = vec![0_u16; 32_768];
    // SAFETY: the input is NUL-terminated and all output buffers remain live and writable for
    // the synchronous Win32 calls. Their lengths match the values supplied to the APIs.
    let volume_path_ok = unsafe {
        GetVolumePathNameW(
            path_wide.as_ptr(),
            volume_path.as_mut_ptr(),
            volume_path.len() as u32,
        )
    };
    if volume_path_ok == 0 {
        return Err(MirageError::from(std::io::Error::last_os_error()));
    }
    let volume_path_length = volume_path
        .iter()
        .position(|value| *value == 0)
        .ok_or_else(|| MirageError::integrity_mismatch("volume path is not NUL-terminated"))?;
    volume_path.truncate(volume_path_length + 1);

    let mut volume_name = vec![0_u16; 128];
    // SAFETY: volume_path is NUL-terminated and the output buffer is writable and correctly sized.
    let volume_name_ok = unsafe {
        GetVolumeNameForVolumeMountPointW(
            volume_path.as_ptr(),
            volume_name.as_mut_ptr(),
            volume_name.len() as u32,
        )
    };
    if volume_name_ok == 0 {
        return Err(MirageError::from(std::io::Error::last_os_error()));
    }
    let volume_name_length = volume_name
        .iter()
        .position(|value| *value == 0)
        .ok_or_else(|| MirageError::integrity_mismatch("volume name is not NUL-terminated"))?;
    let volume_id = String::from_utf16(&volume_name[..volume_name_length])
        .map_err(|_| MirageError::integrity_mismatch("volume name is not valid UTF-16"))?
        .to_ascii_uppercase();

    let mut available_bytes = 0_u64;
    let mut total_bytes = 0_u64;
    let mut total_free_bytes = 0_u64;
    // SAFETY: volume_path is NUL-terminated and all three output pointers are valid and unique.
    let space_ok = unsafe {
        GetDiskFreeSpaceExW(
            volume_path.as_ptr(),
            &mut available_bytes,
            &mut total_bytes,
            &mut total_free_bytes,
        )
    };
    if space_ok == 0 {
        return Err(MirageError::from(std::io::Error::last_os_error()));
    }
    Ok(VolumeSpace {
        volume_id,
        available_bytes,
        total_bytes,
        total_free_bytes,
    })
}

#[cfg(not(windows))]
pub(crate) fn query(_path: &Path) -> Result<VolumeSpace, MirageError> {
    Err(MirageError::provider_unavailable(
        "physical volume accounting is currently available only on Windows NTFS",
    ))
}
