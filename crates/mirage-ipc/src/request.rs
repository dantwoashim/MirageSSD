use mirage_types::{CapsuleId, GenerationId, RepositoryId, SpaceLeaseId};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use std::path::PathBuf;
use zeroize::Zeroizing;

#[derive(Clone, PartialEq, Eq)]
pub struct SensitiveString(Zeroizing<String>);

impl SensitiveString {
    pub fn new(value: String) -> Result<Self, mirage_types::MirageError> {
        if value.is_empty() || value.len() > 16 * 1024 || value.chars().any(char::is_control) {
            return Err(mirage_types::MirageError::invalid_argument(
                "sensitive IPC string is empty, oversized, or malformed",
            ));
        }
        Ok(Self(Zeroizing::new(value)))
    }

    #[must_use]
    pub fn expose(&self) -> &str {
        self.0.as_str()
    }
}

impl std::fmt::Debug for SensitiveString {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("[REDACTED]")
    }
}

impl Serialize for SensitiveString {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.expose())
    }
}

impl<'de> Deserialize<'de> for SensitiveString {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DriveQuotaSnapshot {
    pub limit_bytes: Option<u64>,
    pub usage_bytes: u64,
}

impl DriveQuotaSnapshot {
    pub fn validate(self) -> Result<(), mirage_types::MirageError> {
        if self
            .limit_bytes
            .is_some_and(|limit| limit == 0 || self.usage_bytes > limit)
        {
            return Err(mirage_types::MirageError::invalid_argument(
                "Drive quota snapshot is invalid",
            ));
        }
        Ok(())
    }

