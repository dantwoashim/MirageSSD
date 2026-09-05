use std::fs::File;
use std::io;

#[cfg(windows)]
pub(crate) fn open_restrictive(path: &std::path::Path, create_new: bool) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).share_mode(0);
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
