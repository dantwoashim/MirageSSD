#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriveMethod {
    Download,
    GetMetadata,
    List,
    Create,
    Update,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuotaPolicy {
    pub download: u32,
    pub get_metadata: u32,
    pub list: u32,
    pub create: u32,
    pub update: u32,
}
impl QuotaPolicy {
    pub fn units(&self, method: DriveMethod) -> u32 {
        match method {
            DriveMethod::Download => self.download,
            DriveMethod::GetMetadata => self.get_metadata,
            DriveMethod::List => self.list,
            DriveMethod::Create => self.create,
            DriveMethod::Update => self.update,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StorageQuota {
    pub limit: Option<u64>,
    pub usage: u64,
    pub usage_in_drive: u64,
    pub usage_in_trash: u64,
}

pub async fn storage_quota(
    transport: &dyn HttpTransport,
    access_token: &str,
) -> Result<StorageQuota, BackendError> {
    let response = transport
        .execute(HttpRequest {
            method: Method::Get,
            url: "https://www.googleapis.com/drive/v3/about?fields=storageQuota(limit,usage,usageInDrive,usageInDriveTrash)".into(),
            headers: [("authorization".into(), format!("Bearer {access_token}"))].into(),
            body: Default::default(),
        })
        .await?;
    if response.status != 200 {
        return Err(crate::error::classify_response(&response));
    }
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct About {
        storage_quota: WireQuota,
    }
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct WireQuota {
        limit: Option<String>,
        usage: String,
        usage_in_drive: String,
        usage_in_drive_trash: String,
    }
    let wire: About = serde_json::from_slice(&response.body)
        .map_err(|_| BackendError::integrity("Drive quota response was invalid"))?;
    Ok(StorageQuota {
        limit: wire
            .storage_quota
            .limit
            .map(|value| parse(&value))
            .transpose()?,
        usage: parse(&wire.storage_quota.usage)?,
        usage_in_drive: parse(&wire.storage_quota.usage_in_drive)?,
        usage_in_trash: parse(&wire.storage_quota.usage_in_drive_trash)?,
    })
}

fn parse(value: &str) -> Result<u64, BackendError> {
    value
        .parse()
        .map_err(|_| BackendError::integrity("Drive quota byte count was invalid"))
}
use mirage_backend::BackendError;

use crate::http::{HttpRequest, HttpTransport, Method};