    #[must_use]
    pub const fn available_bytes(self) -> Option<u64> {
        match self.limit_bytes {
            Some(limit) => Some(limit.saturating_sub(self.usage_bytes)),
            None => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "command", content = "body", rename_all = "snake_case")]
pub enum Command {
    Status,
    RepositoryList,
    RepositoryDetail {
        repository_id: RepositoryId,
    },
    RepositoryRegister {
        repository_id: RepositoryId,
        display_name: String,
        native_root: PathBuf,
        mount_subtree: PathBuf,
        import_root: PathBuf,
        launcher_relative: PathBuf,
        arguments: Vec<String>,
        version_label: String,
        configuration_label: String,
        cache_bytes: u64,
    },
    RepositoryAdopt {
        repository_id: RepositoryId,
    },
    RepositoryConvert {
        repository_id: RepositoryId,
        apply: bool,
    },
    RepositoryRestoreNative {
        repository_id: RepositoryId,
        apply: bool,
    },
    RepositorySetDriveOrigin {
        repository_id: RepositoryId,
        drive: bool,
    },
    Mount {
        repository_id: RepositoryId,
        generation: GenerationId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        drive_letter: Option<String>,
    },
    Unmount {
        repository_id: RepositoryId,
    },
    Profile {
        repository_id: RepositoryId,
        maximum_duration_seconds: u32,
    },
    ProfileConfigure {
        repository_id: RepositoryId,
        launcher_relative: PathBuf,
        arguments: Vec<String>,
        version_label: String,
        configuration_label: String,
    },
    Simulate {
        repository_id: RepositoryId,
    },
    CapacityPlan {
        repository_id: RepositoryId,
        requested_bytes: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        drive_access_token: Option<SensitiveString>,
    },
    CapacityAcquire {
        repository_id: RepositoryId,
        requested_bytes: u64,
        lifetime_seconds: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        drive_access_token: Option<SensitiveString>,
    },
    CapacityStatus {
        repository_id: RepositoryId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        lease_id: Option<SpaceLeaseId>,
    },
    CapacityConsume {
        repository_id: RepositoryId,
        lease_id: SpaceLeaseId,
    },
    CapacityRelease {
        repository_id: RepositoryId,
        lease_id: SpaceLeaseId,
    },
    NativeActivate {
        repository_id: RepositoryId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        drive_access_token: Option<SensitiveString>,
    },
    NativeStatus {
        repository_id: RepositoryId,
    },
    Plan {
        repository_id: RepositoryId,
        #[serde(default)]
        full_volume: bool,
    },
    Materialize {
        repository_id: RepositoryId,
        capsule_id: CapsuleId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        drive_access_token: Option<SensitiveString>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        drive_quota: Option<DriveQuotaSnapshot>,
    },
    Admit {
        repository_id: RepositoryId,
        capsule_id: CapsuleId,
    },
    Launch {
        repository_id: RepositoryId,
        capsule_id: Option<CapsuleId>,
        maximum_duration_seconds: Option<u64>,
    },
    Verify {
        repository_id: RepositoryId,
        deep: bool,
    },
    UpdateBegin {
        repository_id: RepositoryId,
    },
    UpdateStatus {
        repository_id: RepositoryId,
    },
    UpdateCommit {
        repository_id: RepositoryId,
    },
    UpdateRollback {
        repository_id: RepositoryId,
    },
    Repair {
        repository_id: RepositoryId,
    },
    Cancel {
        repository_id: RepositoryId,
        cancellation_id: u64,
    },
}
impl Command {
    pub const fn mutates(&self) -> bool {
        matches!(
            self,
            Self::RepositoryRegister { .. }
                | Self::RepositoryAdopt { .. }
                | Self::RepositoryConvert { .. }
                | Self::RepositoryRestoreNative { .. }
                | Self::RepositorySetDriveOrigin { .. }
                | Self::Mount { .. }
                | Self::Unmount { .. }
                | Self::Profile { .. }
                | Self::ProfileConfigure { .. }
                | Self::Materialize { .. }
                | Self::Admit { .. }
                | Self::Launch { .. }
                | Self::CapacityAcquire { .. }
                | Self::CapacityConsume { .. }
                | Self::CapacityRelease { .. }
                | Self::NativeActivate { .. }
                | Self::UpdateBegin { .. }
                | Self::UpdateCommit { .. }
                | Self::UpdateRollback { .. }
                | Self::Repair { .. }
                | Self::Cancel { .. }
        )
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub protocol_version: u16,
    pub request_id: u64,
    pub cancellation_id: Option<u64>,
    pub command: Command,
}
impl Request {
    pub fn validate(&self) -> Result<(), mirage_types::MirageError> {
        if self.protocol_version != crate::PROTOCOL_VERSION {
            return Err(mirage_types::MirageError::unsupported_layout(
                "unsupported IPC protocol version",
            ));
        }
        if self.request_id == 0 {
            return Err(mirage_types::MirageError::invalid_argument(
                "IPC request ID cannot be zero",
            ));
        }
        if let Command::Materialize {
            drive_access_token,
            drive_quota,
            ..
        } = &self.command
        {
            if drive_access_token.is_some() != drive_quota.is_some() {
                return Err(mirage_types::MirageError::invalid_argument(
                    "Drive token and quota snapshot must be supplied together",
                ));
            }
            if let Some(quota) = drive_quota {
                quota.validate()?;
            }
        }
        match &self.command {
            Command::CapacityPlan {
                requested_bytes, ..
            }
            | Command::CapacityAcquire {
                requested_bytes, ..
            } if *requested_bytes == 0 || *requested_bytes > (1_u64 << 50) => {
                return Err(mirage_types::MirageError::invalid_argument(
                    "Space Lease request must be between 1 byte and 1 PiB",
                ));
            }
            Command::CapacityAcquire {
                lifetime_seconds, ..
            } if !(30..=86_400).contains(lifetime_seconds) => {
                return Err(mirage_types::MirageError::invalid_argument(
                    "Space Lease lifetime must be between 30 seconds and 24 hours",
                ));
            }
            _ => {}
        }
        Ok(())
    }
}
