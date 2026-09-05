use mirage_backend::{BackendError, ImmutableRevision, ProviderObjectId};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateMetadata<'a> {
    pub name: &'a str,
    pub app_properties: &'a std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DriveFile {
    pub id: String,
    pub size: Option<String>,
    pub md5_checksum: Option<String>,
    pub head_revision_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CompletedUpload {
    pub file_id: ProviderObjectId,
    pub revision: Option<ImmutableRevision>,
    pub size: u64,
    pub md5_checksum: Option<String>,
}

impl TryFrom<DriveFile> for CompletedUpload {
    type Error = BackendError;
    fn try_from(value: DriveFile) -> Result<Self, Self::Error> {
        let size = value
            .size
            .ok_or_else(|| BackendError::integrity("Drive upload response omitted size"))?
            .parse::<u64>()
            .map_err(|_| BackendError::integrity("Drive upload response contained invalid size"))?;
        Ok(Self {
            file_id: ProviderObjectId::new(value.id).map_err(|_| {
                BackendError::integrity("Drive upload response contained invalid file ID")
            })?,
            revision: value
                .head_revision_id
                .map(ImmutableRevision::new)
                .transpose()
                .map_err(|_| {
                    BackendError::integrity("Drive upload response contained invalid revision")
                })?,
            size,
            md5_checksum: value.md5_checksum,
        })
    }
}
