use mirage_db::Database;
use mirage_index::MountIndex;
use mirage_manifest::{DecodeLimits, decode_manifest_bounded, manifest_hash};
use mirage_types::{ContentHash, MirageError, RepositoryId};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepairLevel {
    Metadata,
    LocalCache,
    SampledRemote,
    DeepRemote,
}
#[derive(Debug, Default, PartialEq, Eq)]
pub struct RepairReport {
    pub verified_objects: usize,
    pub missing_objects: Vec<ContentHash>,
    pub dirty_pages_preserved: usize,
}

pub trait RepairSource {
    fn verify(&self, level: RepairLevel) -> Result<RepairReport, MirageError>;
}
pub fn repair(source: &impl RepairSource, level: RepairLevel) -> Result<RepairReport, MirageError> {
    source.verify(level)
}

#[derive(Debug, Clone)]
pub struct LocalMetadataRepairSource {
    database: Database,
    repository_id: RepositoryId,
}

impl LocalMetadataRepairSource {
    #[must_use]
    pub const fn new(database: Database, repository_id: RepositoryId) -> Self {
        Self {
            database,
            repository_id,
        }
    }
}

impl RepairSource for LocalMetadataRepairSource {
    fn verify(&self, level: RepairLevel) -> Result<RepairReport, MirageError> {
        if matches!(level, RepairLevel::SampledRemote | RepairLevel::DeepRemote) {
            return Err(MirageError::provider_unavailable(
                "remote repair requires an authenticated repository backend",
            ));
        }
        let active = self
            .database
            .load_active_generation(self.repository_id)?
            .ok_or_else(|| MirageError::invalid_argument("repository has no active generation"))?;
        let bytes = std::fs::read(&active.manifest_local_path).map_err(|error| {
            MirageError::integrity_mismatch("active manifest is unavailable").with_source(error)
        })?;
        let manifest = decode_manifest_bounded(&bytes, DecodeLimits::default())?;
        if manifest.repository_id != self.repository_id
            || manifest.generation_id != active.generation_id
            || manifest_hash(&manifest)? != active.manifest_hash
        {
            return Err(MirageError::integrity_mismatch(
                "active manifest identity does not match durable generation metadata",
            ));
        }
        if let Some(index_path) = &active.mount_index_path {
            let index = MountIndex::open(index_path)?;
            if index.header().repository_id != self.repository_id
                || index.header().generation_id != active.generation_id
            {
                return Err(MirageError::integrity_mismatch(
                    "mount index identity does not match active generation",
                ));
            }
        }
        let verified_objects = manifest
            .remote_locations
            .iter()
            .map(|location| location.object.content_hash)
            .collect::<BTreeSet<_>>()
            .len();
        Ok(RepairReport {
            verified_objects,
            missing_objects: Vec::new(),
            dirty_pages_preserved: 0,
        })
    }
}
