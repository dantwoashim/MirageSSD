use std::fs::File;
use std::io;

#[cfg(windows)]
pub(crate) fn open_restrictive(path: &std::path::Path, create_new: bool) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_SHARE_READ: u32 = 0x1;
    const FILE_SHARE_WRITE: u32 = 0x2;
    let mut options = std::fs::OpenOptions::new();
    // The service (materialize/admit) and the WinFsp host (mirage-fs.exe) hold the arena open
    // concurrently; an exclusive open made sealed admission fail with a sharing violation.
    options
        .read(true)
        .write(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE);
    if create_new {
        options.create_new(true);
    }
    options.open(path)
}

#[cfg(not(windows))]
pub(crate) fn open_restrictive(path: &std::path::Path, create_new: bool) -> io::Result<File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true);
    if create_new {
        options.create_new(true);
    }
    options.open(path)
}

#[cfg(windows)]
pub(crate) fn write_exact_at(file: &File, mut bytes: &[u8], mut offset: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !bytes.is_empty() {
        let written = file.seek_write(bytes, offset)?;
        if written == 0 {
            return Err(io::ErrorKind::WriteZero.into());
        }
        bytes = &bytes[written..];
        offset += written as u64;
    }
    Ok(())
}

#[cfg(not(windows))]
pub(crate) fn write_exact_at(file: &File, mut bytes: &[u8], mut offset: u64) -> io::Result<()> {
    use std::os::unix::fs::FileExt;
    while !bytes.is_empty() {
        let written = file.write_at(bytes, offset)?;
        if written == 0 {
            return Err(io::ErrorKind::WriteZero.into());
        }
        bytes = &bytes[written..];
        offset += written as u64;
    }
    Ok(())
}

#[cfg(windows)]
pub(crate) fn read_exact_at(file: &File, mut bytes: &mut [u8], mut offset: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !bytes.is_empty() {
        let read = file.seek_read(bytes, offset)?;
        if read == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        let (_, rest) = bytes.split_at_mut(read);
        bytes = rest;
        offset += read as u64;
    }
    Ok(())
}

/// Read-only arena handle without cache-manager buffering. The mounted volume
/// is already kernel-cached, so buffering arena reads here would hold every hot
/// page in the cache manager twice.
#[cfg(windows)]
pub(crate) fn open_unbuffered_read(path: &std::path::Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_SHARE_READ: u32 = 0x1;
    const FILE_SHARE_WRITE: u32 = 0x2;
    const FILE_FLAG_NO_BUFFERING: u32 = 0x2000_0000;
    std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_NO_BUFFERING)
        .open(path)
}

#[cfg(not(windows))]
pub(crate) fn open_unbuffered_read(path: &std::path::Path) -> io::Result<File> {
    std::fs::OpenOptions::new().read(true).open(path)
}

/// 4096-aligned scratch buffer; `FILE_FLAG_NO_BUFFERING` requires the transfer
/// buffer to be sector-aligned.
pub(crate) struct AlignedBuf {
    ptr: std::ptr::NonNull<u8>,
    layout: std::alloc::Layout,
}

#[allow(unsafe_code)]
impl AlignedBuf {
    pub fn new(size: usize) -> io::Result<Self> {
        let layout = std::alloc::Layout::from_size_align(size.max(1), 4096)
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        // SAFETY: layout was just constructed as valid; a null allocation is
        // checked immediately below.
        let ptr = unsafe { std::alloc::alloc_zeroed(layout) };
        let ptr = std::ptr::NonNull::new(ptr).ok_or(io::ErrorKind::OutOfMemory)?;
        Ok(Self { ptr, layout })
    }
    pub fn len(&self) -> usize {
        self.layout.size()
    }
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: `ptr` owns `layout.size()` bytes for the lifetime of self.
        unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.layout.size()) }
    }
}

#[allow(unsafe_code)]
impl Drop for AlignedBuf {
    fn drop(&mut self) {
        // SAFETY: `ptr`/`layout` came from the `alloc` call in `new` and are
        // released exactly once here.
        unsafe { std::alloc::dealloc(self.ptr.as_ptr(), self.layout) }
    }
}

#[cfg(not(windows))]
pub(crate) fn read_exact_at(file: &File, mut bytes: &mut [u8], mut offset: u64) -> io::Result<()> {
    use std::os::unix::fs::FileExt;
    while !bytes.is_empty() {
        let read = file.read_at(bytes, offset)?;
        if read == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        let (_, rest) = bytes.split_at_mut(read);
        bytes = rest;
        offset += read as u64;
    }
    Ok(())
}
