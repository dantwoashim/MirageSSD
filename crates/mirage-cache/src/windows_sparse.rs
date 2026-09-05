use std::fs::File;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::AsRawHandle;
use std::path::Path;

use mirage_types::MirageError;
use windows_sys::Win32::Foundation::{ERROR_MORE_DATA, ERROR_SUCCESS, GetLastError};
use windows_sys::Win32::Storage::FileSystem::GetCompressedFileSizeW;
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::System::Ioctl::{
    FILE_ALLOCATED_RANGE_BUFFER, FILE_ZERO_DATA_INFORMATION, FSCTL_QUERY_ALLOCATED_RANGES,
    FSCTL_SET_SPARSE, FSCTL_SET_ZERO_DATA,
};

pub(crate) fn set_sparse(file: &File) -> Result<(), MirageError> {
    let mut returned = 0_u32;
    // SAFETY: the handle is live for the call, no input/output buffers are required by
    // FSCTL_SET_SPARSE, and the byte-count pointer is valid and uniquely borrowed.
    let success = unsafe {
        DeviceIoControl(
            file.as_raw_handle(),
            FSCTL_SET_SPARSE,
            std::ptr::null(),
            0,
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    if success == 0 {
        return Err(MirageError::from(std::io::Error::last_os_error()));
    }
    Ok(())
}

pub(crate) fn allocated_bytes(path: &Path) -> Result<u64, MirageError> {
    let wide = path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let mut high = 0_u32;
    // SAFETY: wide is NUL-terminated and remains live; high is a valid output pointer.
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

pub(crate) fn allocated_bytes_in_range(
    file: &File,
    offset: u64,
    length: u64,
) -> Result<u64, MirageError> {
    const OUTPUT_RANGES: usize = 512;

    if length == 0 {
        return Ok(0);
    }
    let range_end = offset
        .checked_add(length)
        .ok_or_else(|| MirageError::invalid_argument("allocation query range overflows"))?;
    let mut query_start = offset;
    let mut allocated = 0_u64;

    while query_start < range_end {
        let query = FILE_ALLOCATED_RANGE_BUFFER {
            FileOffset: i64::try_from(query_start).map_err(|_| {
                MirageError::invalid_argument("allocation query offset exceeds i64")
            })?,
            Length: i64::try_from(range_end - query_start).map_err(|_| {
                MirageError::invalid_argument("allocation query length exceeds i64")
            })?,
        };
        let mut output = vec![FILE_ALLOCATED_RANGE_BUFFER::default(); OUTPUT_RANGES];
        let mut returned = 0_u32;
        // SAFETY: all pointers refer to live, correctly sized buffers for the duration of this
        // synchronous call. The file handle is valid and the output byte count is uniquely borrowed.
        let success = unsafe {
            DeviceIoControl(
                file.as_raw_handle(),
                FSCTL_QUERY_ALLOCATED_RANGES,
                (&raw const query).cast(),
                std::mem::size_of::<FILE_ALLOCATED_RANGE_BUFFER>() as u32,
                output.as_mut_ptr().cast(),
                (output.len() * std::mem::size_of::<FILE_ALLOCATED_RANGE_BUFFER>()) as u32,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        let more_data = if success == 0 {
            // SAFETY: GetLastError takes no pointers and observes this thread's last error.
            let error = unsafe { GetLastError() };
            if error != ERROR_MORE_DATA {
                return Err(MirageError::from(std::io::Error::from_raw_os_error(
                    error as i32,
                )));
            }
            true
        } else {
            false
        };
        let record_size = std::mem::size_of::<FILE_ALLOCATED_RANGE_BUFFER>();
        if !(returned as usize).is_multiple_of(record_size) {
            return Err(MirageError::integrity_mismatch(
                "allocated-range query returned a partial record",
            ));
        }
        let count = returned as usize / record_size;
        if count > output.len() {
            return Err(MirageError::internal_invariant(
                "allocated-range query exceeded its output buffer",
            ));
        }

        let mut next_start = query_start;
        for range in &output[..count] {
            let start = u64::try_from(range.FileOffset).map_err(|_| {
                MirageError::integrity_mismatch("allocated range has a negative offset")
            })?;
            let range_length = u64::try_from(range.Length).map_err(|_| {
                MirageError::integrity_mismatch("allocated range has a negative length")
            })?;
            let end = start
                .checked_add(range_length)
                .ok_or_else(|| MirageError::integrity_mismatch("allocated range end overflows"))?;
            let clipped_start = start.max(offset);
            let clipped_end = end.min(range_end);
            if clipped_end > clipped_start {
                allocated = allocated
                    .checked_add(clipped_end - clipped_start)
                    .ok_or_else(|| {
                        MirageError::invalid_argument("allocated-byte accounting overflows")
                    })?;
            }
            next_start = next_start.max(end);
        }

        if !more_data {
            return Ok(allocated);
        }
        if count == 0 || next_start <= query_start {
            return Err(MirageError::provider_unavailable(
                "allocated-range query could not make progress",
            ));
        }
        query_start = next_start.min(range_end);
    }

    Ok(allocated)
}

pub(crate) fn deallocate(file: &File, offset: u64, length: u64) -> Result<(), MirageError> {
    let beyond = offset
        .checked_add(length)
        .ok_or_else(|| MirageError::invalid_argument("sparse deallocation range overflows"))?;
    let range = FILE_ZERO_DATA_INFORMATION {
        FileOffset: i64::try_from(offset)
            .map_err(|_| MirageError::invalid_argument("sparse offset exceeds i64"))?,
        BeyondFinalZero: i64::try_from(beyond)
            .map_err(|_| MirageError::invalid_argument("sparse end exceeds i64"))?,
    };
    let mut returned = 0_u32;
    // SAFETY: the live file handle and immutable input struct remain valid for the synchronous call; no output buffer is used.
    let success = unsafe {
        DeviceIoControl(
            file.as_raw_handle(),
            FSCTL_SET_ZERO_DATA,
            (&raw const range).cast(),
            std::mem::size_of::<FILE_ZERO_DATA_INFORMATION>() as u32,
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    if success == 0 {
        return Err(MirageError::from(std::io::Error::last_os_error()));
    }
    Ok(())
}
