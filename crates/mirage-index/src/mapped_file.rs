use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

use memmap2::{Mmap, MmapOptions};
use mirage_types::{MirageError, MirageErrorKind};

#[derive(Debug)]
pub struct ReadOnlyMapping(Mmap);

impl ReadOnlyMapping {
    #[allow(unsafe_code)]
    pub fn open(path: &Path) -> Result<Self, MirageError> {
        let file = open_immutable(path).map_err(MirageError::from)?;
        if file.metadata().map_err(MirageError::from)?.len() == 0 {
            return Err(MirageError::new(
                MirageErrorKind::ManifestInvalid,
                MirageErrorKind::ManifestInvalid.default_code(),
                "mount index file is empty",
            ));
        }
        // SAFETY: the mapping is read-only, the file handle is valid for this call, and
        // all bytes remain untrusted until Header and record validation complete. Mirage
        // publishes immutable index files by atomic rename and never mutates them in place.
        let mapping = unsafe { MmapOptions::new().map(&file) }.map_err(MirageError::from)?;
        Ok(Self(mapping))
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

#[cfg(windows)]
fn open_immutable(path: &Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_DELETE, FILE_SHARE_READ};

    OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_DELETE)
        .open(path)
}

#[cfg(not(windows))]
fn open_immutable(path: &Path) -> io::Result<File> {
    OpenOptions::new().read(true).open(path)
}
