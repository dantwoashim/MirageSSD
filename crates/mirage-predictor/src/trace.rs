use mirage_types::{GenerationId, MirageError, RepositoryId, StableFileId};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};

const MAGIC: &[u8; 8] = b"MIRTRC1\0";
const VERSION: u32 = 1;
const HEADER_BYTES: usize = 50;
const EVENT_BYTES: usize = 32;
const MAX_EVENTS: usize = 65_536;
const MAX_BLOCK_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TraceHeader {
    pub schema_version: u32,
    pub repository_id: RepositoryId,
    pub manifest_generation: GenerationId,
    pub page_size: u32,
    pub machine_profile: String,
    pub dropped_event_count: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TraceEvent {
    pub timestamp_ns: u64,
    pub stable_file_id: StableFileId,
    pub offset: u64,
    pub length: u32,
    pub flags: u32,
}

pub struct TraceBlockEncoder<W> {
    writer: W,
}
impl<W: Write> TraceBlockEncoder<W> {
    pub fn new(mut writer: W, header: &TraceHeader) -> Result<Self, MirageError> {
        validate_header(header)?;
        writer.write_all(MAGIC).map_err(MirageError::from)?;
        writer
            .write_all(&VERSION.to_le_bytes())
            .map_err(MirageError::from)?;
        writer
            .write_all(header.repository_id.as_bytes())
            .map_err(MirageError::from)?;
        writer
            .write_all(&header.manifest_generation.as_u64().to_le_bytes())
            .map_err(MirageError::from)?;
        writer
            .write_all(&header.page_size.to_le_bytes())
            .map_err(MirageError::from)?;
        writer
            .write_all(&header.dropped_event_count.to_le_bytes())
            .map_err(MirageError::from)?;
        let profile = header.machine_profile.as_bytes();
        writer
            .write_all(&(profile.len() as u16).to_le_bytes())
            .map_err(MirageError::from)?;
        writer.write_all(profile).map_err(MirageError::from)?;
        Ok(Self { writer })
    }
    pub fn write_block(&mut self, events: &[TraceEvent]) -> Result<(), MirageError> {
        if events.is_empty() || events.len() > MAX_EVENTS {
            return Err(MirageError::invalid_argument(
                "trace block event count is outside bounds",
            ));
        }
        let mut body = Vec::with_capacity(events.len() * EVENT_BYTES);
        let (mut prior_time, mut prior_id) = (0_u64, 0_u64);
        for event in events {
            if event.timestamp_ns < prior_time {
                return Err(MirageError::invalid_argument(
                    "trace timestamps are not monotonic",
                ));
            }
            body.extend_from_slice(&(event.timestamp_ns - prior_time).to_le_bytes());
            body.extend_from_slice(&(event.stable_file_id.as_u64() ^ prior_id).to_le_bytes());
            body.extend_from_slice(&event.offset.to_le_bytes());
            body.extend_from_slice(&event.length.to_le_bytes());
            body.extend_from_slice(&event.flags.to_le_bytes());
            prior_time = event.timestamp_ns;
            prior_id = event.stable_file_id.as_u64();
        }
        if body.len() > MAX_BLOCK_BYTES {
            return Err(MirageError::invalid_argument(
                "trace block exceeds byte bound",
            ));
        }
        self.writer
            .write_all(&(events.len() as u32).to_le_bytes())
            .map_err(MirageError::from)?;
        self.writer
            .write_all(&(body.len() as u32).to_le_bytes())
            .map_err(MirageError::from)?;
        self.writer
            .write_all(&crc32fast::hash(&body).to_le_bytes())
            .map_err(MirageError::from)?;
        self.writer.write_all(&body).map_err(MirageError::from)
    }
    pub fn finish(self) -> W {
        self.writer
    }
}

pub struct TraceBlockDecoder<R> {
    reader: R,
    pub header: TraceHeader,
}
impl<R: Read> TraceBlockDecoder<R> {
    pub fn new(mut reader: R) -> Result<Self, MirageError> {
        let mut fixed = [0_u8; HEADER_BYTES];
        reader.read_exact(&mut fixed).map_err(MirageError::from)?;
        if &fixed[..8] != MAGIC
            || u32::from_le_bytes(fixed[8..12].try_into().expect("slice")) != VERSION
        {
            return Err(MirageError::unsupported_layout("unsupported trace format"));
        }
        let profile_len = u16::from_le_bytes(fixed[48..50].try_into().expect("slice")) as usize;
        if profile_len > 4096 {
            return Err(MirageError::manifest_invalid("trace profile is too long"));
        }
        let mut profile = vec![0_u8; profile_len];
        reader.read_exact(&mut profile).map_err(MirageError::from)?;
        let header = TraceHeader {
            schema_version: VERSION,
            repository_id: RepositoryId::from_bytes(fixed[12..28].try_into().expect("slice")),
            manifest_generation: GenerationId::from_u64(u64::from_le_bytes(
                fixed[28..36].try_into().expect("slice"),
            )),
            page_size: u32::from_le_bytes(fixed[36..40].try_into().expect("slice")),
            dropped_event_count: u64::from_le_bytes(fixed[40..48].try_into().expect("slice")),
            machine_profile: String::from_utf8(profile)
                .map_err(|_| MirageError::manifest_invalid("trace profile is not UTF-8"))?,
        };
        validate_header(&header)?;
        Ok(Self { reader, header })
    }
    pub fn read_block(&mut self) -> Result<Option<Vec<TraceEvent>>, MirageError> {
        let mut frame = [0_u8; 12];
        if self
            .reader
            .read(&mut frame[..1])
            .map_err(MirageError::from)?
            == 0
        {
            return Ok(None);
        }
        self.reader
            .read_exact(&mut frame[1..])
            .map_err(MirageError::from)?;
        let count = u32::from_le_bytes(frame[..4].try_into().expect("slice")) as usize;
        let length = u32::from_le_bytes(frame[4..8].try_into().expect("slice")) as usize;
        if count == 0
            || count > MAX_EVENTS
            || length > MAX_BLOCK_BYTES
            || length != count * EVENT_BYTES
        {
            return Err(MirageError::manifest_invalid(
                "trace block bounds are invalid",
            ));
        }
        let mut body = vec![0_u8; length];
        self.reader
            .read_exact(&mut body)
            .map_err(MirageError::from)?;
        if crc32fast::hash(&body) != u32::from_le_bytes(frame[8..12].try_into().expect("slice")) {
            return Err(MirageError::integrity_mismatch(
                "trace block checksum is invalid",
            ));
        }
        let mut output = Vec::with_capacity(count);
        let (mut time, mut prior_id) = (0_u64, 0_u64);
        for chunk in body.chunks_exact(EVENT_BYTES) {
            time = time
                .checked_add(u64::from_le_bytes(chunk[..8].try_into().expect("slice")))
                .ok_or_else(|| MirageError::manifest_invalid("trace timestamp overflows"))?;
            let id = u64::from_le_bytes(chunk[8..16].try_into().expect("slice")) ^ prior_id;
            output.push(TraceEvent {
                timestamp_ns: time,
                stable_file_id: StableFileId::from_u64(id),
                offset: u64::from_le_bytes(chunk[16..24].try_into().expect("slice")),
                length: u32::from_le_bytes(chunk[24..28].try_into().expect("slice")),
                flags: u32::from_le_bytes(chunk[28..32].try_into().expect("slice")),
            });
            prior_id = id;
        }
        Ok(Some(output))
    }
}
fn validate_header(header: &TraceHeader) -> Result<(), MirageError> {
    if header.schema_version != VERSION
        || header.page_size == 0
        || header.machine_profile.len() > 4096
    {
        Err(MirageError::invalid_argument(
            "trace header is outside supported bounds",
        ))
    } else {
        Ok(())
    }
}
