use std::fs::File;
use std::io::Read;
use std::path::Path;

use bytes::Bytes;
use futures_util::stream;
use mirage_backend::{BackendByteStream, BackendError, UploadSource};
use mirage_types::{ByteCount, ContentHash, MirageError};

const UPLOAD_CHUNK_BYTES: usize = 1024 * 1024;

pub fn upload_source_from_file(path: &Path) -> Result<UploadSource, MirageError> {
    let file = File::open(path).map_err(MirageError::from)?;
    let length = file.metadata().map_err(MirageError::from)?.len();
    let chunks = stream::try_unfold(file, |mut file| async move {
        let mut buffer = vec![0_u8; UPLOAD_CHUNK_BYTES];
        let read = file.read(&mut buffer).map_err(|error| {
            BackendError::permanent("failed to read local publication source").with_source(error)
        })?;
        if read == 0 {
            Ok(None)
        } else {
            buffer.truncate(read);
            Ok(Some((Bytes::from(buffer), file)))
        }
    });
    UploadSource::new(
        ByteCount::from_u64(length),
        BackendByteStream::new(chunks, length),
    )
    .map_err(Into::into)
}

#[must_use]
pub fn pack_set_hash(mut hashes: Vec<ContentHash>) -> ContentHash {
    hashes.sort_unstable();
    hashes.dedup();
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"MirageSSD pack set v1\0");
    for hash in hashes {
        hasher.update(hash.as_bytes());
    }
    ContentHash::from_bytes(*hasher.finalize().as_bytes())
}
