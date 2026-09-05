use crate::correlate::TraceEvent;
use crate::segment::{SEGMENT_VERSION, Segment};
use mirage_types::MirageError;
use std::path::{Path, PathBuf};

pub struct SegmentWriter {
    root: PathBuf,
    sequence: u64,
    maximum_events: usize,
}
impl SegmentWriter {
    pub fn new(root: &Path, maximum_events: usize) -> Result<Self, MirageError> {
        if maximum_events == 0 || maximum_events > 1_000_000 {
            return Err(MirageError::invalid_argument("invalid trace segment bound"));
        }
        std::fs::create_dir_all(root).map_err(MirageError::from)?;
        Ok(Self {
            root: root.to_owned(),
            sequence: 0,
            maximum_events,
        })
    }
    pub fn write(&mut self, events: Vec<TraceEvent>) -> Result<PathBuf, MirageError> {
        if events.is_empty() || events.len() > self.maximum_events {
            return Err(MirageError::invalid_argument(
                "trace segment event count is outside bound",
            ));
        }
        let payload = serde_json::to_vec(&(SEGMENT_VERSION, self.sequence, &events))
            .map_err(|_| MirageError::internal_invariant("trace serialization failed"))?;
        let checksum = blake3::hash(&payload).to_hex().to_string();
        let segment = Segment {
            version: SEGMENT_VERSION,
            sequence: self.sequence,
            events,
            checksum,
        };
        let bytes = serde_json::to_vec(&segment)
            .map_err(|_| MirageError::internal_invariant("trace serialization failed"))?;
        let final_path = self.root.join(format!("segment-{:08}.json", self.sequence));
        let temporary = final_path.with_extension("tmp");
        {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary)
                .map_err(MirageError::from)?;
            file.write_all(&bytes).map_err(MirageError::from)?;
            file.sync_all().map_err(MirageError::from)?;
        }
        std::fs::rename(&temporary, &final_path).map_err(MirageError::from)?;
        self.sequence += 1;
        Ok(final_path)
    }
}
